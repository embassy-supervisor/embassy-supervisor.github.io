//! Native CI guard for the interpreter: parse a gateway-style DSL, build the
//! runtime graph, and drive it on embassy-executor's std platform with the
//! mock clock — the same code path the wasm page runs, minus the browser.

use std::sync::atomic::Ordering;
use std::time::Duration as StdDuration;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor::{ControlOp, try_request_control};
use embassy_supervisor_playground::{build, parse, registry};
use embassy_time::{Duration, MockDriver};

const DSL: &str = r#"
supervisor_graph! {
    executor HIGH;
    node WATCHDOG = Terminate, task: watchdog_task, executor: HIGH;
    node SENSOR_BUS = Pause, task: sensor_task, beat_timeout: 500,
        writes: [signals::RAW_SAMPLES observed beat];
    node FILTER = Terminate, deps: [SENSOR_BUS], task: filter_task,
        reads: [signals::RAW_SAMPLES], writes: [signals::FILTERED];
    node NET = Terminate, task: net_task, provides: [NET_STACK], slot_timeout: 5000;
    node HTTP_API = Terminate, deps: [NET ready], task: http_task,
        resources: [NET_STACK: shared Stack], slot_timeout: 5000;
    pool MQTT = [Terminate, OnDemand, OnDemand], deps: [NET ready], task: mqtt_task,
        policy: DeferredShrink::new(Duration::from_secs(2)), min: 1, max: 3, slot_timeout: 5000,
        reads: [signals::FILTERED];
    node OTA = Terminate, deps: [NET ready], task: ota_task, disabled, slot_timeout: 5000;
}
"#;

const BEHAVIORS: &str = r#"{
    "SENSOR_BUS": { "kind": "periodic", "period_ms": 100 },
    "FILTER": { "kind": "pipeline", "work_ms": 150 },
    "NET": { "kind": "provider", "startup_ms": 300 },
    "WATCHDOG": { "kind": "idle" },
    "HTTP_API": { "kind": "idle" },
    "OTA": { "kind": "oneshot", "run_ms": 400 }
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

/// Advance virtual time with no condition to meet.
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

#[test]
fn gateway_runs() {
    let mut outcome = parse::parse(DSL);
    assert!(
        outcome.ok,
        "parse errors: {:?}",
        outcome.errors.iter().map(|e| &e.msg).collect::<Vec<_>>()
    );
    let model = outcome.model.take().unwrap();
    let built = build::build(model, BEHAVIORS).expect("build");

    for (_, slot) in &built.named_executors {
        let slot = *slot;
        std::thread::spawn(move || {
            let executor = Box::leak(Box::new(Executor::new()));
            executor.run(|sp| slot.set(sp.make_send()));
        });
    }
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|sp| sp.spawn(supervise(sp).unwrap()));
    });

    // Bring-up: provider NET turns ready after 300 virtual ms; everything
    // gated on it follows; OTA stays down (declared disabled).
    assert!(
        settle(
            || {
                by_name("WATCHDOG").node.is_running()
                    && by_name("SENSOR_BUS").node.is_running()
                    && by_name("FILTER").node.is_running()
                    && by_name("NET").node.is_ready()
                    && by_name("HTTP_API").node.is_running()
                    && by_name("MQTT#0").node.is_running()
            },
            3000
        ),
        "bring-up did not settle"
    );
    assert!(
        !by_name("OTA").node.is_running(),
        "disabled OTA must stay down"
    );
    assert!(!by_name("MQTT#1").node.is_running(), "pool starts at min=1");

    // The provider actually filled its slot; a shared handle stays filled
    // while its takers run.
    let net_stack = registry::resources()
        .iter()
        .find(|r| r.name == "NET_STACK")
        .unwrap();
    assert!(net_stack.slot.is_filled());
    assert_eq!(net_stack.kind, registry::ResKind::Shared);

    // Dataflow: the sensor writes, the filter reads and writes onward.
    let raw = registry::signals()
        .iter()
        .find(|s| s.name == "signals::RAW_SAMPLES")
        .unwrap();
    let before = raw.writes.load(Ordering::Relaxed);
    assert!(settle(
        || raw.writes.load(Ordering::Relaxed) > before + 3,
        1000
    ));
    let filtered = registry::signals()
        .iter()
        .find(|s| s.name == "signals::FILTERED")
        .unwrap();
    assert!(filtered.writes.load(Ordering::Relaxed) > 0);
    // Pool members only consume while serving jobs: turn the load dial up.
    for m in ["MQTT#0", "MQTT#1", "MQTT#2"] {
        by_name(m).input.store(1.0f32.to_bits(), Ordering::Relaxed);
    }
    assert!(settle(|| filtered.reads.load(Ordering::Relaxed) > 0, 3000));

    // A crashed producer starves its consumers: reads must freeze with the
    // writes instead of counting phantom consumption of a dead signal.
    by_name("SENSOR_BUS")
        .fault
        .store(registry::fault::EXIT, Ordering::Relaxed);
    assert!(
        settle(|| by_name("SENSOR_BUS").node.has_exited(), 2000),
        "crashed sensor should exit"
    );
    // Let in-flight consumption settle, then hold still for a while.
    advance_ms(500);
    let raw_r = raw.reads.load(Ordering::Relaxed);
    let filtered_w = filtered.writes.load(Ordering::Relaxed);
    advance_ms(1500);
    assert_eq!(
        raw.reads.load(Ordering::Relaxed),
        raw_r,
        "no new RAW_SAMPLES reads after the crash"
    );
    assert_eq!(
        filtered.writes.load(Ordering::Relaxed),
        filtered_w,
        "a starved FILTER stops producing"
    );
    assert!(
        by_name("FILTER").node.is_running(),
        "starved consumers stay alive"
    );
    // Restart the sensor; the pipeline picks back up.
    by_name("SENSOR_BUS")
        .fault
        .store(registry::fault::NONE, Ordering::Relaxed);
    try_request_control(by_name("SENSOR_BUS").node, ControlOp::Restart).unwrap();
    assert!(
        settle(|| raw.reads.load(Ordering::Relaxed) > raw_r, 3000),
        "reads resume once the producer is back"
    );

    // Control: activate the disabled OTA; its oneshot behavior runs and exits.
    try_request_control(by_name("OTA").node, ControlOp::Activate).unwrap();
    assert!(
        settle(|| by_name("OTA").node.is_running(), 2000),
        "OTA should start on activate"
    );
    assert!(
        settle(|| by_name("OTA").node.has_exited(), 2000),
        "oneshot OTA should exit"
    );

    // Fault injection: stall the sensor; the liveness monitor reports Stale.
    by_name("SENSOR_BUS")
        .fault
        .store(registry::fault::STALL, Ordering::Relaxed);
    assert!(
        settle(
            || by_name("SENSOR_BUS").node.ticks_since_beat() > 500_000,
            2000
        ),
        "stalled sensor should stop beating"
    );
    by_name("SENSOR_BUS")
        .fault
        .store(registry::fault::NONE, Ordering::Relaxed);

    // Restart bumps the epoch and comes back.
    let epoch = by_name("FILTER").node.epoch();
    try_request_control(by_name("FILTER").node, ControlOp::Restart).unwrap();
    assert!(
        settle(
            || by_name("FILTER").node.epoch() > epoch && by_name("FILTER").node.is_running(),
            3000
        ),
        "FILTER should restart with a higher epoch"
    );
}
