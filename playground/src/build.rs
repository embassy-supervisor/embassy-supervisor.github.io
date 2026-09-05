//! Runtime graph builder: everything `supervisor_graph!` emits as statics,
//! reconstructed from a [`GraphModel`] with `Box::leak` and the public
//! `const fn` constructor surface.
//!
//! One wasm instance builds one graph (the page re-instantiates the module
//! per run), so the process-wide statics here are filled exactly once.

use std::cell::UnsafeCell;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize};

use embassy_supervisor::{
    Backed, Budget, Claimant, Coupling, CouplingPoint, DeferredShrink, Divisible, ElasticPool,
    Graph, GraphRef, Leased, Mode, NodeCfg, Observer, Pool, ResourceGate, ResourceSlot,
    SpawnerSlot, Supervisor, TaskNode, Topology, VetoGate, shape,
};
use embassy_time::Duration;

use crate::behavior::{Behavior, BehaviorSpec, infer, required_gate};
use crate::model::{Badge, GraphModel, MAX_NODES, NodeModel};
use crate::registry::{self, Gate, HELD_BY_NONE, NodeRt, ResKind, ResObj, ResourceRt, SignalRt};

#[derive(Clone, Copy, PartialEq)]
pub enum GateKind {
    Plain,
    Backed,
    Leased,
}

/// The one node-slot array every `cfg.graph`/`Graph.nodes` reference points
/// at. `GraphRef::new` needs the complete `&'static` slice before the nodes
/// exist (the macro solves this with cyclic statics), so the array lives in
/// an `UnsafeCell` and is written during the single-threaded build.
struct SlotArray(UnsafeCell<[Option<&'static TaskNode>; MAX_NODES]>);
// SAFETY: written only inside `build()` (single-threaded, before the
// executor starts); every later access is a read. References derived from an
// `UnsafeCell` interior legally observe those writes.
unsafe impl Sync for SlotArray {}
static GRAPH_SLOTS: SlotArray = SlotArray(UnsafeCell::new([None; MAX_NODES]));

/// Our own [`Topology`]: leaked dep rows plus the Kahn order computed at
/// parse time (never `Ordered::new`, whose cycle check is a const panic).
/// `shape::ALL` only forgoes dead-code elision; unset bits would be promises.
pub struct PlaygroundTopo {
    rows: [&'static [u8]; MAX_NODES],
    order: [u8; MAX_NODES],
}

impl Topology<MAX_NODES> for PlaygroundTopo {
    const SHAPE: u32 = shape::ALL;

    fn deps_of(&self, i: u8) -> &'static [u8] {
        self.rows[i as usize]
    }

    fn order_at(&self, k: usize) -> u8 {
        self.order[k]
    }
}

pub type PgGraph = Graph<MAX_NODES, PlaygroundTopo>;
pub type PgSupervisor = Supervisor<MAX_NODES, PlaygroundTopo>;

pub struct Built {
    pub graph: &'static PgGraph,
    pub sup: &'static PgSupervisor,
    /// Named executors the caller must start and wire (`SpawnerSlot::set`).
    pub named_executors: Vec<(String, &'static SpawnerSlot)>,
    /// Sharp edges found only once behaviors are known (appended to the
    /// parse outcome by `start_run`).
    pub badges: Vec<Badge>,
    /// Per-node health escalation policy (`supervise` applies it).
    pub escalations: BTreeMap<String, Escalation>,
}

/// What the app-owned health policy does when a node goes stale. The monitor
/// is report-only by design; this is the application acting on the report.
#[derive(Clone, PartialEq)]
pub enum Escalation {
    /// Log it, change nothing (the default).
    Report,
    /// Withdraw the node's readiness: `ready bound` dependents stop — the
    /// safe state, without restarting a wedged loop mid-flight.
    ClearReady,
    /// Queue a restart through the control mailbox.
    Restart,
    /// Queue a deactivate.
    Deactivate,
    /// Queue an activate of another (usually `disabled`) node by name.
    Activate(String),
}

impl Escalation {
    fn parse(s: &str) -> Result<Self, String> {
        Ok(match s {
            "report" => Escalation::Report,
            "clear_ready" => Escalation::ClearReady,
            "restart" => Escalation::Restart,
            "deactivate" => Escalation::Deactivate,
            other => match other.strip_prefix("activate:") {
                Some(name) => Escalation::Activate(name.to_string()),
                None => return Err(format!("unknown escalation `{other}`")),
            },
        })
    }
}

/// The behaviors JSON: either a plain `{node: spec}` map, or a wrapper
/// carrying the escalation policy beside it.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BehaviorsFile {
    #[serde(default)]
    nodes: BTreeMap<String, BehaviorSpec>,
    #[serde(default)]
    escalation: BTreeMap<String, String>,
}

static BUILT: OnceLock<Built> = OnceLock::new();

pub fn built() -> Option<&'static Built> {
    BUILT.get()
}

fn leak<T>(v: T) -> &'static T {
    Box::leak(Box::new(v))
}

fn leak_str(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

fn point_dyn(gate: &Gate) -> &'static dyn CouplingPoint {
    match gate {
        Gate::Plain(a) => *a,
        Gate::Backed(b) => *b,
        Gate::Leased(l) => *l,
        Gate::Veto(g) => *g,
    }
}

/// What an unprovided `divisible` budget is filled with at boot, the way
/// `main` would hand over a fixed limit: an allocator behavior with
/// `provides:` replaces this with its own total.
pub const BOOT_CAPACITY: u32 = 100;

/// Build the whole runtime graph. Call once per process/wasm instance.
pub fn build(model: GraphModel, behaviors_json: &str) -> Result<&'static Built, String> {
    if BUILT.get().is_some() {
        return Err("this instance already built a graph; reset re-instantiates the module".into());
    }
    let (specs, escalation_raw): (BTreeMap<String, BehaviorSpec>, BTreeMap<String, String>) =
        if behaviors_json.trim().is_empty() {
            (BTreeMap::new(), BTreeMap::new())
        } else if let Ok(f) = serde_json::from_str::<BehaviorsFile>(behaviors_json) {
            (f.nodes, f.escalation)
        } else {
            (
                serde_json::from_str(behaviors_json).map_err(|e| format!("behaviors: {e}"))?,
                BTreeMap::new(),
            )
        };
    let mut escalations = BTreeMap::new();
    for (node, esc) in &escalation_raw {
        escalations.insert(node.clone(), Escalation::parse(esc)?);
    }
    let mut badges: Vec<Badge> = Vec::new();
    let model: &'static GraphModel = leak(model);

    // SAFETY: see `SlotArray` — the only writes happen below, before anything
    // reads through this reference.
    let slots: &'static [Option<&'static TaskNode>; MAX_NODES] = unsafe { &*GRAPH_SLOTS.0.get() };
    let graph_ref: &'static GraphRef = leak(GraphRef::new(slots));

    // Named executors.
    let mut executor_slots: BTreeMap<&str, &'static SpawnerSlot> = BTreeMap::new();
    let mut named_executors = Vec::new();
    for name in &model.executors {
        let slot: &'static SpawnerSlot = leak(SpawnerSlot::new());
        executor_slots.insert(name.as_str(), slot);
        named_executors.push((name.clone(), slot));
    }

    // Behavior spec per node (scenario metadata by node name, then by task
    // path, else inferred from the graph shape).
    let node_specs: Vec<BehaviorSpec> = model
        .nodes
        .iter()
        .map(|n| {
            specs
                .get(&n.name)
                // A pool's members answer to the pool's name...
                .or_else(|| n.pool.as_ref().and_then(|p| specs.get(p)))
                // ...and any node to its task path text.
                .or_else(|| n.task.as_ref().and_then(|t| specs.get(t)))
                .cloned()
                .unwrap_or_else(|| infer(n))
        })
        .collect();

    // Gate kind per signal: Plain unless some behavior opens/leases it.
    let mut gates: BTreeMap<&str, GateKind> = BTreeMap::new();
    for spec in &node_specs {
        if let Some((name, kind)) = required_gate(spec) {
            let sig = model
                .signals
                .iter()
                .find(|s| s.name == name)
                .ok_or_else(|| {
                    format!(
                        "behavior references unknown signal `{name}` (declare it in reads:/writes:)"
                    )
                })?;
            let prev = gates.insert(leak_str(&sig.name), kind);
            if prev.is_some_and(|p| p != kind) {
                return Err(format!("signal `{name}` is both opened and leased"));
            }
        }
    }
    // A sharp edge worth flagging: `Backed` demand-starts its producer with
    // an `Activate`, which re-enables an `OnDemand` node but never spawns it
    // — the natural-looking mode for a demand-started producer is the one
    // that does not work.
    for (sig_name, kind) in &gates {
        if *kind != GateKind::Backed {
            continue;
        }
        if let Some(sig) = model.signals.iter().find(|s| s.name == *sig_name) {
            for w in &sig.writers {
                if model
                    .nodes
                    .iter()
                    .any(|n| &n.name == w && n.mode == "ondemand")
                {
                    badges.push(Badge {
                        item: w.clone(),
                        clause: "OnDemand".into(),
                        note: format!(
                            "`{sig_name}` is opened through a Backed gate, but its producer is OnDemand: the gate's Activate re-enables it without spawning it. Use Terminate + disabled for a demand-started producer"
                        ),
                    });
                }
            }
        }
    }

    // Resources.
    let provided: Vec<&str> = model
        .nodes
        .iter()
        .flat_map(|n| n.provides.iter().map(String::as_str))
        .collect();
    // Take kind per name: consume wins over shared wins over lend (take-kind
    // names are globally unique in the real DSL; only `shared` and
    // `divisible` repeat, and parse rejects a name mixing the two worlds).
    let mut kinds: BTreeMap<&str, ResKind> = BTreeMap::new();
    for n in &model.nodes {
        for r in &n.resources {
            let k = if r.divisible {
                ResKind::Divisible
            } else if r.consume {
                ResKind::Consume
            } else if r.shared {
                ResKind::Shared
            } else {
                ResKind::Lend
            };
            let e = kinds.entry(leak_str(&r.name)).or_insert(k);
            if matches!(k, ResKind::Consume) {
                *e = k;
            }
        }
    }
    let mut resource_map: BTreeMap<&str, &'static ResourceRt> = BTreeMap::new();
    let mut resources = Vec::new();
    for n in &model.nodes {
        for r in n
            .resources
            .iter()
            .map(|r| r.name.as_str())
            .chain(n.provides.iter().map(String::as_str))
        {
            if resource_map.contains_key(r) {
                continue;
            }
            let kind = kinds.get(r).copied().unwrap_or(ResKind::Lend);
            let slot = match kind {
                // One budget per name, sized to the node cap so slot `i` is
                // node `i` (the real macro sizes it to the declaring
                // holders; the numbering is what matters).
                ResKind::Divisible => ResObj::Budget(leak(Budget::new())),
                _ => ResObj::Slot(leak(ResourceSlot::new())),
            };
            let boot = match kind {
                ResKind::Divisible => BOOT_CAPACITY,
                _ => 1,
            };
            if !provided.contains(&r) {
                // Nobody provides it at runtime: fill once at boot, the way
                // main hands over what it owns. Never refilled — a `consume`
                // taker leaves it empty for good until something re-provides.
                slot.provide(boot);
            }
            let rt = leak(ResourceRt {
                name: leak_str(r),
                slot,
                kind,
                held_by: AtomicUsize::new(HELD_BY_NONE),
                capacity: AtomicU32::new(boot),
            });
            resource_map.insert(rt.name, rt);
            resources.push(rt);
        }
    }

    // Signals: one gate object (the coupling point) + one canonical coupling.
    let mut signals: Vec<&'static SignalRt> = Vec::new();
    for (j, s) in model.signals.iter().enumerate() {
        let name = leak_str(&s.name);
        let kind = gates.get(s.name.as_str()).copied();
        if s.veto && kind.is_some() {
            return Err(format!(
                "signal `{}` carries `veto` but a behavior also opens or leases it: a veto gate is neither Backed nor Leased",
                s.name
            ));
        }
        let gate = if s.veto {
            Gate::Veto(leak(VetoGate::new()))
        } else {
            match kind.unwrap_or(GateKind::Plain) {
                GateKind::Plain => Gate::Plain(leak(AtomicU32::new(0))),
                GateKind::Backed => Gate::Backed(leak(Backed::new(()))),
                GateKind::Leased => Gate::Leased(leak(Leased::new(AtomicU32::new(0)))),
            }
        };
        let mut coupling = Coupling::new(name, point_dyn(&gate));
        if s.observed {
            coupling = coupling.observed(Observer::new(registry::OBS_FNS[j]));
        }
        if s.beat {
            coupling = coupling.beat();
        }
        signals.push(leak(SignalRt {
            name,
            coupling: leak(coupling),
            gate,
            writes: AtomicU32::new(0),
            reads: AtomicU32::new(0),
            claims: AtomicU32::new(0),
            value: AtomicU32::new(0),
            depth: AtomicU32::new(0),
            depth_active: AtomicBool::new(false),
        }));
    }
    let sig_index = |name: &str| -> usize { signals.iter().position(|s| s.name == name).unwrap() };

    // Nodes, constructed in Kahn order so ready/bound dep targets exist.
    let mut node_rts: Vec<Option<&'static NodeRt>> = vec![None; model.nodes.len()];
    let mut dep_rows: [&'static [u8]; MAX_NODES] = [&[]; MAX_NODES];
    for &oi in &model.order {
        let i = oi as usize;
        let m: &'static NodeModel = &model.nodes[i];
        let mode = match m.mode.as_str() {
            "pause" => Mode::Pause,
            "ondemand" => Mode::OnDemand,
            _ => Mode::Terminate,
        };
        let spawn = m.task.as_ref().map(|_| registry::SPAWN_FNS[i]);
        let mut cfg = NodeCfg::new(leak_str(&m.name), mode, spawn).with_graph(graph_ref);
        // Task nodes use the injected shell so stall/crash apply; parked
        // nodes only wedge, like a spawned task.
        if spawn.is_some() {
            cfg = cfg.with_shell();
        }

        if let Some(ms) = m.slot_timeout_ms {
            cfg = cfg.with_slot_timeout(Duration::from_millis(ms));
        }
        if let Some(ms) = m.ack_timeout_ms {
            cfg = cfg.with_ack_timeout(Duration::from_millis(ms));
        }
        if let Some(ms) = m.beat_timeout_ms {
            cfg = cfg.with_beat_timeout(Duration::from_millis(ms));
        }
        if let Some(w) = m.beat_window {
            cfg = cfg.with_beat_window(w);
        }
        if m.ready_on_write {
            cfg = cfg.with_ready_on_write();
        }
        if let Some(ex) = &m.executor {
            let slot = executor_slots
                .get(ex.as_str())
                .ok_or_else(|| format!("{}: unknown executor `{ex}`", m.name))?;
            cfg = cfg.with_executor(slot);
        }

        // Dep rows (spawn order) and the ready/bound subsets.
        let mut row: Vec<u8> = Vec::new();
        let mut ready_deps: Vec<&'static TaskNode> = Vec::new();
        let mut bound_deps: Vec<&'static TaskNode> = Vec::new();
        for d in &m.deps {
            let targets: Vec<usize> =
                if let Some(k) = model.nodes.iter().position(|n| n.name == d.name) {
                    vec![k]
                } else {
                    // A dep naming a pool resolves to the pool's floor member
                    // only, matching the crate: `deps: [WORKERS]` means "once the
                    // pool floor is up", not "once every member is".
                    model
                        .pools
                        .iter()
                        .find(|p| p.name == d.name)
                        .and_then(|p| p.members.first())
                        .and_then(|mn| model.nodes.iter().position(|n| &n.name == mn))
                        .map(|k| vec![k])
                        .unwrap_or_default()
                };
            for k in targets {
                row.push(k as u8);
                let dep_node = node_rts[k]
                    .ok_or_else(|| {
                        format!("{}: dep `{}` not built before dependent", m.name, d.name)
                    })?
                    .node;
                if d.ready {
                    ready_deps.push(dep_node);
                }
                if d.bound {
                    bound_deps.push(dep_node);
                }
            }
        }
        dep_rows[i] = Box::leak(row.into_boxed_slice());
        if !ready_deps.is_empty() {
            cfg = cfg.with_ready_deps(Box::leak(ready_deps.into_boxed_slice()));
        }
        if !bound_deps.is_empty() {
            cfg = cfg.with_bound_deps(Box::leak(bound_deps.into_boxed_slice()));
        }

        // Resource gates, provides and budget claims. A budget gates its
        // holders like any slot (unprovided = `ResourceMissing` at the
        // deadline); the claims table is what the supervisor releases when
        // the holder stops, and this node's slot in every budget is `i`.
        if !m.resources.is_empty() {
            let gates: Vec<&'static dyn ResourceGate> = m
                .resources
                .iter()
                .map(|r| resource_map[r.name.as_str()].slot.gate())
                .collect();
            cfg = cfg.with_resources(Box::leak(gates.into_boxed_slice()));
        }
        let claims: Vec<(&'static ResourceRt, Claimant)> = m
            .resources
            .iter()
            .filter_map(|r| {
                let rt = resource_map[r.name.as_str()];
                rt.slot.budget().map(|b| (rt, b.claimant(i as u8)))
            })
            .collect();
        if !claims.is_empty() {
            let table: Vec<(&'static dyn Divisible, u8)> = claims
                .iter()
                .map(|(r, c)| (r.slot.budget().unwrap() as &'static dyn Divisible, c.slot()))
                .collect();
            cfg = cfg.with_claims(Box::leak(table.into_boxed_slice()));
        }
        if !m.provides.is_empty() {
            let gates: Vec<&'static dyn ResourceGate> = m
                .provides
                .iter()
                .map(|r| resource_map[r.as_str()].slot.gate())
                .collect();
            cfg = cfg.with_provides(Box::leak(gates.into_boxed_slice()));
        }

        // Coupling tables. Every entry wraps the signal's shared point, so
        // `same_signal` (pointer identity on the point) matches across nodes.
        let mut reads_rt = Vec::new();
        let mut writes_rt = Vec::new();
        for (refs, out, is_write) in [
            (&m.reads, &mut reads_rt, false),
            (&m.writes, &mut writes_rt, true),
        ] {
            let mut table: Vec<Coupling> = Vec::new();
            for r in refs {
                let j = sig_index(&r.name);
                let sig = signals[j];
                let mut c = Coupling::new(sig.name, point_dyn(&sig.gate));
                if r.observed {
                    c = c.observed(Observer::new(registry::OBS_FNS[j]));
                }
                if r.beat {
                    c = c.beat();
                }
                if is_write && r.veto {
                    // Contributor bits are numbered per gate over the
                    // veto-carrying writers in declaration order, as the
                    // macro numbers them in item order.
                    let slot = model.signals[j]
                        .veto_writers
                        .iter()
                        .position(|w| *w == m.name)
                        .expect("a veto writer is listed on its signal");
                    c = c.veto(slot as u8);
                }
                table.push(c);
                out.push(sig);
            }
            if !table.is_empty() {
                let inner: &'static [Coupling] = Box::leak(table.into_boxed_slice());
                let outer: &'static [&'static [Coupling]] =
                    Box::leak(vec![inner].into_boxed_slice());
                cfg = if is_write {
                    cfg.with_writes(outer)
                } else {
                    cfg.with_reads(outer)
                };
            }
        }

        let cfg: &'static NodeCfg = leak(cfg);
        let node: &'static TaskNode = leak(TaskNode::new(cfg, m.disabled));
        // SAFETY: single-threaded build phase; nothing reads the slot array
        // until the executor starts.
        unsafe {
            (*GRAPH_SLOTS.0.get())[i] = Some(node);
        }

        let behavior = resolve_behavior(&node_specs[i], &signals, sig_index)?;
        // Retirement watches one gate: a producer with several Backed
        // outputs would retire while readers still hold the others.
        if matches!(
            behavior,
            Behavior::Periodic {
                retire_ms: Some(_),
                ..
            } | Behavior::GatedConsumer {
                retire_ms: Some(_),
                ..
            }
        ) {
            let backed: Vec<&str> = writes_rt
                .iter()
                .filter(|s| matches!(s.gate, Gate::Backed(_)))
                .map(|s| s.name)
                .collect();
            if backed.len() > 1 {
                badges.push(Badge {
                    item: m.name.clone(),
                    clause: "retire_ms".into(),
                    note: format!(
                        "retirement watches `{}` only; readers of {} keep no hold on this producer",
                        backed[0],
                        backed[1..]
                            .iter()
                            .map(|b| format!("`{b}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            }
        }
        if matches!(behavior, Behavior::Queue { .. })
            && let Some(out) = writes_rt.first()
        {
            out.depth_active
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let input = match &behavior {
            Behavior::Link { initially_up } => {
                if *initially_up {
                    1.0f32
                } else {
                    0.0
                }
            }
            Behavior::Periodic { .. } => 0.5,
            // The site limit dial: full capacity until turned down.
            Behavior::Budget { .. } => 1.0,
            _ => 0.0,
        };
        let consumes: Vec<&'static ResourceRt> = m
            .resources
            .iter()
            .filter(|r| resource_map[r.name.as_str()].kind == ResKind::Consume)
            .map(|r| resource_map[r.name.as_str()])
            .collect();
        let lends: Vec<&'static ResourceRt> = m
            .resources
            .iter()
            .filter(|r| resource_map[r.name.as_str()].kind == ResKind::Lend)
            .map(|r| resource_map[r.name.as_str()])
            .collect();
        let provides: Vec<&'static ResourceRt> = m
            .provides
            .iter()
            .map(|r| resource_map[r.as_str()])
            .collect();
        let read_marks = reads_rt.iter().map(|_| AtomicU32::new(0)).collect();
        node_rts[i] = Some(leak(NodeRt {
            idx: i,
            node,
            model: m,
            behavior,
            reads: reads_rt,
            read_marks,
            writes: writes_rt,
            provides,
            consumes,
            lends,
            claims,
            input: AtomicU32::new(input.to_bits()),
            pool: model
                .pools
                .iter()
                .position(|p| p.members.iter().any(|mn| mn == &m.name)),
            session_open: AtomicBool::new(false),
            last_poll_us: AtomicU32::new(0),
            max_poll_us: AtomicU32::new(0),
            exec_us: AtomicU64::new(0),
            exec_id: AtomicU32::new(0),
        }));
    }
    let node_rts: Vec<&'static NodeRt> = node_rts.into_iter().map(Option::unwrap).collect();

    // Pools.
    let mut pools: Vec<&'static dyn Pool> = Vec::new();
    let mut pool_rts = Vec::new();
    for p in &model.pools {
        let members: Vec<&'static TaskNode> = p
            .members
            .iter()
            .map(|mn| node_rts[model.nodes.iter().position(|n| &n.name == mn).unwrap()].node)
            .collect();
        let members: &'static [&'static TaskNode] = Box::leak(members.into_boxed_slice());
        let pool = leak(ElasticPool {
            nodes: members,
            min: p.min,
            max: p.max,
            policy: DeferredShrink::new(Duration::from_millis(p.cooldown_ms)),
        });
        pools.push(pool as &'static dyn Pool);
        // `min:` is the shrink floor, not a start floor: bring-up starts the
        // `Terminate` members and the policy grows the rest on demand, so a
        // floor above the always-on count is never reached by a quiet pool.
        let always_on = p.modes.iter().filter(|m| m.as_str() == "Terminate").count();
        if usize::from(p.min) > always_on {
            badges.push(Badge {
                item: p.name.clone(),
                clause: "min".into(),
                note: format!(
                    "`min: {}` is a shrink floor; only the {always_on} `Terminate` member{} start at boot and the pool grows on demand, one idle spare at a time",
                    p.min,
                    if always_on == 1 { "" } else { "s" }
                ),
            });
        }
        pool_rts.push(registry::PoolRt {
            name: leak_str(&p.name),
            members,
            min: p.min,
            max: p.max,
        });
    }

    // Fill the Kahn order out to the full slot count (unused slots are None
    // and skipped, but order_at must cover 0..N).
    let mut order = [0u8; MAX_NODES];
    for (k, &oi) in model.order.iter().enumerate() {
        order[k] = oi;
    }
    for (k, slot) in (model.order.len()..MAX_NODES).zip(model.nodes.len()..MAX_NODES) {
        order[k] = slot as u8;
    }

    let graph: &'static PgGraph = leak(Graph {
        nodes: slots,
        topo: PlaygroundTopo {
            rows: dep_rows,
            order,
        },
        pools: Box::leak(pools.into_boxed_slice()),
        graph_ref,
    });
    let sup: &'static PgSupervisor = leak(Supervisor::new(graph));

    registry::install(node_rts, signals, resources, pool_rts);
    let built = Built {
        graph,
        sup,
        named_executors,
        badges,
        escalations,
    };
    BUILT.set(built).ok().expect("built twice");
    Ok(BUILT.get().unwrap())
}

/// Drive the built supervisor forever: bring-up with the liveness monitor
/// running *alongside* it (`run()` only starts the monitor after `start()`
/// completes, so a `ready` dep on a `ready_on_write` node would deadlock a
/// cold boot — the crate's own tests use this same composition), then the
/// driver loop composed from the crate's public pieces: pools, the control
/// queue, bound-dep binds, the monitor. Not `run()`: that begins with its
/// own `start()`, and a second bring-up wave retries every bound-parked
/// node with a full `slot_timeout` budget before the first queued command
/// is served, so a demand-start requested during boot landed seconds late.
pub async fn drive_supervisor(
    spawner: &embassy_executor::Spawner,
) -> embassy_supervisor::NodeFault {
    use embassy_futures::select::{Either, Either3, select, select3};
    use embassy_supervisor::{wait_bind, wait_control};

    let sup = built().expect("build first").sup;
    match select(sup.start(spawner), sup.monitor()).await {
        Either::First(Ok(())) => {}
        Either::First(Err(fault)) => return fault,
        Either::Second(never) => match never {},
    }
    let driver = async {
        loop {
            match select(sup.run_pools(spawner), wait_control()).await {
                Either::First(fault) => return fault,
                Either::Second(cmd) => {
                    if let Err(fault) = sup.apply_control(cmd, spawner).await {
                        return fault;
                    }
                }
            }
        }
    };
    let binds = async {
        loop {
            wait_bind().await;
            if let Err(fault) = sup.apply_bind(spawner).await {
                return fault;
            }
        }
    };
    match select3(driver, binds, sup.monitor()).await {
        Either3::First(fault) | Either3::Second(fault) => fault,
        Either3::Third(never) => match never {},
    }
}

fn resolve_behavior(
    spec: &BehaviorSpec,
    signals: &[&'static SignalRt],
    sig_index: impl Fn(&str) -> usize,
) -> Result<Behavior, String> {
    Ok(match spec {
        BehaviorSpec::Periodic {
            period_ms,
            scaled,
            retire_ms,
        } => Behavior::Periodic {
            period_ms: *period_ms,
            scaled: *scaled,
            retire_ms: *retire_ms,
        },
        BehaviorSpec::Pipeline {
            work_ms,
            accumulate,
        } => Behavior::Pipeline {
            work_ms: *work_ms,
            accumulate: *accumulate,
        },
        BehaviorSpec::Server { busy_ms } => Behavior::Server { busy_ms: *busy_ms },
        BehaviorSpec::Poller { period_ms, txn_ms } => Behavior::Poller {
            period_ms: *period_ms,
            txn_ms: *txn_ms,
        },
        BehaviorSpec::Queue {
            capacity,
            policy,
            drain_ms,
        } => Behavior::Queue {
            capacity: *capacity,
            policy: *policy,
            drain_ms: *drain_ms,
        },
        BehaviorSpec::Budget {
            total,
            period_ms,
            step,
        } => Behavior::Budget {
            total: *total,
            period_ms: *period_ms,
            step: step.unwrap_or(total / 10).max(1),
        },
        BehaviorSpec::Session { busy_ms } => Behavior::Session { busy_ms: *busy_ms },
        BehaviorSpec::ControlLoop { period_ms } => Behavior::ControlLoop {
            period_ms: *period_ms,
        },
        BehaviorSpec::Selftest { run_ms } => Behavior::Selftest { run_ms: *run_ms },
        BehaviorSpec::PowerCoordinator => Behavior::PowerCoordinator,
        BehaviorSpec::Provider { startup_ms } => Behavior::Provider {
            startup_ms: *startup_ms,
        },
        BehaviorSpec::Link { initially_up } => Behavior::Link {
            initially_up: *initially_up,
        },
        BehaviorSpec::Oneshot { run_ms } => Behavior::Oneshot { run_ms: *run_ms },
        BehaviorSpec::Watchdog { feed_ms } => Behavior::Watchdog { feed_ms: *feed_ms },
        BehaviorSpec::VetoWriter { period_ms } => Behavior::VetoWriter {
            period_ms: *period_ms,
        },
        BehaviorSpec::VetoSink { period_ms } => Behavior::VetoSink {
            period_ms: *period_ms,
        },
        BehaviorSpec::Idle => Behavior::Idle,
        BehaviorSpec::GatedConsumer {
            open,
            period_ms,
            delay_ms,
            retire_ms,
        } => {
            let sig = signals[sig_index(open)];
            let Gate::Backed(target) = sig.gate else {
                return Err(format!("signal `{open}` should have been built Backed"));
            };
            Behavior::GatedConsumer {
                entry: sig.coupling,
                target,
                period_ms: *period_ms,
                delay_ms: *delay_ms,
                retire_ms: *retire_ms,
            }
        }
        BehaviorSpec::LeaseUser { lease, hold_ms } => {
            let sig = signals[sig_index(lease)];
            let Gate::Leased(leased) = sig.gate else {
                return Err(format!("signal `{lease}` should have been built Leased"));
            };
            Behavior::LeaseUser {
                leased,
                hold_ms: *hold_ms,
            }
        }
    })
}
