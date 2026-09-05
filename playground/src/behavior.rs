//! The generic interpreted worker: one async fn whose behavior per node is
//! chosen by scenario metadata (or inferred from the graph shape), driving
//! the real task-side supervisor APIs — beats, readiness, busy marks,
//! resource provides, gated reads, leases, pause parking.

use std::future::Future;
use std::sync::atomic::Ordering;

use embassy_futures::select::select;
use embassy_supervisor::{Backed, Coupling, Injected, Leased, Mode, ShrinkFastGrowSlow, Sig};
use embassy_time::{Duration, Instant, Timer};
use serde::Deserialize;
use std::sync::atomic::AtomicU32;

use crate::registry::{self, Gate, HELD_BY_NONE, NodeRt, ResourceRt, SignalRt};

fn default_true() -> bool {
    true
}

/// A `queue` behavior's overflow policy. Direction-dependent in real systems:
/// back-pressure toward anything you can slow down, drop toward anything
/// driven by a clock you cannot.
#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OverflowPolicy {
    /// Full: refuse the arrival, count it, never block the producer (the cFS
    /// software-bus pipe).
    Reject,
    /// Full: stop consuming, so the backlog piles up at the producer (a
    /// planner-to-RT motion queue).
    Backpressure,
    /// Full: admit the arrival, lose the oldest (an RT-to-logger ring).
    DropOldest,
}

/// What scenario metadata may say about a node (keyed by node name or by its
/// `task:` path text).
#[derive(Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BehaviorSpec {
    /// Emit to every declared `writes:` signal each period; beats. With
    /// `scaled`, the input dial is activity: emission accumulates by the
    /// input each period and a write goes out only when a whole unit is
    /// reached — 0 is a still sensor, 1 a write every period. With
    /// `retire_ms`, a producer whose output runs `Backed` retires itself
    /// once no reader has held the gate for that long: `TaskNode::retire`
    /// withdraws readiness and requests its own deactivate.
    Periodic {
        period_ms: u64,
        #[serde(default)]
        scaled: bool,
        #[serde(default)]
        retire_ms: Option<u64>,
    },
    /// Consume `reads:`, produce `writes:` each cycle; beats. With
    /// `accumulate`, each produced batch adds one to the output signal's
    /// value instead of republishing the input dial — a running total (an
    /// energy register, a sample count) that survives a respawn because it
    /// lives in the signal's atomic.
    Pipeline {
        work_ms: u64,
        #[serde(default)]
        accumulate: bool,
    },
    /// Pool member: takes a job when declared input arrives or the pool's
    /// load dial fires; marks busy while serving.
    Server { busy_ms: u64 },
    /// Pool member owning one field transaction per cycle on a serialized
    /// link; the load dial stretches the transaction until the member calls
    /// for help (`mark_busy`), growing the pool.
    Poller { period_ms: u64, txn_ms: u64 },
    /// Bounded staging buffer. Depth is published on the first write.
    /// `drain_ms` is the per-consumer service time, so drain rate scales
    /// with the number of active readers.
    Queue {
        capacity: u32,
        policy: OverflowPolicy,
        drain_ms: u64,
    },
    /// The allocator of a `divisible` resource it `provides:`: fills the
    /// real `Budget` with `total` units and re-divides it over the holders'
    /// wants with `ShrinkFastGrowSlow` — cuts land at once, increases at
    /// most `step` units (default a tenth of the total) per `period_ms`.
    /// The node's dial scales the provided capacity.
    Budget {
        total: u32,
        period_ms: u64,
        #[serde(default)]
        step: Option<u32>,
    },
    /// Pool member holding a client session: the dial is the client count,
    /// dealt out across the running members (see [`session_wanted`]); busy
    /// for the session's duration.
    Session { busy_ms: u64 },
    /// Fixed-rate loop. Holds last-good output when the input goes quiet.
    ControlLoop { period_ms: u64 },
    /// Detached run-once: reports, then returns without ever being respawned.
    Selftest { run_ms: u64 },
    /// Marks the parked power-coordinator node: `start_run` spawns the real
    /// coordinator task (it needs the root `Spawner`), not the generic worker.
    PowerCoordinator,
    /// Slow bring-up, then `provide()`s its `provides:` slots and turns ready.
    Provider { startup_ms: u64 },
    /// Readiness follows the UI's up/down switch.
    Link {
        #[serde(default = "default_true")]
        initially_up: bool,
    },
    /// Runs once, then exits (the supervisor sees a completed worker).
    Oneshot { run_ms: u64 },
    /// Data-deps demo: `open()`s a gated signal (demand-starting its
    /// producer), then consumes it each period. `delay_ms` holds the open
    /// back, so a scenario can stagger its demand-starts into watchable
    /// waves instead of one instant cascade.
    GatedConsumer {
        open: String,
        period_ms: u64,
        #[serde(default)]
        delay_ms: u64,
        /// As on `periodic`: a gated stage that is itself a demand-started
        /// producer retires once its own readers have left.
        #[serde(default)]
        retire_ms: Option<u64>,
    },
    /// Data-deps demo: repeatedly holds a lease on a `Leased` signal.
    LeaseUser { lease: String, hold_ms: u64 },
    /// Feeds the simulated hardware watchdog; when this task stops feeding,
    /// the watchdog bites and the MCU reboots.
    Watchdog { feed_ms: u64 },
    /// A contributor to a `veto` write: asserts its bit while the widget
    /// switch is on, releases it when off; reads its inputs each period. A
    /// stopped writer's bit stays up (fail-safe), until it runs again and
    /// re-evaluates.
    VetoWriter { period_ms: u64 },
    /// The consumer of a `veto` read: produces onward every period while
    /// any contributor bit is up, sits quiet otherwise.
    VetoSink { period_ms: u64 },
    /// Beats slowly and waits for shutdown.
    Idle,
}

/// The resolved runtime behavior (signal names bound to runtime objects).
pub enum Behavior {
    Periodic {
        period_ms: u64,
        scaled: bool,
        retire_ms: Option<u64>,
    },
    Pipeline {
        work_ms: u64,
        accumulate: bool,
    },
    Server {
        busy_ms: u64,
    },
    Poller {
        period_ms: u64,
        txn_ms: u64,
    },
    Queue {
        capacity: u32,
        policy: OverflowPolicy,
        drain_ms: u64,
    },
    Budget {
        total: u32,
        period_ms: u64,
        step: u32,
    },
    Session {
        busy_ms: u64,
    },
    ControlLoop {
        period_ms: u64,
    },
    Selftest {
        run_ms: u64,
    },
    PowerCoordinator,
    Provider {
        startup_ms: u64,
    },
    Link {
        initially_up: bool,
    },
    Oneshot {
        run_ms: u64,
    },
    GatedConsumer {
        entry: &'static Coupling,
        target: &'static Backed<()>,
        period_ms: u64,
        delay_ms: u64,
        retire_ms: Option<u64>,
    },
    LeaseUser {
        leased: &'static Leased<AtomicU32>,
        hold_ms: u64,
    },
    Watchdog {
        feed_ms: u64,
    },
    VetoWriter {
        period_ms: u64,
    },
    VetoSink {
        period_ms: u64,
    },
    Idle,
}

fn input_f32(rt: &NodeRt) -> f32 {
    f32::from_bits(rt.input.load(Ordering::Relaxed))
}

fn emit(rt: &'static NodeRt, v: f32) {
    for s in &rt.writes {
        s.writes.fetch_add(1, Ordering::Relaxed);
        s.value.store(v.to_bits(), Ordering::Relaxed);
    }
}

fn write_all(rt: &'static NodeRt) {
    emit(rt, input_f32(rt));
}

/// Consume whatever the writers produced since this node's last look.
/// Returns false when nothing new arrived, so a dead producer visibly
/// starves its consumers (frozen read counters, no onward writes).
fn read_new(rt: &'static NodeRt) -> bool {
    let mut any = false;
    for (s, mark) in rt.reads.iter().zip(&rt.read_marks) {
        let w = s.writes.load(Ordering::Relaxed);
        if mark.swap(w, Ordering::Relaxed) != w {
            s.reads.fetch_add(1, Ordering::Relaxed);
            any = true;
        }
    }
    any
}

/// Competing-consumer claim for pool members: each written unit is consumed
/// by exactly one claimant, so a pool's read counter tracks the writes
/// instead of multiplying by the member count.
fn claim_new(rt: &'static NodeRt) -> bool {
    let mut any = false;
    for s in &rt.reads {
        let w = s.writes.load(Ordering::Relaxed);
        if s.claims
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| {
                (c < w).then_some(c + 1)
            })
            .is_ok()
        {
            s.reads.fetch_add(1, Ordering::Relaxed);
            any = true;
        }
    }
    any
}

/// Assert readiness by hand — unless the node declares `ready_on_write`,
/// where the sweep's own poll of the observed-beat write is what asserts it
/// and a manual set_ready would defeat the story.
fn assert_ready(rt: &'static NodeRt) {
    if !rt.model.ready_on_write {
        rt.node.set_ready();
    }
}

/// Count active consumers of `sig`. Queue drain rate scales with this.
/// Session members count only while open; an unread signal counts as one sink.
fn running_readers(sig: &'static SignalRt) -> u32 {
    let mut declared = 0u32;
    let mut active = 0u32;
    for rt in registry::nodes() {
        if rt.reads.iter().any(|s| std::ptr::eq(*s, sig)) {
            declared += 1;
            let open = !matches!(rt.behavior, Behavior::Session { .. })
                || rt.session_open.load(Ordering::Relaxed);
            if rt.node.is_running() && open {
                active += 1;
            }
        }
    }
    if declared == 0 { 1 } else { active }
}

/// This node's pool siblings, itself included; just itself for a plain node.
fn pool_members(rt: &'static NodeRt) -> impl Iterator<Item = &'static NodeRt> {
    registry::nodes()
        .iter()
        .copied()
        .filter(move |m| m.idx == rt.idx || (rt.pool.is_some() && m.pool == rt.pool))
}

/// Whether an idle session member should open now: more clients than open
/// sessions across the pool. Sessions go to whichever running member asks
/// first, not to a member number: after a shrink the surviving spare may be
/// the highest-numbered member, and it must still take the next client.
fn session_wanted(rt: &'static NodeRt, clients: u32) -> bool {
    let open = pool_members(rt)
        .filter(|m| m.session_open.load(Ordering::Relaxed))
        .count() as u32;
    open < clients
}

/// Whether an open session should close now: fewer clients than open
/// sessions, and this is the highest-numbered live one, so a dial step down
/// ends exactly one session and never migrates another.
fn session_surplus(rt: &'static NodeRt, clients: u32) -> bool {
    let mut open = 0;
    for m in pool_members(rt) {
        if m.session_open.load(Ordering::Relaxed) {
            open += 1;
            if m.idx > rt.idx {
                return false;
            }
        }
    }
    open > clients
}

/// Once per task instance: subscribe reads from now (the cumulative write
/// history is not a backlog), reopen any leased output this producer owns,
/// and take the take-kind resources. For a `Pause` node this runs once, not
/// per resume — a parked task still holds what it took.
fn prologue(rt: &'static NodeRt) {
    for (s, mark) in rt.reads.iter().zip(&rt.read_marks) {
        mark.store(s.writes.load(Ordering::Relaxed), Ordering::Relaxed);
    }
    for s in &rt.writes {
        if let Gate::Leased(l) = &s.gate
            && l.is_drained()
        {
            l.reopen();
            log::info!("{}: reopened {}", rt.node.name(), s.name);
        }
    }
    for r in &rt.consumes {
        if r.slot.take().is_some() {
            log::info!("{}: consumed {}", rt.node.name(), r.name);
        }
    }
    for r in &rt.lends {
        if r.slot.take().is_some() {
            r.held_by.store(rt.idx, Ordering::Relaxed);
        }
    }
    // A plain holder of a budget wants as much as there is; a session
    // states its want only while a client is connected (see `Session`).
    if !matches!(rt.behavior, Behavior::Session { .. }) {
        for (r, c) in &rt.claims {
            c.want(capacity_of(r));
        }
    }
}

/// A budget's provided capacity, or what the allocator last provided while
/// it is momentarily empty, never zero: a want of zero is no claim at all.
fn capacity_of(r: &ResourceRt) -> u32 {
    r.slot
        .budget()
        .map(|b| b.capacity())
        .filter(|&c| c > 0)
        .unwrap_or_else(|| r.capacity.load(Ordering::Relaxed))
        .max(1)
}

/// Ordered post-cancel work, run *before* the shutdown ack: drain any leased
/// output (new leases refused, live ones waited out — a consumer leaking a
/// guard turns this into an ordinary `ShutdownTimeout`), then restore lent
/// resources so a respawn re-takes the same instance. Consume slots stay
/// empty: that is the point of `consume`.
async fn epilogue(rt: &'static NodeRt) {
    for s in &rt.writes {
        if let Gate::Leased(l) = &s.gate {
            log::info!("{}: draining {} before release", rt.node.name(), s.name);
            l.drain().await;
        }
    }
    for r in &rt.lends {
        r.held_by.store(HELD_BY_NONE, Ordering::Relaxed);
        r.slot.provide(1);
    }
}

/// The single worker body every node runs. `Pause` nodes park (ack, wait,
/// resume in place, keeping held resources); everything else runs
/// cancellable. The body sits inside the crate's `Injected` shell, exactly
/// where the `task:` macro puts a worker: an injected stall withholds its
/// polls, a crash drops it, and a wedge (which lives in the node, not the
/// shell) hides the stop request and swallows the ack. Nothing in the
/// behaviors cooperates with a fault.
pub async fn run(rt: &'static NodeRt) {
    rt.node.adopt_current().await;
    prologue(rt);
    if rt.node.mode() == Mode::Pause {
        // `Err`: parked and resumed, rerun the body in place. `Ok`: body
        // returned or crashed, so the task ends like any finished worker.
        loop {
            let body = core::pin::pin!(body(rt));
            if rt
                .node
                .run_pausable(Injected::new(rt.node, body))
                .await
                .is_ok()
            {
                break;
            }
        }
        epilogue(rt).await;
        rt.node.mark_exited();
    } else {
        let body = core::pin::pin!(body(rt));
        let done = rt.node.run_cancellable(Injected::new(rt.node, body)).await;
        // The body is dropped mid-await, so any busy/ready state and the
        // session flag would outlive the task. Clear them on every stop path.
        rt.node.mark_idle();
        rt.node.clear_ready();
        rt.session_open.store(false, Ordering::Relaxed);
        epilogue(rt).await;
        match done {
            // Both paths release budget claims. `Ok(None)` is a crash.
            Ok(_) => rt.node.mark_exited(),
            // Stop: teardown done, now ack.
            Err(_aborted) => rt.node.ack_dropped(),
        }
    }
}

/// Run `work` beside a retirement watch when the behavior carries a cooldown
/// and one of its writes runs `Backed`: a demand-started producer retires
/// once its last reader has left. `retire` clears readiness (a late opener
/// waits for the next activation instead of reading a producer on its way
/// out), then requests the node's own deactivate through the control queue,
/// which cancels this body like any stop.
async fn retiring(rt: &'static NodeRt, retire_ms: Option<u64>, work: impl Future<Output = ()>) {
    let backed = retire_ms.and_then(|ms| {
        rt.writes.iter().find_map(|s| match s.gate {
            Gate::Backed(target) => Some((s.coupling, target, ms)),
            _ => None,
        })
    });
    match backed {
        None => work.await,
        Some((entry, target, ms)) => {
            let node = rt.node;
            let retire = async {
                node.retire(Sig { entry, target }, Duration::from_millis(ms))
                    .await;
                node.report_status("retired: no readers");
                log::info!("{}: retired, no reader for {ms} ms", node.name());
                core::future::pending::<()>().await;
            };
            select(work, retire).await;
        }
    }
}

async fn body(rt: &'static NodeRt) {
    let node = rt.node;
    match &rt.behavior {
        Behavior::Periodic {
            period_ms,
            scaled,
            retire_ms,
        } => {
            assert_ready(rt);
            node.report_status("sampling");
            let work = async {
                // Scaled emission: the dial is activity, and a still sensor
                // has nothing to report — it stays alive (beats) but writes
                // nothing.
                let mut acc = 0.0f32;
                loop {
                    Timer::after_millis(*period_ms).await;
                    rt.node.beat();
                    if *scaled {
                        acc = (acc + input_f32(rt)).clamp(0.0, 16.0);
                        while acc >= 1.0 {
                            acc -= 1.0;
                            write_all(rt);
                        }
                    } else {
                        write_all(rt);
                    }
                }
            };
            retiring(rt, *retire_ms, work).await;
        }
        Behavior::Pipeline {
            work_ms,
            accumulate,
        } => {
            assert_ready(rt);
            node.report_status("processing");
            loop {
                Timer::after_millis(*work_ms).await;
                rt.node.beat();
                // A starved stage stays alive (it beats) but produces
                // nothing until upstream writes again.
                if rt.reads.is_empty() || read_new(rt) {
                    if *accumulate {
                        // Running total in the signal atomic itself, so it
                        // survives a respawn like a register would.
                        let total = rt
                            .writes
                            .first()
                            .map(|s| f32::from_bits(s.value.load(Ordering::Relaxed)))
                            .unwrap_or(0.0);
                        emit(rt, total + 1.0);
                    } else {
                        write_all(rt);
                    }
                }
            }
        }
        Behavior::Server { busy_ms } => {
            node.set_ready();
            // One stable status: report_status logs every change, and
            // flipping serving/idle per published report floods the log.
            node.report_status("publishing");
            // A job is an arrived input (a report to publish) or a tick of
            // the load dial (0..=1 extra jobs per tick); a busy member asks
            // the pool to reevaluate scaling.
            let mut acc = 0.0f32;
            loop {
                Timer::after_millis(200).await;
                rt.node.beat();
                acc += input_f32(rt);
                let arrived = claim_new(rt);
                let surge = acc >= 1.0;
                if surge {
                    acc -= 1.0;
                    // mark_busy is the documented scale-out hint, so only
                    // surge load claims it; routine arrivals are quick
                    // publishes served at the current size.
                    node.mark_busy();
                }
                if arrived || surge {
                    Timer::after_millis(*busy_ms).await;
                    if surge {
                        node.mark_idle();
                    }
                }
            }
        }
        Behavior::Poller { period_ms, txn_ms } => {
            assert_ready(rt);
            node.report_status("polling");
            loop {
                Timer::after_millis(*period_ms).await;
                rt.node.beat();
                // One serialized field transaction; the device dial stretches
                // it (more registers, slower peers). A cycle eating most of
                // the period is this member calling for help.
                let txn = (*txn_ms as f32 * (1.0 + input_f32(rt) * 4.0)) as u64;
                let overrun = txn * 10 >= *period_ms * 8;
                if overrun {
                    node.mark_busy();
                }
                Timer::after_millis(txn).await;
                if !overrun {
                    node.mark_idle();
                }
                let _ = claim_new(rt);
                write_all(rt);
            }
        }
        Behavior::Queue {
            capacity,
            policy,
            drain_ms,
        } => {
            node.set_ready();
            node.report_status("staging");
            let depth_sig = rt.writes.first().copied();
            let set_depth = |d: u32| {
                if let Some(s) = depth_sig {
                    s.depth.store(d, Ordering::Relaxed);
                }
            };
            // Depth lives in the signal's atomic, not a body local: a parked
            // Pause queue keeps its FIFO across a sleep (the park drops the
            // body future), and the UI reads the same number.
            let mut depth = depth_sig
                .map(|s| s.depth.load(Ordering::Relaxed))
                .unwrap_or(0);
            let mut lost = 0u32;
            // Track elapsed time, not wakes, so a late wake settles all
            // owed service at once.
            let mut last = Instant::now();
            let mut credit_ms = 0u64;
            // Wake on a fixed cadence; drain rate scales with active readers.
            let tick_ms = (*drain_ms).clamp(50, 250);
            // Latch overload until the queue has real room, so the status
            // does not flicker.
            let mut hot = false;
            loop {
                Timer::after_millis(tick_ms).await;
                rt.node.beat();
                let now = Instant::now();
                let elapsed_ms = now.saturating_duration_since(last).as_millis();
                last = now;
                // Admit arrivals until full, then apply the overflow policy.
                let mut overflowed = false;
                for (s, mark) in rt.reads.iter().zip(&rt.read_marks) {
                    let w = s.writes.load(Ordering::Relaxed);
                    while mark.load(Ordering::Relaxed) != w {
                        if depth < *capacity {
                            depth += 1;
                        } else {
                            overflowed = true;
                            match policy {
                                OverflowPolicy::Reject => lost += 1,
                                OverflowPolicy::Backpressure => break,
                                OverflowPolicy::DropOldest => lost += 1,
                            }
                        }
                        mark.fetch_add(1, Ordering::Relaxed);
                        s.reads.fetch_add(1, Ordering::Relaxed);
                    }
                }
                // Drain one item per active consumer per service time.
                if depth > 0
                    && let Some(out) = depth_sig
                {
                    let readers = u64::from(running_readers(out));
                    credit_ms += elapsed_ms * readers;
                    let take = (credit_ms / *drain_ms).min(u64::from(depth)) as u32;
                    // An idle server banks nothing: at most a fraction of
                    // one item carries over.
                    credit_ms = (credit_ms - u64::from(take) * *drain_ms).min(*drain_ms);
                    depth -= take;
                    for _ in 0..take {
                        write_all(rt);
                    }
                } else {
                    credit_ms = 0;
                }
                set_depth(depth);
                // The full-state wording follows the policy — a ring at
                // capacity is operating as designed, not overflowing.
                let was_hot = hot;
                hot = overflowed || (hot && depth + 1 >= *capacity);
                if was_hot && !hot && lost > 0 {
                    log::info!(
                        "{}: overload over, {} {}",
                        node.name(),
                        lost,
                        match policy {
                            OverflowPolicy::DropOldest => "dropped",
                            _ => "rejected",
                        }
                    );
                    lost = 0;
                }
                node.report_status(if hot {
                    match policy {
                        OverflowPolicy::Reject => "overflowing: rejecting",
                        OverflowPolicy::Backpressure => "full: back-pressuring",
                        OverflowPolicy::DropOldest => "ring full: dropping oldest",
                    }
                } else if depth * 10 >= *capacity * 8 {
                    "backlog"
                } else {
                    "staging"
                });
            }
        }
        Behavior::Budget {
            total,
            period_ms,
            step,
        } => {
            assert_ready(rt);
            let Some(res) = rt.provides.iter().find(|r| r.slot.budget().is_some()) else {
                log::error!(
                    "{}: a budget behavior allocates a `divisible` resource it provides:; none declared",
                    node.name()
                );
                node.report_status("nothing to allocate");
                loop {
                    Timer::after_millis(*period_ms).await;
                    rt.node.beat();
                }
            };
            let budget = res.slot.budget().unwrap();
            // The site limit: the dial scales the provided capacity, so a
            // derate is a `provide` with less and every grant above the new
            // fair share is cut on the next division.
            let limit =
                |rt: &NodeRt| ((*total as f32) * input_f32(rt).clamp(0.0, 1.0)).round() as u32;
            let capacity = limit(rt);
            budget.provide(capacity);
            res.capacity.store(capacity, Ordering::Relaxed);
            log::info!("{}: provided {} ({capacity} units)", node.name(), res.name);
            let policy = ShrinkFastGrowSlow::new(*step, Duration::from_millis(*period_ms));
            node.report_status("allocating");
            loop {
                rt.node.beat();
                // The allocator watches its declared inputs (site load, a
                // derate) — consume them so the dataflow reads as live.
                let _ = read_new(rt);
                // The allocator owns the offer: compare against the live
                // budget, not a shadow, so a hand provide or clear from the
                // resource readout is overridden on the next wake.
                let now = limit(rt);
                if now != budget.capacity() {
                    budget.provide(now);
                    res.capacity.store(now, Ordering::Relaxed);
                    log::info!("{}: {} capacity now {now}", node.name(), res.name);
                }
                // Divide, then sleep until something moves (a want, a
                // release, a capacity change) or the ramp's next step is
                // due; the period also paces the beats.
                let next = budget.rebalance(&policy, Instant::now());
                let due =
                    next.unwrap_or_else(|| Instant::now() + Duration::from_millis(*period_ms));
                let _ = select(budget.wait_change(), Timer::at(due)).await;
            }
        }
        Behavior::Session { busy_ms } => {
            node.set_ready();
            let mut open = false;
            loop {
                Timer::after_millis(200).await;
                rt.node.beat();
                // The dial is the client count, the pool max the real
                // session cap. The check and the flag store share one poll,
                // so two members never answer the same client.
                let clients = input_f32(rt).max(0.0) as u32;
                let want = if open {
                    !session_surplus(rt, clients)
                } else {
                    session_wanted(rt, clients)
                };
                if want != open {
                    open = want;
                    rt.session_open.store(open, Ordering::Relaxed);
                    if open {
                        node.mark_busy();
                        // A connected client claims its share of every
                        // budget this member holds; the allocator re-divides.
                        for (r, c) in &rt.claims {
                            c.want(capacity_of(r));
                        }
                        node.report_status("session open");
                    } else {
                        node.mark_idle();
                        for (_, c) in &rt.claims {
                            c.release();
                        }
                        node.report_status("idle");
                    }
                }
                if open {
                    let _ = claim_new(rt);
                    Timer::after_millis(*busy_ms).await;
                    write_all(rt);
                }
            }
        }
        Behavior::ControlLoop { period_ms } => {
            assert_ready(rt);
            node.report_status("tracking");
            let mut last_good = 0.0f32;
            let mut fresh = false;
            let mut missed = 0u32;
            let mut gaps = [1u32; 8];
            let mut seen = false;
            loop {
                Timer::after_millis(*period_ms).await;
                rt.node.beat();
                // A fixed-rate loop never skips an output: with fresh input
                // it tracks, without it holds last-good instead of dying.
                if read_new(rt) {
                    if seen {
                        gaps.rotate_right(1);
                        gaps[0] = missed + 1;
                    }
                    seen = true;
                    missed = 0;
                    last_good = rt
                        .reads
                        .first()
                        .map(|s| f32::from_bits(s.value.load(Ordering::Relaxed)))
                        .unwrap_or(last_good);
                    if !fresh {
                        fresh = true;
                        node.report_status("tracking");
                    }
                } else {
                    missed += 1;
                    let cadence = gaps.iter().copied().max().unwrap_or(1);
                    if fresh && missed >= (3 * cadence).max(2) {
                        fresh = false;
                        node.report_status("holding last-good");
                    }
                }
                emit(rt, last_good);
            }
        }
        Behavior::Selftest { run_ms } => {
            // Detached: run once ever; the next wake's respawn skips it.
            node.set_detached(true);
            node.set_ready();
            node.report_status("self-test");
            Timer::after_millis(*run_ms).await;
            write_all(rt);
            node.report_status("passed");
            log::info!("{}: self-test passed", node.name());
        }
        Behavior::PowerCoordinator => {
            // Never reached: the coordinator is spawned by hand in start_run
            // (it needs the root Spawner). A stray generic spawn just parks.
            core::future::pending::<()>().await;
        }
        Behavior::Provider { startup_ms } => {
            node.report_status("starting");
            Timer::after_millis(*startup_ms).await;
            for r in &rt.provides {
                r.slot.provide(1);
                log::info!("{}: provided {}", node.name(), r.name);
            }
            node.set_ready();
            node.report_status("serving");
            loop {
                Timer::after_millis(500).await;
                rt.node.beat();
                write_all(rt);
            }
        }
        Behavior::Link { initially_up } => {
            let mut up = *initially_up;
            let apply_up = |up: bool| {
                for r in &rt.provides {
                    if up {
                        r.slot.provide(1);
                    } else {
                        r.slot.clear();
                    }
                }
                if up {
                    node.set_ready();
                } else {
                    node.clear_ready();
                }
                node.report_status(if up { "link up" } else { "link down" });
            };
            apply_up(up);
            loop {
                Timer::after_millis(100).await;
                rt.node.beat();
                // An up link transmits: it consumes its declared reads
                // (upload frames); a down link lets them back up.
                if up {
                    let _ = read_new(rt);
                }
                let want_up = input_f32(rt) > 0.5;
                if want_up != up {
                    up = want_up;
                    if up {
                        apply_up(true);
                        // Service is back: poke the pool policies so demand
                        // (bound-stopped OnDemand members) regrows.
                        embassy_supervisor::request_scale();
                        log::info!("{}: link up", node.name());
                    } else {
                        // A leased output rolls over before the link drops:
                        // refuse new leases, wait out live ones, then clear.
                        for s in &rt.writes {
                            if let Gate::Leased(l) = &s.gate {
                                log::info!("{}: draining {} for rollover", node.name(), s.name);
                                l.drain().await;
                                l.reopen();
                            }
                        }
                        apply_up(false);
                        log::warn!("{}: link down", node.name());
                    }
                }
            }
        }
        Behavior::Oneshot { run_ms } => {
            node.set_ready();
            node.report_status("working");
            Timer::after_millis(*run_ms).await;
            write_all(rt);
            node.report_status("done");
            log::info!("{}: finished", node.name());
        }
        Behavior::GatedConsumer {
            entry,
            target,
            period_ms,
            delay_ms,
            retire_ms,
        } => {
            let work = async {
                if *delay_ms > 0 {
                    node.report_status("warming up");
                    Timer::after_millis(*delay_ms).await;
                }
                node.report_status("waiting for producer");
                // The guard is the reader count: it lives as long as this
                // body, and a stop drops it with the body.
                let _gate = node
                    .open(Sig {
                        entry,
                        target: *target,
                    })
                    .await;
                node.set_ready();
                node.report_status("consuming");
                log::info!("{}: producer serving, consuming", node.name());
                loop {
                    Timer::after_millis(*period_ms).await;
                    rt.node.beat();
                    // A gated consumer that also declares writes: is a gated
                    // pipeline stage (read through the gate, produce onward);
                    // both stall while the producer emits nothing new.
                    if read_new(rt) {
                        write_all(rt);
                    }
                }
            };
            retiring(rt, *retire_ms, work).await;
        }
        Behavior::LeaseUser { leased, hold_ms } => {
            node.set_ready();
            // One stable status while cycling: report_status logs every
            // change, and flipping holding/released per lease floods the log
            // pane with churn. Only the drained transition is news.
            node.report_status("cycling leases");
            loop {
                rt.node.beat();
                match leased.lease() {
                    Some(lease) => {
                        // Taking the lease is the read: consume whatever the
                        // producer published since the last hold.
                        let _ = read_new(rt);
                        node.report_status("cycling leases");
                        Timer::after_millis(*hold_ms).await;
                        drop(lease);
                        // A stage that leases frames and declares writes: is
                        // a pipeline stage (scale while holding, hand on).
                        write_all(rt);
                    }
                    None => {
                        node.report_status("drained: no lease");
                        Timer::after_millis(*hold_ms).await;
                    }
                }
                Timer::after_millis(*hold_ms).await;
            }
        }
        Behavior::Watchdog { feed_ms } => {
            // Feeding is the job: the feeder is ready the moment it arms.
            assert_ready(rt);
            node.report_status("feeding");
            let feed = || {
                crate::registry::HW_WATCHDOG
                    .last_fed_us
                    .store(embassy_time::Instant::now().as_micros(), Ordering::Relaxed);
            };
            feed();
            crate::registry::HW_WATCHDOG
                .armed
                .store(true, Ordering::Relaxed);
            loop {
                Timer::after_millis(*feed_ms).await;
                rt.node.beat();
                feed();
            }
        }
        Behavior::VetoWriter { period_ms } => {
            assert_ready(rt);
            let handle = rt.writes.iter().find_map(|s| match s.gate {
                Gate::Veto(target) => node
                    .veto(Sig {
                        entry: s.coupling,
                        target,
                    })
                    .map(|v| (*s, v)),
                _ => None,
            });
            let Some((sig, veto)) = handle else {
                log::error!(
                    "{}: a veto_writer behavior needs a `writes:` entry carrying `veto`",
                    node.name()
                );
                node.report_status("no veto to write");
                loop {
                    Timer::after_millis(*period_ms).await;
                    rt.node.beat();
                }
            };
            // Re-evaluate the pickup on every (re)start: a bit left up by a
            // stopped instance stays up until this instance decides.
            let mut picked = input_f32(rt) < 0.5;
            node.report_status("armed");
            loop {
                let want = input_f32(rt) >= 0.5;
                if want != picked {
                    picked = want;
                    let flipped = if want { veto.assert() } else { veto.release() };
                    emit(rt, if want { 1.0 } else { 0.0 });
                    node.report_status(if want { "tripping" } else { "reset" });
                    log::info!(
                        "{}: {} bit {} {}{}",
                        node.name(),
                        sig.name,
                        veto.slot(),
                        if want { "asserted" } else { "released" },
                        if flipped { " (gate flipped)" } else { "" }
                    );
                }
                Timer::after_millis(*period_ms).await;
                rt.node.beat();
                let _ = read_new(rt);
            }
        }
        Behavior::VetoSink { period_ms } => {
            assert_ready(rt);
            let Some((sig, gate)) = rt.reads.iter().find_map(|s| match s.gate {
                Gate::Veto(g) => Some((*s, g)),
                _ => None,
            }) else {
                log::error!(
                    "{}: a veto_sink behavior needs a `reads:` entry on a veto gate",
                    node.name()
                );
                node.report_status("no veto to watch");
                loop {
                    Timer::after_millis(*period_ms).await;
                    rt.node.beat();
                }
            };
            node.report_status("clear");
            let mut vetoed = false;
            loop {
                // Wake on the gate's own signal or the period, whichever
                // first; the period paces the beats and the onward writes.
                let wake = async {
                    if vetoed {
                        gate.wait_released().await
                    } else {
                        gate.wait_asserted().await
                    }
                };
                let _ = select(wake, Timer::after_millis(*period_ms)).await;
                rt.node.beat();
                let now = gate.is_asserted();
                if now != vetoed {
                    vetoed = now;
                    node.report_status(if now { "vetoed" } else { "clear" });
                    log::info!(
                        "{}: {} {}",
                        node.name(),
                        sig.name,
                        if now { "asserted" } else { "released" }
                    );
                }
                // Reading the gate is the read; the other inputs (a breaker
                // failure trip) are consumed as they arrive.
                let others = read_new(rt);
                if vetoed {
                    sig.reads.fetch_add(1, Ordering::Relaxed);
                    emit(rt, gate.contributors().count_ones() as f32);
                } else if others {
                    write_all(rt);
                }
            }
        }
        Behavior::Idle => {
            node.set_ready();
            loop {
                Timer::after_millis(1000).await;
                rt.node.beat();
            }
        }
    }
}

/// Pick a default behavior from the graph shape when the scenario metadata
/// names none, so user-typed nodes do something sensible immediately.
pub fn infer(model: &crate::model::NodeModel) -> BehaviorSpec {
    if model.pool.is_some() {
        BehaviorSpec::Server { busy_ms: 400 }
    } else if !model.provides.is_empty() {
        BehaviorSpec::Provider { startup_ms: 200 }
    } else if !model.reads.is_empty() && !model.writes.is_empty() {
        BehaviorSpec::Pipeline {
            work_ms: 150,
            accumulate: false,
        }
    } else if !model.writes.is_empty() {
        BehaviorSpec::Periodic {
            period_ms: 100,
            scaled: false,
            retire_ms: None,
        }
    } else if !model.reads.is_empty() {
        BehaviorSpec::GatedConsumer {
            open: model.reads[0].name.clone(),
            period_ms: 200,
            delay_ms: 0,
            retire_ms: None,
        }
    } else {
        BehaviorSpec::Idle
    }
}

/// A signal a `GatedConsumer` opens must run `Backed`; a `LeaseUser`'s must
/// run `Leased`. The builder collects these before creating the signals.
pub fn required_gate(spec: &BehaviorSpec) -> Option<(&str, crate::build::GateKind)> {
    match spec {
        BehaviorSpec::GatedConsumer { open, .. } => Some((open, crate::build::GateKind::Backed)),
        BehaviorSpec::LeaseUser { lease, .. } => Some((lease, crate::build::GateKind::Leased)),
        _ => None,
    }
}

pub fn signal_by_name(signals: &[&'static SignalRt], name: &str) -> Option<&'static SignalRt> {
    signals.iter().find(|s| s.name == name).copied()
}
