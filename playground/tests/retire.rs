//! Native guard for the refcounted stop: a demand-started producer with a
//! `retire_ms` cooldown withdraws once its last reader has left, through the
//! real `TaskNode::retire` (clear readiness, then request its own
//! deactivate), and the next gated read demand-starts it again.

use std::time::Duration as StdDuration;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor::{ControlOp, try_request_control};
use embassy_supervisor_playground::registry::Gate;
use embassy_supervisor_playground::{build, parse, registry};
use embassy_time::{Duration, MockDriver};

const DSL: &str = r#"
supervisor_graph! {
    node SENSOR_HUB = Terminate, task: hub_task, disabled,
        writes: [TELEMETRY observed];
    node CLOUD_SYNC = Terminate, task: cloud_task,
        reads: [TELEMETRY];
    node DASHBOARD = Terminate, task: dash_task,
        reads: [TELEMETRY];
}
"#;

const BEHAVIORS: &str = r#"{
    "SENSOR_HUB": { "kind": "periodic", "period_ms": 100, "retire_ms": 1000 },
    "CLOUD_SYNC": { "kind": "gated_consumer", "open": "TELEMETRY", "period_ms": 100 },
    "DASHBOARD": { "kind": "gated_consumer", "open": "TELEMETRY", "period_ms": 100, "delay_ms": 300 }
}"#;

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
        .unwrap()
}

fn openers() -> u32 {
    let sig = registry::signals()
        .iter()
        .find(|s| s.name == "TELEMETRY")
        .unwrap();
    match sig.gate {
        Gate::Backed(b) => b.openers(),
        _ => panic!("TELEMETRY should run Backed"),
    }
}

#[test]
fn producer_retires_after_its_last_reader() {
    let mut outcome = parse::parse(DSL);
    assert!(
        outcome.ok,
        "parse errors: {:?}",
        outcome.errors.iter().map(|e| &e.msg).collect::<Vec<_>>()
    );
    build::build(outcome.model.take().unwrap(), BEHAVIORS).expect("build");

    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|sp| sp.spawn(supervise(sp).unwrap()));
    });

    let hub = by_name("SENSOR_HUB");
    let cloud = by_name("CLOUD_SYNC");
    let dash = by_name("DASHBOARD");

    // Two readers open the gate: the producer is demand-started once, and
    // the guard count is what the readers hold.
    assert!(
        settle(
            || hub.node.is_running() && cloud.node.is_ready() && dash.node.is_ready(),
            5000
        ),
        "demand-start did not settle"
    );
    assert_eq!(openers(), 2, "both readers hold an Open guard");
    assert_eq!(hub.node.epoch(), 1);

    // One reader leaves: still watched, the producer stays.
    try_request_control(dash.node, ControlOp::Deactivate).expect("control queue");
    assert!(
        settle(|| openers() == 1, 3000),
        "a dropped guard leaves the count"
    );
    advance_ms(1500);
    assert!(hub.node.is_running(), "a watched producer never retires");

    // The last reader leaves: after the cooldown the producer retires
    // itself — readiness withdrawn, its own deactivate requested and
    // landed, the `disabled` latch set for the next demand-start.
    try_request_control(cloud.node, ControlOp::Deactivate).expect("control queue");
    assert!(settle(|| openers() == 0, 3000), "the gate is unwatched");
    advance_ms(500);
    assert!(hub.node.is_running(), "the cooldown has not elapsed yet");
    assert!(
        settle(|| !hub.node.is_running() && hub.node.is_disabled(), 3000),
        "the producer retires once unwatched for the cooldown"
    );

    // A reader comes back: its open demand-starts the producer again.
    try_request_control(cloud.node, ControlOp::Activate).expect("control queue");
    assert!(
        settle(|| hub.node.is_running() && cloud.node.is_ready(), 5000),
        "the next open demand-starts the retired producer"
    );
    assert_eq!(hub.node.epoch(), 2, "a fresh instance");
    assert_eq!(openers(), 1);
}
