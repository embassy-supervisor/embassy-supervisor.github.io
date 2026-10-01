//! Activation faults must reach the page's report-and-reenter driver loop.
//! One test and one build: the interpreter's registry is process-global.

use std::sync::OnceLock;
use std::time::{Duration as StdDuration, Instant};

use embassy_executor::{Executor, Spawner};
use embassy_supervisor::{ControlOp, Fault, FaultKind, NodeFault, try_request_control};
use embassy_supervisor_playground::{build, events, parse, registry};
use embassy_time::{Duration, MockDriver};

const DSL: &str = r#"
supervisor_graph! {
    node RADIO = Terminate, task: radio_task,
        resources: [HW: consume Wifi], slot_timeout: 200;
    node CLIENT = Terminate, deps: [RADIO ready], task: client_task,
        disabled, slot_timeout: 200;
}
"#;

const BEHAVIORS: &str = r#"{
    "RADIO": { "kind": "idle" },
    "CLIENT": { "kind": "idle" }
}"#;

static FIRST_FAULT: OnceLock<NodeFault> = OnceLock::new();

/// Keep the same policy as the page: deliver the fault, then retry bring-up.
#[embassy_executor::task]
async fn supervise(spawner: Spawner) {
    loop {
        let fault = build::drive_supervisor(&spawner).await;
        events::push_fault(&fault);
        let _ = FIRST_FAULT.set(fault);
    }
}

fn settle(mut cond: impl FnMut() -> bool, max_virtual_ms: u64) -> bool {
    let deadline = Instant::now() + StdDuration::from_secs(10);
    for _ in 0..max_virtual_ms {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
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
fn activation_reports_consumed_resource_fault_and_recovers() {
    let mut outcome = parse::parse(DSL);
    assert!(
        outcome.ok,
        "parse errors: {:?}",
        outcome.errors.iter().map(|e| &e.msg).collect::<Vec<_>>()
    );
    build::build(outcome.model.take().unwrap(), BEHAVIORS).expect("build");

    std::thread::spawn(|| {
        Box::leak(Box::new(Executor::new())).run(|spawner| {
            spawner.spawn(supervise(spawner).unwrap());
        });
    });

    let radio = by_name("RADIO").node;
    let client = by_name("CLIENT").node;
    let hw = registry::resources()
        .iter()
        .find(|r| r.name == "HW")
        .expect("HW resource");
    assert_eq!(hw.kind, registry::ResKind::Consume);
    assert!(
        settle(|| radio.is_running() && radio.is_ready(), 3000),
        "RADIO did not enter its idle worker"
    );
    assert!(!client.is_running());
    assert!(client.is_disabled());
    assert!(!hw.slot.is_filled(), "RADIO consumes HW on entry");
    assert!(FIRST_FAULT.get().is_none());

    radio.inject(Fault::Crash).expect("crash RADIO");
    assert!(
        settle(|| radio.has_exited() && !radio.is_running(), 3000),
        "RADIO did not exit after the injected crash"
    );
    assert!(
        !hw.slot.is_filled(),
        "a consumed resource stays empty on exit"
    );

    try_request_control(client, ControlOp::Activate).expect("queue CLIENT activation");
    assert!(
        settle(|| FIRST_FAULT.get().is_some(), 5000),
        "activation fault never reached the supervisor driver"
    );
    let first_fault = FIRST_FAULT.get().expect("reported activation fault");
    assert!(std::ptr::eq(first_fault.node, radio), "{first_fault:?}");
    assert!(matches!(first_fault.kind, FaultKind::ResourceMissing));
    assert!(!radio.is_running());
    assert!(!client.is_running());
    assert!(
        !client.is_disabled(),
        "Activate clears CLIENT's disabled latch"
    );

    // The page's reentry can report more faults while HW is absent. Repair
    // the resource, not the fault count, and let that same loop recover.
    hw.slot.provide(1);
    assert!(
        settle(
            || radio.is_running() && radio.is_ready() && client.is_running() && client.is_ready(),
            5000
        ),
        "reproviding HW did not recover RADIO and CLIENT through driver reentry"
    );
    assert!(!hw.slot.is_filled(), "recovered RADIO consumes the new HW");
}
