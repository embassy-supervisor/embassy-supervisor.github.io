//! Native guard for the two node policies the page drives: an `enabled_if:`
//! start condition set before the run, and a `restart_budget:` that refuses
//! a restart storm. One `#[test]` fn: the builder's statics fill once per
//! process.

use std::time::Duration as StdDuration;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor::{ControlOp, try_request_control};
use embassy_supervisor_playground::{build, events, health, parse, registry};
use embassy_time::{Duration, MockDriver};

const DSL: &str = r#"
supervisor_graph! {
    node SENSOR = Terminate, task: sensor_task;
    node RADIO = Terminate, deps: [SENSOR], task: radio_task,
        enabled_if: crate::settings::radio_on;
    node NET = Terminate, task: net_task,
        restart_budget: 2 in 10000;
}
"#;

const BEHAVIORS: &str = r#"{
    "SENSOR": { "kind": "periodic", "period_ms": 50 },
    "RADIO": { "kind": "periodic", "period_ms": 50 },
    "NET": { "kind": "periodic", "period_ms": 50 }
}"#;

#[embassy_executor::task]
async fn health_driver() {
    health::drive().await
}

/// The page's loop: report the fault and re-enter.
#[embassy_executor::task]
async fn supervise(spawner: Spawner) {
    loop {
        let fault = build::drive_supervisor(&spawner).await;
        events::push_fault(&fault);
    }
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

fn by_name(name: &str) -> &'static registry::NodeRt {
    registry::nodes()
        .iter()
        .find(|rt| rt.model.name == name)
        .unwrap_or_else(|| panic!("no node {name}"))
}

#[test]
fn start_conditions_and_restart_budgets_run() {
    let mut outcome = parse::parse(DSL);
    assert!(
        outcome.ok,
        "parse errors: {:?}",
        outcome.errors.iter().map(|e| &e.msg).collect::<Vec<_>>()
    );
    assert!(
        outcome.badges.is_empty(),
        "both clauses run, nothing is degraded: {:?}",
        outcome.badges.iter().map(|b| &b.clause).collect::<Vec<_>>()
    );
    let model = outcome.model.take().unwrap();
    let radio = model.nodes.iter().find(|n| n.name == "RADIO").unwrap();
    assert_eq!(radio.enabled_if.as_deref(), Some("crate::settings::radio_on"));

    // The page sets conditions on the fresh instance before the run.
    registry::set_condition("crate::settings::radio_on", false);
    build::build(model, BEHAVIORS).expect("build");
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|sp| {
            sp.spawn(health_driver().unwrap());
            sp.spawn(supervise(sp).unwrap());
        });
    });

    let sensor = by_name("SENSOR").node;
    let radio = by_name("RADIO").node;
    let net = by_name("NET").node;
    assert!(
        settle(|| sensor.is_running() && net.is_running(), 2000),
        "bring-up"
    );
    assert!(
        radio.is_disabled() && !radio.is_running(),
        "a false start condition keeps the node down and latches it disabled"
    );

    // Two restarts inside the window are allowed; the third is refused,
    // nothing is stopped, and the fault reaches the page.
    let mut faults: Vec<String> = Vec::new();
    for round in 1..=3 {
        let epoch = net.epoch();
        try_request_control(net, ControlOp::Restart).unwrap();
        settle(
            || {
                faults.extend(events::snapshot().faults);
                net.epoch() != epoch || !faults.is_empty()
            },
            1000,
        );
        if round < 3 {
            assert!(faults.is_empty(), "restart {round} refused: {faults:?}");
            assert!(settle(|| net.is_running(), 1000), "restart {round}");
        }
    }
    assert!(
        faults.iter().any(|f| f.contains("restart budget")),
        "the third restart in the window is a restart storm: {faults:?}"
    );
    assert!(net.is_running(), "a refused restart stops nothing");
}
