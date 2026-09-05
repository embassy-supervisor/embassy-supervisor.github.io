//! Fixed per-instance tables bridging fn-pointer-only supervisor APIs to the
//! runtime-built graph.
//!
//! `NodeCfg.spawn` and `Observer` take plain `fn` pointers, so a fixed array
//! of index-capturing fns fans out to the tables here. One wasm instance runs
//! one graph (the page re-instantiates the module per run), so the tables are
//! filled exactly once by the builder.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize};

use embassy_executor::{SpawnError, Spawner};
use embassy_supervisor::{
    Backed, Budget, Claimant, Coupling, Leased, ResourceGate, ResourceSlot, TaskNode, VetoGate,
};

use crate::behavior::Behavior;
use crate::model::{MAX_NODES, MAX_SIGNALS, MAX_VETO_SLOTS, NodeModel};

/// The simulated hardware watchdog: armed by a `watchdog` behavior, fed
/// while its task runs. When feeding stops, the bite (checked per frame in
/// the snapshot) reboots the MCU: the page restarts the whole instance.
pub struct HwWatchdog {
    pub armed: AtomicBool,
    pub last_fed_us: AtomicU64,
}

pub static HW_WATCHDOG: HwWatchdog = HwWatchdog {
    armed: AtomicBool::new(false),
    last_fed_us: AtomicU64::new(0),
};

/// The gating wrapper a signal runs with: `Backed`/`Leased` are decided by
/// scenario metadata (which behavior opens or leases it), `Veto` by the DSL
/// itself (a writer carrying the `veto` marker).
pub enum Gate {
    Plain(&'static AtomicU32),
    Backed(&'static Backed<()>),
    Leased(&'static Leased<AtomicU32>),
    Veto(&'static VetoGate<MAX_VETO_SLOTS>),
}

pub struct SignalRt {
    pub name: &'static str,
    /// Canonical coupling for `Sig.entry`; wraps the same point as every
    /// node-table entry for this signal (`same_signal` matches by point).
    pub coupling: &'static Coupling,
    pub gate: Gate,
    pub writes: AtomicU32,
    pub reads: AtomicU32,
    /// Units of `writes` claimed by competing pool consumers (a report is
    /// published by exactly one member).
    pub claims: AtomicU32,
    /// Last written value (f32 bits) for the UI.
    pub value: AtomicU32,
    /// Queue depth, when a `queue` behavior stages this signal.
    pub depth: AtomicU32,
    /// Set once by the builder when some behavior maintains `depth`.
    pub depth_active: AtomicBool,
}

/// How a resource is taken, resolved from the strongest marker any declaring
/// node carries (take-kind names are globally unique in the real DSL).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResKind {
    /// Taken at body start, restored at exit: a respawn re-takes the same
    /// instance (embassy's `Peri`-by-`&mut` shape).
    Lend,
    /// Taken at body start, gone at exit: a respawn fail-closes until
    /// something re-provides (a `Runner` moved into a `-> !` task).
    Consume,
    /// Never taken: one slot, many holders, stays filled (`embassy_net::Stack`,
    /// which is literally `Copy`).
    Shared,
    /// A `Budget`: never taken either; every holder claims a share through
    /// its `Claimant`, and the supervisor releases the share when the
    /// holder stops.
    Divisible,
}

impl ResKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ResKind::Lend => "lend",
            ResKind::Consume => "consume",
            ResKind::Shared => "shared",
            ResKind::Divisible => "divisible",
        }
    }
}

/// What a resource name is backed by: a slot for the take kinds, a budget
/// sized to the node cap (slot `i` = node `i`) for `divisible`.
#[derive(Clone, Copy)]
pub enum ResObj {
    Slot(&'static ResourceSlot<u32>),
    Budget(&'static Budget<MAX_NODES>),
}

impl ResObj {
    pub fn gate(self) -> &'static dyn ResourceGate {
        match self {
            ResObj::Slot(s) => s,
            ResObj::Budget(b) => b,
        }
    }

    pub fn is_filled(self) -> bool {
        self.gate().is_filled()
    }

    /// Fill the slot with a unit value, or set a budget's capacity.
    pub fn provide(self, v: u32) {
        match self {
            ResObj::Slot(s) => s.provide(v),
            ResObj::Budget(b) => b.provide(v),
        }
    }

    pub fn clear(self) {
        self.gate().clear();
    }

    /// Take a slot's value; a budget has nothing to take.
    pub fn take(self) -> Option<u32> {
        match self {
            ResObj::Slot(s) => s.take(),
            ResObj::Budget(_) => None,
        }
    }

    pub fn budget(self) -> Option<&'static Budget<MAX_NODES>> {
        match self {
            ResObj::Slot(_) => None,
            ResObj::Budget(b) => Some(b),
        }
    }
}

/// `held_by` sentinel: nobody holds the slot's value.
pub const HELD_BY_NONE: usize = usize::MAX;

pub struct ResourceRt {
    pub name: &'static str,
    pub slot: ResObj,
    pub kind: ResKind,
    /// Node index currently holding a lent value ([`HELD_BY_NONE`] when none).
    pub held_by: AtomicUsize,
    /// A budget's last offered capacity (the boot fill, then whatever the
    /// allocator provides), so a hand `provide` restores that instead of a
    /// unit. The allocator overrides it again on its next wake.
    pub capacity: AtomicU32,
}

pub struct NodeRt {
    /// This node's index in the registry (== its graph slot).
    pub idx: usize,
    pub node: &'static TaskNode,
    pub model: &'static NodeModel,
    pub behavior: Behavior,
    pub reads: Vec<&'static SignalRt>,
    /// Last write-counter value this node consumed, per `reads` entry:
    /// a read only counts when the writer has produced something new.
    pub read_marks: Vec<AtomicU32>,
    pub writes: Vec<&'static SignalRt>,
    pub provides: Vec<&'static ResourceRt>,
    pub consumes: Vec<&'static ResourceRt>,
    pub lends: Vec<&'static ResourceRt>,
    /// One claimant per `divisible` resource this node holds (slot = `idx`).
    pub claims: Vec<(&'static ResourceRt, Claimant)>,
    /// Widget-driven input (f32 bits): sensor value, link up/down, load dial.
    pub input: AtomicU32,
    /// Index into [`pools`] of the pool this node is a member of.
    pub pool: Option<usize>,
    /// A `session` member's live client session, visible to its pool
    /// siblings so sessions are dealt out across the members that are
    /// running rather than by member number. Cleared on every stop path.
    pub session_open: AtomicBool,
    /// Wall-clock poll ledger, stamped by `trace_hooks` (browser time).
    pub last_poll_us: AtomicU32,
    pub max_poll_us: AtomicU32,
    pub exec_us: AtomicU64,
    /// The trace executor id this node last polled on.
    pub exec_id: AtomicU32,
}

pub struct PoolRt {
    pub name: &'static str,
    pub members: &'static [&'static TaskNode],
    pub min: u8,
    pub max: u8,
}

static NODES: OnceLock<Vec<&'static NodeRt>> = OnceLock::new();
static SIGNALS: OnceLock<Vec<&'static SignalRt>> = OnceLock::new();
static RESOURCES: OnceLock<Vec<&'static ResourceRt>> = OnceLock::new();
static POOLS: OnceLock<Vec<PoolRt>> = OnceLock::new();

pub fn install(
    nodes: Vec<&'static NodeRt>,
    signals: Vec<&'static SignalRt>,
    resources: Vec<&'static ResourceRt>,
    pools: Vec<PoolRt>,
) {
    NODES.set(nodes).ok().expect("registry installed twice");
    SIGNALS.set(signals).ok().expect("registry installed twice");
    RESOURCES
        .set(resources)
        .ok()
        .expect("registry installed twice");
    POOLS.set(pools).ok().expect("registry installed twice");
}

pub fn pools() -> &'static [PoolRt] {
    POOLS.get().map(Vec::as_slice).unwrap_or(&[])
}

pub fn nodes() -> &'static [&'static NodeRt] {
    NODES.get().map(Vec::as_slice).unwrap_or(&[])
}

pub fn signals() -> &'static [&'static SignalRt] {
    SIGNALS.get().map(Vec::as_slice).unwrap_or(&[])
}

pub fn resources() -> &'static [&'static ResourceRt] {
    RESOURCES.get().map(Vec::as_slice).unwrap_or(&[])
}

pub fn node_rt(i: usize) -> &'static NodeRt {
    nodes()[i]
}

#[embassy_executor::task(pool_size = 48)]
async fn worker(slot: usize) {
    crate::behavior::run(node_rt(slot)).await;
}

macro_rules! fn_tables {
    ($($i:literal)*) => {
        /// One spawn fn per node slot, coercible to `NodeCfg`'s fn pointer.
        pub static SPAWN_FNS: [fn(Spawner) -> Result<(), SpawnError>; MAX_NODES] = [
            $(|sp: Spawner| { sp.spawn(worker($i)?); Ok(()) }),*
        ];
    };
}
fn_tables!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23
           24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47);

fn observed_writes(j: usize) -> u32 {
    signals()
        .get(j)
        .map(|s| s.writes.load(std::sync::atomic::Ordering::Relaxed))
        .unwrap_or(0)
}

macro_rules! obs_tables {
    ($($j:literal)*) => {
        /// One observer fn per signal slot, for `Observer::new`.
        pub static OBS_FNS: [fn() -> u32; MAX_SIGNALS] = [
            $(|| observed_writes($j)),*
        ];
    };
}
obs_tables!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23
            24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47
            48 49 50 51 52 53 54 55 56 57 58 59 60 61 62 63);

// The task macro's pool size must match the node cap; keep them locked.
const _: () = assert!(
    MAX_NODES == 48,
    "worker pool_size literal must match MAX_NODES"
);
