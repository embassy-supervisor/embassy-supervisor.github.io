//! Log capture and per-frame snapshot assembly.
//!
//! Everything the UI shows is pulled, once per animation frame, from the
//! statics here: a capturing `log::Log` backend (the supervisor's bring-up
//! lines, stale reports and fault text all arrive through the `log` facade on
//! wasm) plus drained health events and supervisor faults.

use std::sync::Mutex;
use std::sync::atomic::Ordering;

use embassy_supervisor::{Fault, NodeFault, trace};
use serde::Serialize;

use crate::registry::{self, Gate, HELD_BY_NONE, NodeRt};
use crate::trace_hooks;

#[derive(Serialize)]
pub struct LogEntry {
    /// Virtual timestamp in microseconds (mock-driver time, starts at zero).
    pub ts_us: u64,
    pub level: &'static str,
    pub target: String,
    pub msg: String,
}

#[derive(Serialize)]
pub struct NodeSnap {
    pub idx: u8,
    pub name: &'static str,
    pub mode: &'static str,
    pub running: bool,
    pub busy: bool,
    pub disabled: bool,
    /// Held stopped as a dependent of a deactivated node (0.7.0's
    /// `collateral` hold): released by `activate` on the ancestor.
    pub collateral: bool,
    pub ready: bool,
    pub bound_stopped: bool,
    pub exited: bool,
    pub detached: bool,
    pub epoch: u32,
    pub status: Option<&'static str>,
    pub ticks_since_beat: u32,
    /// This node's share of its first `divisible` resource, when it holds one.
    pub grant: Option<u32>,
    pub want: Option<u32>,
    /// Set if the node declares `beat_timeout:`.
    pub policed: bool,
    /// Node's executor, or `None` for the root executor.
    pub executor: Option<&'static str>,
    pub executor_defaulted: bool,
    /// The trace executor id this node last polled on (0 = never polled).
    pub exec_id: u32,
    /// Genuine counts from the trace recorders.
    pub polls: u32,
    /// Wall-clock poll durations from our own hooks — browser time, not MCU
    /// microseconds.
    pub last_poll_us: u32,
    pub max_poll_us: u32,
    pub exec_us: u64,
    /// Active fault, if any. Crashes clear when the shell drops; stalls
    /// survive stops and restarts.
    pub fault: Option<&'static str>,
}

#[derive(Serialize)]
pub struct HealthSnap {
    pub node: &'static str,
    pub kind: String,
    /// What the app-owned escalation policy did about it.
    pub action: String,
}

/// One row per trace-registered executor: genuine counts, wall-clock exec
/// time (browser time), and who is on it right now.
#[derive(Serialize)]
pub struct ExecutorSnap {
    pub id: u32,
    /// Resolved name: `root`, or the declared `executor NAME`.
    pub name: &'static str,
    pub polls: u32,
    pub passes: u32,
    pub exec_us: u64,
    pub current: Option<&'static str>,
}

#[derive(Serialize)]
pub struct SignalSnap {
    pub name: &'static str,
    /// `plain` | `backed` | `leased` | `veto`
    pub kind: &'static str,
    pub writes: u32,
    pub reads: u32,
    pub value: f32,
    /// Staged backlog, when a `queue` behavior maintains one on this signal.
    pub depth: Option<u32>,
    pub leases: Option<u32>,
    pub drained: Option<bool>,
    /// Live `Open` guards on a backed signal.
    pub openers: Option<u32>,
    /// A veto gate's state and how many contributor bits are up.
    pub asserted: Option<bool>,
    pub contributors: Option<u32>,
}

#[derive(Serialize)]
pub struct PoolSnap {
    pub name: &'static str,
    pub members: Vec<&'static str>,
    pub running: u8,
    pub busy: u8,
    pub min: u8,
    pub max: u8,
}

#[derive(Serialize)]
pub struct ResourceSnap {
    pub name: &'static str,
    pub filled: bool,
    /// lend | consume | shared | divisible
    pub kind: &'static str,
    /// The node holding a lent value, while it is out.
    pub held_by: Option<&'static str>,
    /// A budget's provided capacity, the units granted out of it, and the
    /// holders currently stating a want.
    pub capacity: Option<u32>,
    pub granted: Option<u32>,
    pub claimants: Option<u32>,
}

#[derive(Serialize)]
pub struct Snapshot {
    pub now_us: u64,
    /// The simulated hardware watchdog went unfed too long this frame; the
    /// page reacts by rebooting the MCU (restarting the instance).
    pub watchdog_bite: bool,
    pub logs: Vec<LogEntry>,
    pub nodes: Vec<NodeSnap>,
    pub signals: Vec<SignalSnap>,
    pub pools: Vec<PoolSnap>,
    pub resources: Vec<ResourceSnap>,
    pub executors: Vec<ExecutorSnap>,
    pub health: Vec<HealthSnap>,
    pub faults: Vec<String>,
}

static LOGS: Mutex<Vec<LogEntry>> = Mutex::new(Vec::new());
static FAULTS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static HEALTH: Mutex<Vec<HealthSnap>> = Mutex::new(Vec::new());

struct Capture;
static CAPTURE: Capture = Capture;

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        let mut logs = LOGS.lock().unwrap();
        // A bounded buffer: if JS stops draining, drop the oldest half.
        if logs.len() > 4096 {
            logs.drain(..2048);
        }
        logs.push(LogEntry {
            ts_us: embassy_time::Instant::now().as_micros(),
            level: record.level().as_str(),
            target: record.target().to_string(),
            msg: record.args().to_string(),
        });
    }

    fn flush(&self) {}
}

/// Install the capture backend. Idempotent per wasm instance.
pub fn install_logger() {
    if log::set_logger(&CAPTURE).is_ok() {
        log::set_max_level(log::LevelFilter::Trace);
    }
}

pub fn push_fault(fault: &NodeFault) {
    FAULTS.lock().unwrap().push(format!("{fault}"));
}

/// Record a health event and what the escalation policy did with it. Fed by
/// the `health` driver task, not drained here: the application owns the
/// policy, the snapshot only reports it.
pub fn push_health(node: &'static str, kind: String, action: String) {
    HEALTH
        .lock()
        .unwrap()
        .push(HealthSnap { node, kind, action });
}

pub fn node_snap(idx: u8, rt: &'static NodeRt) -> NodeSnap {
    let node = rt.node;
    NodeSnap {
        idx,
        name: node.name(),
        mode: node.mode().as_str(),
        running: node.is_running(),
        busy: node.is_busy(),
        disabled: node.is_disabled(),
        collateral: node.is_collateral(),
        ready: node.is_ready(),
        bound_stopped: node.is_bound_stopped(),
        exited: node.has_exited(),
        detached: node.is_detached(),
        epoch: node.epoch(),
        status: node.status(),
        ticks_since_beat: node.ticks_since_beat(),
        grant: rt.claims.first().map(|(_, c)| c.grant()),
        want: rt
            .claims
            .first()
            .map(|(r, c)| r.slot.budget().map_or(0, |b| b.want_of(c.slot()))),
        policed: rt.model.beat_timeout_ms.is_some(),
        executor: rt.model.executor.as_deref(),
        executor_defaulted: rt.model.executor_defaulted,
        exec_id: rt.exec_id.load(Ordering::Relaxed),
        polls: node.poll_count(),
        last_poll_us: rt.last_poll_us.load(Ordering::Relaxed),
        max_poll_us: rt.max_poll_us.load(Ordering::Relaxed),
        exec_us: rt.exec_us.load(Ordering::Relaxed),
        fault: match node.fault() {
            Fault::None => None,
            f => Some(f.as_str()),
        },
    }
}

/// Assemble the per-frame snapshot from the registry: drained logs + node,
/// signal, pool and resource states + drained health events and faults.
pub fn snapshot() -> Snapshot {
    let health = std::mem::take(&mut *HEALTH.lock().unwrap());
    let signals = registry::signals()
        .iter()
        .map(|s| {
            let (kind, leases, drained) = match &s.gate {
                Gate::Plain(_) => ("plain", None, None),
                Gate::Backed(_) => ("backed", None, None),
                Gate::Leased(l) => ("leased", Some(l.leases()), Some(l.is_drained())),
                Gate::Veto(_) => ("veto", None, None),
            };
            let openers = match &s.gate {
                Gate::Backed(b) => Some(b.openers()),
                _ => None,
            };
            let (asserted, contributors) = match &s.gate {
                Gate::Veto(g) => (Some(g.is_asserted()), Some(g.contributors().count_ones())),
                _ => (None, None),
            };
            SignalSnap {
                name: s.name,
                kind,
                writes: s.writes.load(Ordering::Relaxed),
                reads: s.reads.load(Ordering::Relaxed),
                value: f32::from_bits(s.value.load(Ordering::Relaxed)),
                depth: s
                    .depth_active
                    .load(Ordering::Relaxed)
                    .then(|| s.depth.load(Ordering::Relaxed)),
                leases,
                drained,
                openers,
                asserted,
                contributors,
            }
        })
        .collect();
    let pools = registry::pools()
        .iter()
        .map(|p| PoolSnap {
            name: p.name,
            members: p.members.iter().map(|n| n.name()).collect(),
            running: p.members.iter().filter(|n| n.is_running()).count() as u8,
            // Busy only counts running members: the flag itself is cleared
            // on stop by the worker, but a parked member may keep it.
            busy: p
                .members
                .iter()
                .filter(|n| n.is_running() && n.is_busy())
                .count() as u8,
            min: p.min,
            max: p.max,
        })
        .collect();
    // A 3 s (virtual) unfed window bites once, then disarms: the reboot is
    // the page's job, and one bite must not fire every following frame.
    let now_us = embassy_time::Instant::now().as_micros();
    let wd = &registry::HW_WATCHDOG;
    let watchdog_bite = wd.armed.load(Ordering::Relaxed)
        && now_us.saturating_sub(wd.last_fed_us.load(Ordering::Relaxed)) > 3_000_000
        && {
            wd.armed.store(false, Ordering::Relaxed);
            true
        };
    let resources = registry::resources()
        .iter()
        .map(|r| {
            let held = r.held_by.load(Ordering::Relaxed);
            let budget = r.slot.budget();
            ResourceSnap {
                name: r.name,
                filled: r.slot.is_filled(),
                kind: r.kind.as_str(),
                held_by: (held != HELD_BY_NONE)
                    .then(|| registry::nodes().get(held).map(|n| n.node.name()))
                    .flatten(),
                capacity: budget.map(|b| b.capacity()),
                granted: budget.map(|b| b.total_granted()),
                claimants: budget.map(|b| {
                    (0..b.slots() as u8)
                        .filter(|&slot| b.want_of(slot) > 0)
                        .count() as u32
                }),
            }
        })
        .collect();
    // Executor rows: trace's registered ids, named by the nodes seen polling
    // on them (the supervisor's own executor is `root`).
    let executors = trace::executors()
        .into_iter()
        .filter(|&id| id != 0)
        .map(|id| {
            let stats = trace::executor_stats(id).unwrap_or_default();
            // On the wasm platform every executor polls on the one main
            // thread and the recorders see a single merged id; label it
            // honestly instead of crediting one tier with everyone's polls.
            // (On the std platform — the native guards — ids are per thread
            // and each tier gets its own row.)
            let mut names: Vec<&'static str> = registry::nodes()
                .iter()
                .filter(|rt| rt.exec_id.load(Ordering::Relaxed) == id)
                .map(|rt| rt.model.executor.as_deref().unwrap_or("root"))
                .collect();
            names.sort_unstable();
            names.dedup();
            let name = match names.len() {
                0 => "root",
                1 => names[0],
                _ => "all executors (one thread)",
            };
            let exec_us = trace_hooks::EXEC_WALL
                .iter()
                .find(|w| w.id.load(Ordering::Relaxed) == id)
                .map(|w| w.exec_us.load(Ordering::Relaxed))
                .unwrap_or(0);
            ExecutorSnap {
                id,
                name,
                polls: stats.polls,
                passes: stats.passes,
                exec_us,
                current: trace::current_task(id).map(|(n, _)| n.name()),
            }
        })
        .collect();
    Snapshot {
        now_us,
        watchdog_bite,
        logs: std::mem::take(&mut *LOGS.lock().unwrap()),
        nodes: registry::nodes()
            .iter()
            .enumerate()
            .map(|(i, rt)| node_snap(i as u8, rt))
            .collect(),
        signals,
        pools,
        resources,
        executors,
        health,
        faults: std::mem::take(&mut *FAULTS.lock().unwrap()),
    }
}
