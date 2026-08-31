//! S4 robot cell controller: the STO bound cascade (a stalled safety channel
//! withdraws readiness, safety IO stops, the limit enforcer and servos
//! follow), and segment starvation degrading to hold-last-good rather than a
//! freeze.

use std::sync::atomic::Ordering;
use std::time::Duration as StdDuration;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor::{ControlOp, try_request_control};
use embassy_supervisor_playground::{build, health, parse, registry};
use embassy_time::{Duration, MockDriver};

const DSL: &str = r#"
supervisor_graph! {
    executor MOTION;
    executor SAFETY;
    node PENDANT = Terminate, task: crate::hmi::pendant_task,
        writes: [signals::PROGRAM observed];
    node IK_SOLVER = Terminate, deps: [PENDANT],
        task: crate::plan::ik_task,
        reads: [signals::PROGRAM], writes: [signals::JOINTS observed];
    node SEGMENT_Q = Terminate, deps: [IK_SOLVER],
        task: crate::plan::segment_queue_task,
        reads: [signals::JOINTS], writes: [signals::SEGMENTS observed];
    node ECAT_MASTER = Terminate, executor: MOTION,
        task: crate::ecat::master_task,
        resources: [ECAT_IF: consume crate::ecat::EcatIf],
        provides: [PDO], slot_timeout: 3000;
    node COARSE_INTERP = Terminate, executor: MOTION,
        deps: [ECAT_MASTER ready, SEGMENT_Q],
        task: crate::motion::coarse_task,
        resources: [PDO: shared crate::ecat::Pdo], slot_timeout: 4000,
        reads: [signals::SEGMENTS], writes: [signals::SETPOINTS observed];
    node LIMIT_ENFORCER = Terminate, executor: MOTION,
        deps: [COARSE_INTERP, SAFE_IO ready bound],
        task: crate::motion::limits_task, slot_timeout: 4000,
        reads: [signals::SETPOINTS], writes: [signals::SAFE_SET observed];
    node SERVO_X = Terminate, executor: MOTION,
        deps: [LIMIT_ENFORCER ready bound],
        task: crate::motion::servo_task, slot_timeout: 4000,
        ready_on_write, beat_timeout: 600,
        reads: [signals::SAFE_SET], writes: [signals::ACTUAL observed beat];
    node SAFE_CH_A = Terminate, executor: SAFETY,
        task: crate::safety::channel_task,
        beat_timeout: 300, writes: [signals::CH_A observed beat];
    node SAFE_CH_B = Terminate, executor: SAFETY,
        task: crate::safety::channel_task,
        beat_timeout: 300, writes: [signals::CH_B observed beat];
    node SAFE_IO = Terminate, executor: SAFETY,
        deps: [SAFE_CH_A ready bound, SAFE_CH_B ready bound],
        task: crate::safety::sto_task, slot_timeout: 2000,
        reads: [signals::CH_A, signals::CH_B];
}
"#;

const BEHAVIORS: &str = r#"{
    "nodes": {
        "PENDANT": { "kind": "periodic", "period_ms": 500 },
        "IK_SOLVER": { "kind": "pipeline", "work_ms": 300 },
        "SEGMENT_Q": { "kind": "queue", "capacity": 8, "policy": "backpressure", "drain_ms": 700 },
        "ECAT_MASTER": { "kind": "provider", "startup_ms": 900 },
        "COARSE_INTERP": { "kind": "control_loop", "period_ms": 150 },
        "LIMIT_ENFORCER": { "kind": "control_loop", "period_ms": 150 },
        "SERVO_X": { "kind": "control_loop", "period_ms": 100 },
        "SAFE_CH_A": { "kind": "periodic", "period_ms": 150 },
        "SAFE_CH_B": { "kind": "periodic", "period_ms": 150 },
        "SAFE_IO": { "kind": "control_loop", "period_ms": 150 }
    },
    "escalation": {
        "SAFE_CH_A": "clear_ready",
        "SAFE_CH_B": "clear_ready"
    }
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

fn depth_of(name: &str) -> u32 {
    registry::signals()
        .iter()
        .find(|s| s.name == name)
        .unwrap()
        .depth
        .load(Ordering::Relaxed)
}

fn reads_of(name: &str) -> u32 {
    registry::signals()
        .iter()
        .find(|s| s.name == name)
        .unwrap()
        .reads
        .load(Ordering::Relaxed)
}

fn writes_of(name: &str) -> u32 {
    registry::signals()
        .iter()
        .find(|s| s.name == name)
        .unwrap()
        .writes
        .load(Ordering::Relaxed)
}

#[test]
fn sto_cascade_and_starvation() {
    let mut outcome = parse::parse(DSL);
    assert!(
        outcome.ok,
        "parse errors: {:?}",
        outcome.errors.iter().map(|e| &e.msg).collect::<Vec<_>>()
    );
    let built = build::build(outcome.model.take().unwrap(), BEHAVIORS).expect("build");

    for (_, slot) in &built.named_executors {
        let slot = *slot;
        std::thread::spawn(move || {
            let executor = Box::leak(Box::new(Executor::new()));
            executor.run(|sp| slot.set(sp.make_send()));
        });
    }
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|sp| {
            sp.spawn(health_driver().unwrap());
            sp.spawn(supervise(sp).unwrap());
        });
    });

    // Bring-up: the master reaches OP, the whole chain follows, the servo
    // asserts readiness through its first actual-position write.
    assert!(
        settle(
            || by_name("ECAT_MASTER").node.is_ready()
                && by_name("SERVO_X").node.is_running()
                && by_name("SERVO_X").node.is_ready(),
            8000
        ),
        "bring-up did not settle"
    );

    // Segment starvation: the program source dies (deactivate would take
    // its dependents with it — a crash starves them instead); segments
    // freeze but the motion tier holds last-good — setpoints keep flowing.
    by_name("PENDANT")
        .fault
        .store(registry::fault::EXIT, Ordering::Relaxed);
    assert!(settle(|| by_name("PENDANT").node.has_exited(), 3000));
    advance_ms(7000); // drain whatever queued during bring-up
    let seg = writes_of("signals::SEGMENTS");
    let set_before = writes_of("signals::SETPOINTS");
    advance_ms(1000);
    assert!(
        writes_of("signals::SEGMENTS") <= seg + 1,
        "segments starve with the planner stopped"
    );
    assert!(
        writes_of("signals::SETPOINTS") > set_before + 3,
        "the interpolator holds last-good instead of freezing"
    );
    by_name("PENDANT")
        .fault
        .store(registry::fault::NONE, Ordering::Relaxed);
    try_request_control(by_name("PENDANT").node, ControlOp::Restart).unwrap();
    assert!(settle(|| by_name("PENDANT").node.is_running(), 3000));

    // Full feed: joints arrive faster than the queue drains, so it fills to
    // capacity and back-pressures — the unconsumed backlog piles up at the
    // producer instead of segments being dropped.
    // The admit and the drain share one wake, so the depth observable
    // between cycles tops out one under capacity.
    assert!(
        settle(|| depth_of("signals::SEGMENTS") >= 7, 30000),
        "the segment queue fills to capacity"
    );
    let gap = writes_of("signals::JOINTS") - reads_of("signals::JOINTS");
    advance_ms(3000);
    assert!(
        depth_of("signals::SEGMENTS") <= 8,
        "back-pressure bounds the queue"
    );
    assert!(
        writes_of("signals::JOINTS") - reads_of("signals::JOINTS") > gap,
        "the producer-side backlog grows while the queue is full"
    );

    // STO: stall channel A. The policy withdraws its readiness; the cascade
    // runs the safety plane down through every bound edge.
    by_name("SAFE_CH_A")
        .fault
        .store(registry::fault::STALL, Ordering::Relaxed);
    assert!(
        settle(|| by_name("SAFE_IO").node.is_bound_stopped(), 6000),
        "safety IO follows the failed channel"
    );
    assert!(
        settle(|| by_name("LIMIT_ENFORCER").node.is_bound_stopped(), 6000),
        "the limit enforcer follows safety IO"
    );
    assert!(
        settle(|| by_name("SERVO_X").node.is_bound_stopped(), 6000),
        "STO: the servo stops"
    );

    // Recovery: clear the fault and restart the channel; the bound cascade
    // brings the plane back up in order.
    by_name("SAFE_CH_A")
        .fault
        .store(registry::fault::NONE, Ordering::Relaxed);
    try_request_control(by_name("SAFE_CH_A").node, ControlOp::Restart).unwrap();
    assert!(
        settle(
            || by_name("SAFE_IO").node.is_running() && by_name("SERVO_X").node.is_running(),
            10000
        ),
        "the plane recovers through the same bound edges"
    );
}
