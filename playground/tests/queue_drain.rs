//! Pins for the queue's service model and the control loop's hold rule: a
//! queue drains one item per running consumer per tick (nothing while none
//! runs, a backlog clearing once they outpace the producer), a scaled
//! periodic source bursts above 1.0, and a control loop learns its
//! producer's cadence instead of flapping on every gap.
//!
//! One `#[test]` fn: the builder's statics fill once per process.

use std::sync::atomic::Ordering;
use std::time::Duration as StdDuration;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor::Fault;
use embassy_supervisor_playground::{build, health, parse, registry};
use embassy_time::{Duration, MockDriver};

const DSL: &str = r#"
supervisor_graph! {
    node SRC = Terminate, task: src_task, writes: [IN];
    node Q = Terminate, deps: [SRC], task: q_task,
        reads: [IN], writes: [OUT observed];
    pool SINK = [OnDemand, OnDemand, OnDemand], task: sink_task,
        policy: DeferredShrink::new(Duration::from_secs(2)),
        min: 0, max: 3, reads: [OUT];
    node SLOW = Terminate, task: slow_task, writes: [TICK];
    node CTL = Terminate, deps: [SLOW], task: ctl_task, reads: [TICK];
}
"#;

const BEHAVIORS: &str = r#"{
    "SRC": { "kind": "periodic", "period_ms": 100 },
    "Q": { "kind": "queue", "capacity": 8, "policy": "drop_oldest", "drain_ms": 100 },
    "SINK": { "kind": "session", "busy_ms": 100 },
    "SLOW": { "kind": "periodic", "period_ms": 700, "scaled": true },
    "CTL": { "kind": "control_loop", "period_ms": 100 }
}"#;

#[embassy_executor::task]
async fn health_driver() {
    health::drive().await
}

#[embassy_executor::task]
async fn supervise(spawner: Spawner) {
    let fault = build::drive_supervisor(&spawner).await;
    panic!("supervisor faulted: {fault}");
}

fn settle(mut cond: impl FnMut() -> bool, max_virtual_ms: u64) -> bool {
    for _ in 0..max_virtual_ms {
        if cond() {
            return true;
        }
        MockDriver::get().advance(Duration::from_millis(1));
        std::thread::sleep(StdDuration::from_micros(300));
    }
    cond()
}

fn advance_ms(n: u64) {
    let mut left = n;
    settle(
        || {
            left = left.saturating_sub(1);
            left == 0
        },
        n + 1,
    );
}

fn by_name(name: &str) -> &'static registry::NodeRt {
    registry::nodes()
        .iter()
        .find(|rt| rt.model.name == name)
        .unwrap_or_else(|| panic!("no node {name}"))
}

fn signal(name: &str) -> &'static registry::SignalRt {
    registry::signals()
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no signal {name}"))
}

/// The widget path: a node's dial, or a pool's dial addressing every member.
fn dial(target: &str, value: f32) {
    let mut pool = false;
    for rt in registry::nodes() {
        if rt.model.name == target || rt.model.pool.as_deref() == Some(target) {
            rt.input.store(value.to_bits(), Ordering::Relaxed);
            pool |= rt.model.pool.is_some();
        }
    }
    if pool {
        embassy_supervisor::request_scale();
    }
}

/// Open sessions across the pool: the members actually taking from OUT
/// (the policy keeps an idle spare running, which serves nobody).
fn open_sinks() -> usize {
    registry::nodes()
        .iter()
        .filter(|rt| {
            rt.model.pool.as_deref() == Some("SINK") && rt.session_open.load(Ordering::Relaxed)
        })
        .count()
}

#[test]
fn drain_scales_with_consumers_and_hold_follows_cadence() {
    let mut outcome = parse::parse(DSL);
    assert!(
        outcome.ok,
        "parse errors: {:?}",
        outcome.errors.iter().map(|e| &e.msg).collect::<Vec<_>>()
    );
    let built = build::build(outcome.model.take().unwrap(), BEHAVIORS).expect("build");
    assert!(built.named_executors.is_empty());
    dial("SLOW", 1.0);

    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|sp| {
            sp.spawn(health_driver().unwrap());
            sp.spawn(supervise(sp).unwrap());
        });
    });

    assert!(
        settle(
            || by_name("Q").node.is_running() && by_name("CTL").node.is_running(),
            5000
        ),
        "bring-up did not settle"
    );

    // No session open (the pool's spare runs but serves nobody): the queue
    // admits at the producer's rate and drains nothing, so the backlog
    // stands at capacity.
    let out = signal("OUT");
    assert!(
        settle(|| out.depth.load(Ordering::Relaxed) >= 7, 3000),
        "with nobody downstream the queue fills"
    );
    assert_eq!(
        out.writes.load(Ordering::Relaxed),
        0,
        "nothing drains while no session is open"
    );

    // One consumer at exactly the producer's rate keeps pace but can never
    // work the backlog off.
    dial("SINK", 1.0);
    assert!(settle(|| open_sinks() == 1, 5000), "one session opens");
    advance_ms(3000);
    assert!(
        out.depth.load(Ordering::Relaxed) >= 6,
        "a single consumer at the arrival rate cannot clear the backlog"
    );

    // Three consumers outpace the producer: the drain is a rate set by the
    // readers, and the backlog clears.
    dial("SINK", 3.0);
    assert!(settle(|| open_sinks() == 3, 8000), "three sessions open");
    assert!(
        settle(|| out.depth.load(Ordering::Relaxed) == 0, 3000),
        "three consumers drain the backlog"
    );
    advance_ms(1000);
    assert!(
        out.depth.load(Ordering::Relaxed) <= 1,
        "and hold it near empty"
    );

    // A scaled source above 1.0 bursts: three emissions per period.
    let tick = signal("TICK");
    let before = tick.writes.load(Ordering::Relaxed);
    dial("SLOW", 3.0);
    advance_ms(2100);
    assert!(
        tick.writes.load(Ordering::Relaxed) >= before + 8,
        "a 3.0 dial emits three per period"
    );
    dial("SLOW", 1.0);

    // The control loop runs seven times faster than its producer. Once the
    // cadence is learned, the gaps between arrivals are not news: the
    // status stays put over three producer periods instead of flapping.
    advance_ms(3000);
    let ctl = by_name("CTL").node;
    assert_eq!(
        ctl.status(),
        Some("tracking"),
        "a slow producer still tracks"
    );
    assert!(
        !settle(|| ctl.status() != Some("tracking"), 2100),
        "no flapping on a producer slower than the loop"
    );

    // Kill the producer: the loop holds last-good once the gap exceeds twice
    // the cadence it learned, well under a second at these rates.
    by_name("SLOW").node.inject(Fault::Crash).unwrap();
    assert!(settle(|| by_name("SLOW").node.has_exited(), 3000));
    assert!(
        settle(|| ctl.status() == Some("holding last-good"), 3000),
        "a dead producer is reported"
    );
    assert!(ctl.is_running(), "holding, not dying");
}
