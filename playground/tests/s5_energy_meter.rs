//! S5 smart energy meter: the association pool is bound to the PLC carrier,
//! the local load profile is not — drop the carrier and the associations
//! stop while the profile keeps growing.

use std::sync::atomic::Ordering;
use std::time::Duration as StdDuration;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor_playground::{build, parse, registry};
use embassy_time::{Duration, MockDriver};

const DSL: &str = r#"
supervisor_graph! {
    executor METROLOGY;
    node SAMPLER = Terminate, executor: METROLOGY,
        task: crate::metrology::sampler_task,
        ready_on_write, beat_timeout: 500,
        writes: [signals::WAVEFORM observed beat];
    node ACCUM = Terminate, executor: METROLOGY, deps: [SAMPLER ready],
        slot_timeout: 2000,
        task: crate::metrology::accumulate_task,
        reads: [signals::WAVEFORM], writes: [signals::ENERGY observed];
    node TARIFF = Terminate, deps: [ACCUM],
        task: crate::registers::tariff_task,
        reads: [signals::ENERGY], writes: [signals::REGISTERS observed];
    node PROFILE_LOG = Pause, deps: [TARIFF],
        task: crate::registers::profile_task,
        reads: [signals::REGISTERS], writes: [signals::PROFILE observed];
    node COMMS = Terminate, task: crate::plc::carrier_task,
        provides: [PLC_LINK], slot_timeout: 5000;
    pool DLMS = [OnDemand, OnDemand, OnDemand],
        deps: [COMMS ready bound],
        task: crate::dlms::association_task,
        policy: DeferredShrink::new(Duration::from_secs(2)),
        min: 0, max: 3, slot_timeout: 2000,
        resources: [PLC_LINK: shared crate::plc::Plc],
        reads: [signals::REGISTERS];
}
"#;

const BEHAVIORS: &str = r#"{
    "SAMPLER": { "kind": "periodic", "period_ms": 100 },
    "ACCUM": { "kind": "pipeline", "work_ms": 250 },
    "TARIFF": { "kind": "pipeline", "work_ms": 300 },
    "PROFILE_LOG": { "kind": "pipeline", "work_ms": 600 },
    "COMMS": { "kind": "link", "initially_up": true },
    "DLMS": { "kind": "session", "busy_ms": 500 }
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

#[test]
fn opportunistic_uplink_authoritative_record() {
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

    // Bring-up: metering readiness comes from the write sweep, the register
    // chain flows, one warm association.
    assert!(
        settle(
            || by_name("SAMPLER").node.is_ready() && by_name("PROFILE_LOG").node.is_running(),
            6000
        ),
        "metrology chain did not settle"
    );
    assert!(
        settle(|| by_name("DLMS#0").node.is_running(), 6000),
        "one warm association"
    );

    // Client dial to the cap: the pool grows to max and no further.
    for m in ["DLMS#0", "DLMS#1", "DLMS#2"] {
        by_name(m).input.store(3.0f32.to_bits(), Ordering::Relaxed);
    }
    assert!(
        settle(|| by_name("DLMS#2").node.is_running(), 10000),
        "the pool grows to the cap"
    );

    // Carrier lost: the association pool bound-stops — and the load profile
    // keeps growing, because the local record is authoritative.
    let profile = registry::signals()
        .iter()
        .find(|s| s.name == "signals::PROFILE")
        .unwrap();
    by_name("COMMS")
        .input
        .store(0.0f32.to_bits(), Ordering::Relaxed);
    assert!(
        settle(
            || by_name("DLMS#0").node.is_bound_stopped() && !by_name("DLMS#1").node.is_running(),
            6000
        ),
        "the associations follow the carrier down"
    );
    let w = profile.writes.load(Ordering::Relaxed);
    assert!(
        settle(|| profile.writes.load(Ordering::Relaxed) > w + 2, 6000),
        "the load profile keeps growing offline"
    );

    // Carrier back: the associations resume.
    by_name("COMMS")
        .input
        .store(1.0f32.to_bits(), Ordering::Relaxed);
    assert!(
        settle(|| by_name("DLMS#0").node.is_running(), 8000),
        "associations resume"
    );
}
