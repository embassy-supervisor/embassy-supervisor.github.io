//! The playground's graph model: what `parse` hands to both the UI (as JSON)
//! and the runtime builder (as Rust data).
//!
//! Field extraction comes from `supervisor-tools`' typed `full_model`
//! accessor; this module keeps exactly the fields the interpreter honors,
//! plus badges for everything it degrades.

use serde::Serialize;

/// Hard caps for one playground run. `MAX_NODES` bounds the embassy task
/// pool; `MAX_SIGNALS` bounds the observer fn table.
pub const MAX_NODES: usize = 48;
pub const MAX_SIGNALS: usize = 64;
/// Contributor bits per veto gate: `VetoGate<N>` caps `N` at a `u32`'s width.
pub const MAX_VETO_SLOTS: usize = 32;

#[derive(Serialize, Clone)]
pub struct ParseError {
    pub line: usize,
    pub msg: String,
}

/// A clause the interpreter parses but does not execute (or reinterprets).
#[derive(Serialize, Clone)]
pub struct Badge {
    /// The node/pool/graph the badge is attached to.
    pub item: String,
    pub clause: String,
    pub note: String,
}

#[derive(Serialize, Clone)]
pub struct DepModel {
    pub name: String,
    pub ready: bool,
    pub bound: bool,
}

#[derive(Serialize, Clone)]
pub struct ResourceModel {
    pub name: String,
    pub local: bool,
    pub consume: bool,
    pub shared: bool,
    /// `divisible`: a budget the holder claims a share of, not a slot.
    pub divisible: bool,
    /// `serialized`: every holder runs on one executor (macro-checked).
    pub serialized: bool,
}

#[derive(Serialize, Clone)]
pub struct SignalRef {
    /// Canonical signal path text, e.g. `signals::RAW_SAMPLES`.
    pub name: String,
    pub observed: bool,
    pub beat: bool,
    /// `veto` on a write: this writer holds a contributor bit of the gate.
    pub veto: bool,
}

#[derive(Serialize, Clone)]
pub struct NodeModel {
    pub name: String,
    /// `terminate` | `pause` | `ondemand`
    pub mode: String,
    pub deps: Vec<DepModel>,
    /// The `task:`/`spawn:` path text; doubles as the behavior id.
    pub task: Option<String>,
    pub resources: Vec<ResourceModel>,
    pub provides: Vec<String>,
    pub disabled: bool,
    pub executor: Option<String>,
    pub executor_defaulted: bool,
    pub slot_timeout_ms: Option<u64>,
    pub ack_timeout_ms: Option<u64>,
    pub beat_timeout_ms: Option<u64>,
    pub beat_window: Option<u8>,
    pub ready_on_write: bool,
    pub reads: Vec<SignalRef>,
    pub writes: Vec<SignalRef>,
    /// Set when this node is a pool member: the pool's name.
    pub pool: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct PoolModel {
    pub name: String,
    /// Member modes as written; the pool has `modes.len()` members.
    pub modes: Vec<String>,
    /// Member node names (generated: `NAME#0`, `NAME#1`, ...).
    pub members: Vec<String>,
    pub min: u8,
    pub max: u8,
    /// Shrink cooldown in ms recovered from the `policy:` expression.
    pub cooldown_ms: u64,
}

#[derive(Serialize, Clone, Default)]
pub struct SignalModel {
    pub name: String,
    pub writers: Vec<String>,
    pub readers: Vec<String>,
    pub observed: bool,
    pub beat: bool,
    /// Some writer carries `veto`: the signal runs as a `VetoGate`.
    pub veto: bool,
    /// The writers carrying `veto`, in declaration order: their contributor
    /// bits are numbered by this order, as the macro numbers them.
    pub veto_writers: Vec<String>,
}

#[derive(Serialize, Clone)]
pub struct GraphModel {
    pub name: Option<String>,
    /// Every runnable node, pool members included, in declaration order.
    pub nodes: Vec<NodeModel>,
    pub pools: Vec<PoolModel>,
    pub executors: Vec<String>,
    pub signals: Vec<SignalModel>,
    /// Node indices (into `nodes`) in dependency order — our own Kahn sort.
    pub order: Vec<u8>,
}

#[derive(Serialize)]
pub struct ParseOutcome {
    pub ok: bool,
    pub errors: Vec<ParseError>,
    pub lints: Vec<String>,
    pub badges: Vec<Badge>,
    pub model: Option<GraphModel>,
}
