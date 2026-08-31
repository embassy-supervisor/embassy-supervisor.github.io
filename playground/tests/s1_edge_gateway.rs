//! S1 industrial edge gateway: bring-up over the serialized fieldbus, the
//! spool absorbing an uplink outage (drop_oldest, bounded), the backlog
//! draining on recovery, and the poll pool growing with the device count.

use std::sync::atomic::Ordering;
use std::time::Duration as StdDuration;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor_playground::{build, parse, registry};
use embassy_time::{Duration, MockDriver};

const DSL: &str = r#"
supervisor_graph! {
    executor FIELDBUS;
    node RS485 = Pause, executor: FIELDBUS, task: crate::fieldbus::bus_task,
        provides: [BUS485], slot_timeout: 2000;
    pool FIELD_POLL = [Terminate, OnDemand, OnDemand, OnDemand],
        executor: FIELDBUS, deps: [RS485 ready], task: crate::fieldbus::poll_task,
        policy: DeferredShrink::new(Duration::from_secs(3)),
        min: 1, max: 4, slot_timeout: 2000,
        resources: [BUS485: shared crate::fieldbus::Rs485Port],
        writes: [signals::TAGS observed];
    node TAG_DB = Terminate, deps: [FIELD_POLL], task: crate::tags::db_task,
        reads: [signals::TAGS], writes: [signals::CHANGES observed];
    node DEADBAND = Terminate, deps: [TAG_DB], task: crate::tags::deadband_task,
        reads: [signals::CHANGES], writes: [signals::EVENTS observed];
    node SPOOL = Terminate, deps: [DEADBAND], task: crate::uplink::spool_task,
        reads: [signals::EVENTS], writes: [signals::BATCH observed];
    node MODEM = Terminate, task: crate::uplink::modem_task,
        provides: [NET_STACK], slot_timeout: 8000;
    node SPARKPLUG = Terminate, deps: [MODEM ready bound, SPOOL],
        task: crate::uplink::sparkplug_task,
        resources: [NET_STACK: shared embassy_net::Stack<'static>],
        slot_timeout: 5000, reads: [signals::BATCH];
}
"#;

const BEHAVIORS: &str = r#"{
    "RS485": { "kind": "provider", "startup_ms": 300 },
    "FIELD_POLL": { "kind": "poller", "period_ms": 400, "txn_ms": 40 },
    "TAG_DB": { "kind": "pipeline", "work_ms": 200 },
    "DEADBAND": { "kind": "pipeline", "work_ms": 250 },
    "SPOOL": { "kind": "queue", "capacity": 12, "policy": "drop_oldest", "drain_ms": 300 },
    "MODEM": { "kind": "link", "initially_up": true },
    "SPARKPLUG": { "kind": "pipeline", "work_ms": 300 }
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

#[test]
fn edge_gateway_story() {
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
        executor.run(|sp| sp.spawn(supervise(sp).unwrap()));
    });

    // Bring-up: the bus provider readies, one poller polls, the pipeline
    // flows, the singleton session consumes.
    assert!(
        settle(
            || {
                by_name("RS485").node.is_ready()
                    && by_name("FIELD_POLL#0").node.is_running()
                    && by_name("SPARKPLUG").node.is_running()
            },
            5000
        ),
        "bring-up did not settle"
    );
    assert!(
        !by_name("FIELD_POLL#1").node.is_running(),
        "pool starts at min=1"
    );
    let batch = registry::signals()
        .iter()
        .find(|s| s.name == "signals::BATCH")
        .unwrap();
    assert!(
        settle(|| batch.reads.load(Ordering::Relaxed) > 2, 5000),
        "the session consumes batches"
    );

    // Uplink drops: the bound session stops, polling continues, and the
    // spool absorbs the backlog instead of losing it silently.
    by_name("MODEM")
        .input
        .store(0.0f32.to_bits(), Ordering::Relaxed);
    assert!(
        settle(|| by_name("SPARKPLUG").node.is_bound_stopped(), 4000),
        "the session follows its bound link down"
    );
    let tags = registry::signals()
        .iter()
        .find(|s| s.name == "signals::TAGS")
        .unwrap();
    let w = tags.writes.load(Ordering::Relaxed);
    assert!(
        settle(|| tags.writes.load(Ordering::Relaxed) > w + 3, 4000),
        "polling continues offline"
    );
    assert!(
        settle(|| depth_of("signals::BATCH") >= 3, 8000),
        "the spool backlog climbs"
    );
    // Bounded: drop_oldest never exceeds capacity.
    assert!(depth_of("signals::BATCH") <= 12);

    // Uplink returns: the session resumes and the backlog drains.
    by_name("MODEM")
        .input
        .store(1.0f32.to_bits(), Ordering::Relaxed);
    assert!(
        settle(|| by_name("SPARKPLUG").node.is_running(), 6000),
        "the session resumes"
    );
    assert!(
        settle(|| depth_of("signals::BATCH") == 0, 15000),
        "the backlog drains"
    );

    // Device count up: transactions stretch, pollers call for help, the
    // pool grows toward max.
    for m in [
        "FIELD_POLL#0",
        "FIELD_POLL#1",
        "FIELD_POLL#2",
        "FIELD_POLL#3",
    ] {
        by_name(m).input.store(4.0f32.to_bits(), Ordering::Relaxed);
    }
    assert!(
        settle(|| by_name("FIELD_POLL#1").node.is_running(), 10000),
        "busy pollers grow the pool"
    );

    // Device count down: DeferredShrink folds the pool back after cooldown.
    for m in [
        "FIELD_POLL#0",
        "FIELD_POLL#1",
        "FIELD_POLL#2",
        "FIELD_POLL#3",
    ] {
        by_name(m).input.store(0.0f32.to_bits(), Ordering::Relaxed);
    }
    assert!(
        settle(
            || !by_name("FIELD_POLL#1").node.is_running()
                && !by_name("FIELD_POLL#2").node.is_running(),
            15000
        ),
        "the pool folds back to the floor"
    );
}
