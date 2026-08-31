//! The generic interpreted worker: one async fn whose behavior per node is
//! chosen by scenario metadata (or inferred from the graph shape), driving
//! the real task-side supervisor APIs — beats, readiness, busy marks,
//! resource provides, gated reads, leases, pause parking.

use std::sync::atomic::Ordering;

use embassy_futures::select::{Either, select};
use embassy_supervisor::{Backed, Coupling, Leased, Mode, Sig};
use embassy_time::Timer;
use serde::Deserialize;
use std::sync::atomic::AtomicU32;

use crate::registry::{self, Gate, HELD_BY_NONE, NodeRt, SignalRt, fault};

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
    /// reached — 0 is a still sensor, 1 a write every period.
    Periodic {
        period_ms: u64,
        #[serde(default)]
        scaled: bool,
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
    /// A bounded staging buffer with an explicit overflow policy; depth is
    /// published on its first `writes:` signal, and it drains only while a
    /// consumer of that signal is running.
    Queue {
        capacity: u32,
        policy: OverflowPolicy,
        drain_ms: u64,
    },
    /// A divisible allocator: one bounded total re-divided across running
    /// claimants (the readers of its write signal); grants shrink instantly
    /// and grow slowly.
    Budget { total: f32, period_ms: u64 },
    /// Pool member holding a client session: member `k` is in session while
    /// the dial is at least `k+1`; busy for the session's duration.
    Session { busy_ms: u64 },
    /// Fixed-rate loop that holds last-good output instead of dying when its
    /// input goes quiet.
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
    },
    /// Data-deps demo: repeatedly holds a lease on a `Leased` signal.
    LeaseUser { lease: String, hold_ms: u64 },
    /// Feeds the simulated hardware watchdog; when this task stops feeding,
    /// the watchdog bites and the MCU reboots.
    Watchdog { feed_ms: u64 },
    /// Beats slowly and waits for shutdown.
    Idle,
}

/// The resolved runtime behavior (signal names bound to runtime objects).
pub enum Behavior {
    Periodic {
        period_ms: u64,
        scaled: bool,
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
        total: f32,
        period_ms: u64,
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
    },
    LeaseUser {
        leased: &'static Leased<AtomicU32>,
        hold_ms: u64,
    },
    Watchdog {
        feed_ms: u64,
    },
    Idle,
}

fn fault_of(rt: &NodeRt) -> u8 {
    rt.fault.load(Ordering::Relaxed)
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

fn stalled(rt: &NodeRt) -> bool {
    fault_of(rt) == fault::STALL
}

fn beat_unless_stalled(rt: &'static NodeRt) {
    if !stalled(rt) {
        rt.node.beat();
    }
}

/// True when the body should end early (injected abrupt exit).
fn exit_requested(rt: &'static NodeRt) -> bool {
    if fault_of(rt) == fault::EXIT {
        log::error!("{}: worker crashed (injected)", rt.node.name());
        return true;
    }
    false
}

/// Wait for an injected wedge fault. Event-driven, never a timer poll: a
/// 50 ms flag poll here would wake every task — a parked Pause task included
/// — and the task panel would show phantom polls on tasks that are asleep.
async fn wait_wedge(rt: &'static NodeRt) {
    loop {
        if fault_of(rt) == fault::WEDGE {
            log::warn!("{}: wedged — will not ack shutdown", rt.node.name());
            return;
        }
        rt.wedge_wake.wait().await;
    }
}

/// True while any consumer of `sig` is running — a queue drains only as fast
/// as something downstream takes from it.
fn readers_running(sig: &'static SignalRt) -> bool {
    let mut any_reader = false;
    for rt in registry::nodes() {
        if rt.reads.iter().any(|s| std::ptr::eq(*s, sig)) {
            any_reader = true;
            if rt.node.is_running() {
                return true;
            }
        }
    }
    !any_reader
}

/// This pool member's index (`NAME#k` -> `k`); 0 for plain nodes.
fn member_index(rt: &NodeRt) -> u32 {
    rt.model
        .name
        .rsplit_once('#')
        .and_then(|(_, k)| k.parse().ok())
        .unwrap_or(0)
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
/// cancellable. The wedge fault escapes both wrappers so a stop runs into
/// `ShutdownTimeout` for real.
pub async fn run(rt: &'static NodeRt) {
    rt.node.adopt_current().await;
    let work = async {
        prologue(rt);
        if rt.node.mode() == Mode::Pause {
            // Parked-and-resumed runs the body again, in place; a body that
            // returns on its own (injected exit) truly ends the task, like
            // any crashed worker.
            while rt.node.run_pausable(body(rt)).await.is_err() {}
            epilogue(rt).await;
            rt.node.mark_exited();
        } else {
            let done = rt.node.run_cancellable(body(rt)).await;
            // The body future is dropped at whatever await it sat on, so a
            // busy mark or readiness asserted mid-cycle would outlive the
            // task: a bound-stopped session would report busy forever, and a
            // stopped module would still read ready to its bound dependents.
            // Clear both here, the one place every stop path funnels through.
            rt.node.mark_idle();
            rt.node.clear_ready();
            epilogue(rt).await;
            match done {
                Ok(()) => rt.node.mark_exited(),
                // Stop requested: ordered teardown first, ack after.
                Err(_aborted) => rt.node.ack_dropped(),
            }
        }
    };
    match select(work, wait_wedge(rt)).await {
        Either::First(()) => {}
        Either::Second(()) => core::future::pending::<()>().await,
    }
}

async fn body(rt: &'static NodeRt) {
    let node = rt.node;
    match &rt.behavior {
        Behavior::Periodic { period_ms, scaled } => {
            assert_ready(rt);
            node.report_status("sampling");
            // Scaled emission: the dial is activity, and a still sensor has
            // nothing to report — it stays alive (beats) but writes nothing.
            let mut acc = 0.0f32;
            loop {
                Timer::after_millis(*period_ms).await;
                if exit_requested(rt) {
                    return;
                }
                if stalled(rt) {
                    continue;
                }
                rt.node.beat();
                if *scaled {
                    acc += input_f32(rt);
                    if acc >= 1.0 {
                        acc -= 1.0;
                        write_all(rt);
                    }
                } else {
                    write_all(rt);
                }
            }
        }
        Behavior::Pipeline {
            work_ms,
            accumulate,
        } => {
            assert_ready(rt);
            node.report_status("processing");
            loop {
                Timer::after_millis(*work_ms).await;
                if exit_requested(rt) {
                    return;
                }
                if stalled(rt) {
                    continue;
                }
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
                if exit_requested(rt) {
                    return;
                }
                if stalled(rt) {
                    continue;
                }
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
                if exit_requested(rt) {
                    return;
                }
                if stalled(rt) {
                    continue;
                }
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
            loop {
                Timer::after_millis(*drain_ms).await;
                if exit_requested(rt) {
                    return;
                }
                if stalled(rt) {
                    continue;
                }
                rt.node.beat();
                // Admit arrivals one by one, applying the overflow policy.
                let mut overflowed = false;
                for (s, mark) in rt.reads.iter().zip(&rt.read_marks) {
                    let w = s.writes.load(Ordering::Relaxed);
                    while mark.load(Ordering::Relaxed) != w {
                        if depth < *capacity {
                            depth += 1;
                        } else {
                            overflowed = true;
                            match policy {
                                // Full: never block the producer; the arrival
                                // is refused and counted.
                                OverflowPolicy::Reject => lost += 1,
                                // Full: stop consuming; the backlog is the
                                // producer's problem now.
                                OverflowPolicy::Backpressure => break,
                                // Full: the arrival enters, the oldest is lost.
                                OverflowPolicy::DropOldest => lost += 1,
                            }
                        }
                        mark.fetch_add(1, Ordering::Relaxed);
                        s.reads.fetch_add(1, Ordering::Relaxed);
                    }
                }
                // Drain toward whoever consumes our output; a down consumer
                // leaves the backlog standing.
                if depth > 0
                    && let Some(out) = depth_sig
                    && readers_running(out)
                {
                    depth -= 1;
                    write_all(rt);
                }
                set_depth(depth);
                // The full-state wording follows the policy — a ring at
                // capacity is operating as designed, not overflowing — and
                // keys on this cycle actually hitting the cap: at steady
                // state the drain leaves depth one under it.
                node.report_status(if overflowed {
                    match policy {
                        OverflowPolicy::Reject => "overflowing: rejecting",
                        OverflowPolicy::Backpressure => "full: back-pressuring",
                        OverflowPolicy::DropOldest => "ring full: dropping oldest",
                    }
                } else if lost > 0 || depth * 10 >= *capacity * 8 {
                    "backlog"
                } else {
                    "staging"
                });
            }
        }
        Behavior::Budget { total, period_ms } => {
            assert_ready(rt);
            node.report_status("allocating");
            let mut grant = 0.0f32;
            loop {
                Timer::after_millis(*period_ms).await;
                if exit_requested(rt) {
                    return;
                }
                if stalled(rt) {
                    continue;
                }
                rt.node.beat();
                // The allocator watches its declared inputs (site load, a
                // derate) — consume them so the dataflow reads as live.
                let _ = read_new(rt);
                // Claimants: running readers of the budget signal. The fair
                // share shrinks the instant one joins; it grows back slowly.
                let claimants = rt
                    .writes
                    .first()
                    .map(|out| {
                        registry::nodes()
                            .iter()
                            .filter(|n| {
                                n.reads.iter().any(|s| std::ptr::eq(*s, *out))
                                    && n.node.is_running()
                            })
                            .count() as f32
                    })
                    .unwrap_or(0.0)
                    .max(1.0);
                let target = *total / claimants;
                if target < grant {
                    grant = target; // shed load immediately
                } else {
                    grant += ((target - grant) * 0.25).min(*total * 0.05); // ramp back slowly
                }
                emit(rt, grant);
            }
        }
        Behavior::Session { busy_ms } => {
            node.set_ready();
            let k = member_index(rt);
            let mut open = false;
            loop {
                Timer::after_millis(200).await;
                if exit_requested(rt) {
                    return;
                }
                if stalled(rt) {
                    continue;
                }
                rt.node.beat();
                // Member k serves the (k+1)-th concurrent client; the dial is
                // the client count, the pool max is the real session cap.
                let want = input_f32(rt) >= (k + 1) as f32;
                if want != open {
                    open = want;
                    if open {
                        node.mark_busy();
                        node.report_status("session open");
                    } else {
                        node.mark_idle();
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
            loop {
                Timer::after_millis(*period_ms).await;
                if exit_requested(rt) {
                    return;
                }
                if stalled(rt) {
                    continue;
                }
                rt.node.beat();
                // A fixed-rate loop never skips an output: with fresh input
                // it tracks, without it holds last-good instead of dying.
                if read_new(rt) {
                    last_good = rt
                        .reads
                        .first()
                        .map(|s| f32::from_bits(s.value.load(Ordering::Relaxed)))
                        .unwrap_or(last_good);
                    if !fresh {
                        fresh = true;
                        node.report_status("tracking");
                    }
                } else if fresh {
                    fresh = false;
                    node.report_status("holding last-good");
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
                if exit_requested(rt) {
                    return;
                }
                if stalled(rt) {
                    continue;
                }
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
                if exit_requested(rt) {
                    return;
                }
                beat_unless_stalled(rt);
                // An up link transmits: it consumes its declared reads
                // (upload frames); a down link lets them back up.
                if up && !stalled(rt) {
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
        } => {
            if *delay_ms > 0 {
                node.report_status("warming up");
                Timer::after_millis(*delay_ms).await;
            }
            node.report_status("waiting for producer");
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
                if exit_requested(rt) {
                    return;
                }
                if stalled(rt) {
                    continue;
                }
                rt.node.beat();
                // A gated consumer that also declares writes: is a gated
                // pipeline stage (read through the gate, produce onward);
                // both stall while the producer emits nothing new.
                if read_new(rt) {
                    write_all(rt);
                }
            }
        }
        Behavior::LeaseUser { leased, hold_ms } => {
            node.set_ready();
            // One stable status while cycling: report_status logs every
            // change, and flipping holding/released per lease floods the log
            // pane with churn. Only the drained transition is news.
            node.report_status("cycling leases");
            loop {
                if exit_requested(rt) {
                    return;
                }
                beat_unless_stalled(rt);
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
                if exit_requested(rt) {
                    return;
                }
                if stalled(rt) {
                    continue;
                }
                rt.node.beat();
                feed();
            }
        }
        Behavior::Idle => {
            node.set_ready();
            loop {
                Timer::after_millis(1000).await;
                if exit_requested(rt) {
                    return;
                }
                beat_unless_stalled(rt);
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
        }
    } else if !model.reads.is_empty() {
        BehaviorSpec::GatedConsumer {
            open: model.reads[0].name.clone(),
            period_ms: 200,
            delay_ms: 0,
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
