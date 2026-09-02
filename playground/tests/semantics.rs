//! Pins for the two places the interpreter used to diverge from the crate,
//! plus the `Pause` parking contract — exactly where a regression would go
//! unnoticed.
//!
//! One `#[test]` fn: the builder's statics fill once per process.

use std::sync::atomic::Ordering;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor::{ControlOp, try_request_control};
use embassy_supervisor_playground::{build, parse, registry};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, MockDriver};
use std::time::Duration as StdDuration;

const DSL: &str = r#"
supervisor_graph! {
    pool W = [Terminate, OnDemand, OnDemand], task: w_task,
        policy: DeferredShrink::new(Duration::from_secs(2)), min: 1, max: 3;
    node DISPATCH = Terminate, deps: [W ready], task: d_task;
    node LENDER = Terminate, task: l_task, resources: [PORT: Uart],
        slot_timeout: 200;
    node BURNER = Terminate, task: c_task, resources: [RUNNER: consume Wifi],
        slot_timeout: 200;
    node PARKED = Pause, task: p_task, resources: [BUF: Ram], writes: [TICKS];
}
"#;

const BEHAVIORS: &str = r#"{
    "W": { "kind": "idle" },
    "DISPATCH": { "kind": "idle" },
    "LENDER": { "kind": "idle" },
    "BURNER": { "kind": "idle" },
    "PARKED": { "kind": "periodic", "period_ms": 50 }
}"#;

/// stop_node uses embassy_time internally, so it must run on the embassy
/// executor; this mailbox drives it from the test thread.
static STOP: Channel<CriticalSectionRawMutex, &'static embassy_supervisor::TaskNode, 2> =
    Channel::new();

#[embassy_executor::task]
async fn stopper() {
    loop {
        let node = STOP.receive().await;
        let sup = build::built().unwrap().sup;
        sup.stop_node(node).await.expect("stop_node");
    }
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

#[test]
fn interpreter_matches_the_crate() {
    let mut outcome = parse::parse(DSL);
    assert!(
        outcome.ok,
        "parse errors: {:?}",
        outcome.errors.iter().map(|e| &e.msg).collect::<Vec<_>>()
    );
    let built = build::build(outcome.model.take().unwrap(), BEHAVIORS).expect("build");
    assert!(built.named_executors.is_empty());

    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|sp| {
            sp.spawn(stopper().unwrap());
            sp.spawn(supervise(sp).unwrap());
        });
    });

    // ── Finding 1: `deps: [POOL]` resolves to the floor member only. ──────
    // DISPATCH waits on `W ready`. Under the old every-member expansion its
    // ready gate includes W#1 and W#2, which are OnDemand and down at
    // min: 1 — DISPATCH could never start. Floor-member semantics start it
    // once W#0 alone is ready.
    assert!(
        settle(|| by_name("DISPATCH").node.is_running(), 3000),
        "deps: [POOL] must gate on the floor member only"
    );
    assert!(!by_name("W#1").node.is_running(), "pool stays at min=1");
    assert!(!by_name("W#2").node.is_running(), "pool stays at min=1");

    // ── Finding 2: the three resource kinds behave differently. ───────────
    let port = registry::resources()
        .iter()
        .find(|r| r.name == "PORT")
        .unwrap();
    let runner = registry::resources()
        .iter()
        .find(|r| r.name == "RUNNER")
        .unwrap();
    assert_eq!(port.kind, registry::ResKind::Lend);
    assert_eq!(runner.kind, registry::ResKind::Consume);

    // Lend: taken while the worker runs (the UI shows who holds it) ...
    assert!(settle(|| by_name("LENDER").node.is_running(), 2000));
    assert!(
        !port.slot.is_filled(),
        "a lent value is out while the taker runs"
    );
    assert_eq!(
        port.held_by.load(Ordering::Relaxed),
        by_name("LENDER").idx,
        "held_by names the taker"
    );
    // ... restored on exit, so a respawn re-takes the same instance.
    by_name("LENDER")
        .fault
        .store(registry::fault::EXIT, Ordering::Relaxed);
    assert!(settle(|| by_name("LENDER").node.has_exited(), 3000));
    assert!(port.slot.is_filled(), "a lent value comes back at exit");
    by_name("LENDER")
        .fault
        .store(registry::fault::NONE, Ordering::Relaxed);
    try_request_control(by_name("LENDER").node, ControlOp::Restart).unwrap();
    assert!(
        settle(
            || by_name("LENDER").node.is_running() && !port.slot.is_filled(),
            3000
        ),
        "the respawn re-takes the restored instance"
    );

    // Consume: the slot stays empty after exit; the respawn fail-closes
    // until something re-provides.
    assert!(settle(|| by_name("BURNER").node.is_running(), 2000));
    assert!(!runner.slot.is_filled(), "consumed at spawn");
    by_name("BURNER")
        .fault
        .store(registry::fault::EXIT, Ordering::Relaxed);
    assert!(settle(|| by_name("BURNER").node.has_exited(), 3000));
    assert!(
        !runner.slot.is_filled(),
        "a consumed value does not come back"
    );
    by_name("BURNER")
        .fault
        .store(registry::fault::NONE, Ordering::Relaxed);
    try_request_control(by_name("BURNER").node, ControlOp::Restart).unwrap();
    advance_ms(1000); // past the 200 ms slot_timeout
    assert!(
        !by_name("BURNER").node.is_running(),
        "a respawn with an empty consume slot fail-closes"
    );
    // Re-provide (the wasm page does this through resource_command).
    runner.slot.provide(1);
    try_request_control(by_name("BURNER").node, ControlOp::Restart).unwrap();
    assert!(
        settle(|| by_name("BURNER").node.is_running(), 3000),
        "re-providing unblocks the respawn"
    );

    // ── Pause parks rather than exits. ────────────────────────────────────
    let parked = by_name("PARKED");
    let buf = registry::resources()
        .iter()
        .find(|r| r.name == "BUF")
        .unwrap();
    assert!(settle(|| parked.node.is_running(), 2000));
    assert!(
        !buf.slot.is_filled(),
        "the Pause node took its lend resource"
    );
    let ticks = registry::signals()
        .iter()
        .find(|s| s.name == "TICKS")
        .unwrap();
    let sup = build::built().unwrap().sup;
    STOP.try_send(parked.node).unwrap();
    assert!(
        settle(|| !parked.node.is_running(), 3000),
        "stop parks the node"
    );
    assert!(
        !parked.node.has_exited(),
        "a parked Pause node has not exited"
    );
    assert!(
        parked.node.shutdown_requested(),
        "the park is an acked stop"
    );
    // The park keeps the task instance alive: what it took stays taken —
    // the property that distinguishes a resume from a respawn.
    assert!(
        !buf.slot.is_filled(),
        "a parked task still holds what it took"
    );
    let w = ticks.writes.load(Ordering::Relaxed);
    advance_ms(500);
    assert_eq!(
        ticks.writes.load(Ordering::Relaxed),
        w,
        "a parked node is silent"
    );
    sup.resume_node(parked.node);
    assert!(
        settle(|| ticks.writes.load(Ordering::Relaxed) > w, 3000),
        "a resumed node picks up in place"
    );
    assert!(
        !buf.slot.is_filled(),
        "the resumed body reuses the held resource"
    );
}
