//! Activation faults must reach the page through the lifecycle sink while
//! `run()` keeps driving. One test and one build: the interpreter's registry
//! is process-global.

use std::sync::Mutex;
use std::time::{Duration as StdDuration, Instant};

use embassy_executor::{Executor, Spawner};
use embassy_supervisor::{ControlOp, Fault, try_request_control};
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

/// Fault texts drained from the page snapshot, accumulated across polls.
static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn seen_fault(text: &str) -> bool {
    let mut seen = SEEN.lock().unwrap();
    seen.extend(events::snapshot().faults);
    seen.iter().any(|f| f == text)
}

/// The page's driver: `run()` reports faults through the sink and returns
/// only a missed ack or a refused restart, which re-entering retries.
#[embassy_executor::task]
async fn supervise(spawner: Spawner) {
    loop {
        let _ = build::drive_supervisor(&spawner).await;
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
    assert!(SEEN.lock().unwrap().is_empty() && events::snapshot().faults.is_empty());

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
    let missing = format!("{}: a resource slot was never provided", radio.name());
    assert!(
        settle(|| seen_fault(&missing), 5000),
        "the activation fault never reached the lifecycle sink: {:?}",
        SEEN.lock().unwrap()
    );
    assert!(!radio.is_running());
    assert!(!client.is_running());
    assert!(
        !client.is_disabled(),
        "Activate clears CLIENT's disabled latch"
    );

    // A faulted node stays down and enabled while `run()` keeps serving
    // control: providing HW alone brings nothing back, the next Activate does.
    hw.slot.provide(1);
    assert!(
        !settle(|| radio.is_running(), 500),
        "RADIO came back without an Activate"
    );
    try_request_control(client, ControlOp::Activate).expect("queue CLIENT activation");
    assert!(
        settle(
            || radio.is_running() && radio.is_ready() && client.is_running() && client.is_ready(),
            5000
        ),
        "Activate after reproviding HW did not bring RADIO and CLIENT up"
    );
    assert!(!hw.slot.is_filled(), "recovered RADIO consumes the new HW");
}
