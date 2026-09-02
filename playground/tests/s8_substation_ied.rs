//! S8 substation protection IED: two networks, two completely different
//! blast radii. Losing PTP bound-stops differential while overcurrent keeps
//! protecting; losing the station bus stops SCADA sessions and touches
//! nothing in the protection plane. Breaker failure arms on demand. TRIP is
//! a real `VetoGate`: any protection function asserts its own bit, and a
//! bound-stopped function's bit stays up until it runs again.

use std::sync::atomic::Ordering;
use std::time::Duration as StdDuration;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor::{ControlOp, try_request_control};
use embassy_supervisor_playground::registry::Gate;
use embassy_supervisor_playground::{build, parse, registry};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, MockDriver};

const DSL: &str = r#"
supervisor_graph! {
    executor PROCESS_BUS;
    node PTP_SLAVE = Terminate, task: crate::ptp::slave_task;
    node SV_RX_MU1 = Terminate, executor: PROCESS_BUS,
        task: crate::sv::rx_task, writes: [signals::SV1 observed];
    node SV_RX_MU2 = Terminate, executor: PROCESS_BUS,
        task: crate::sv::rx_task, writes: [signals::SV2 observed];
    node SV_ALIGN = Terminate, executor: PROCESS_BUS,
        deps: [SV_RX_MU1, SV_RX_MU2],
        task: crate::sv::align_task,
        ready_on_write, beat_timeout: 500,
        reads: [signals::SV1, signals::SV2],
        writes: [signals::PHASORS observed beat];
    node PROT_5051 = Terminate, executor: PROCESS_BUS,
        deps: [SV_ALIGN ready], slot_timeout: 3000,
        task: crate::prot::overcurrent_task,
        reads: [signals::PHASORS], writes: [signals::TRIP veto observed];
    node PROT_87 = Terminate, executor: PROCESS_BUS,
        deps: [SV_ALIGN ready, PTP_SLAVE ready bound], slot_timeout: 3000,
        task: crate::prot::differential_task,
        reads: [signals::PHASORS], writes: [signals::TRIP veto observed];
    node PROT_50BF = OnDemand, executor: PROCESS_BUS,
        task: crate::prot::breaker_failure_task,
        reads: [signals::TRIP], writes: [signals::BF_TRIP observed];
    node STATION_LINK = Terminate, task: crate::station::link_task,
        provides: [STATION_BUS], slot_timeout: 5000;
    pool MMS = [OnDemand, OnDemand, OnDemand],
        deps: [STATION_LINK ready bound], slot_timeout: 2000,
        task: crate::mms::session_task,
        policy: DeferredShrink::new(Duration::from_secs(2)),
        min: 0, max: 3,
        resources: [STATION_BUS: shared crate::station::Bus],
        reads: [signals::GOOSE];
    node TRIP_LOGIC = Terminate, executor: PROCESS_BUS,
        deps: [PROT_5051],
        task: crate::prot::trip_matrix_task,
        reads: [signals::TRIP, signals::BF_TRIP],
        writes: [signals::GOOSE observed];
}
"#;

const BEHAVIORS: &str = r#"{
    "PTP_SLAVE": { "kind": "link", "initially_up": true },
    "crate::sv::rx_task": { "kind": "periodic", "period_ms": 100 },
    "SV_ALIGN": { "kind": "pipeline", "work_ms": 100 },
    "PROT_5051": { "kind": "veto_writer", "period_ms": 150 },
    "PROT_87": { "kind": "veto_writer", "period_ms": 150 },
    "PROT_50BF": { "kind": "control_loop", "period_ms": 200 },
    "STATION_LINK": { "kind": "link", "initially_up": true },
    "MMS": { "kind": "session", "busy_ms": 400 },
    "TRIP_LOGIC": { "kind": "veto_sink", "period_ms": 150 }
}"#;

static START: Channel<CriticalSectionRawMutex, &'static embassy_supervisor::TaskNode, 2> =
    Channel::new();

#[embassy_executor::task]
async fn starter(spawner: Spawner) {
    loop {
        let node = START.receive().await;
        let sup = build::built().unwrap().sup;
        sup.start_node(node, &spawner).await.expect("start_node");
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

fn trip()
-> &'static embassy_supervisor::VetoGate<{ embassy_supervisor_playground::model::MAX_VETO_SLOTS }> {
    let sig = registry::signals()
        .iter()
        .find(|s| s.name == "signals::TRIP")
        .unwrap();
    match sig.gate {
        Gate::Veto(g) => g,
        _ => panic!("TRIP should run as a veto gate"),
    }
}

fn pickup(name: &str, on: bool) {
    by_name(name)
        .input
        .store(if on { 1.0f32 } else { 0.0 }.to_bits(), Ordering::Relaxed);
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
fn two_networks_two_blast_radii() {
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
            sp.spawn(starter(sp).unwrap());
            sp.spawn(supervise(sp).unwrap());
        });
    });

    // Bring-up: alignment readies through its first full window, both
    // protection functions run.
    assert!(
        settle(
            || by_name("SV_ALIGN").node.is_ready()
                && by_name("PROT_5051").node.is_running()
                && by_name("PROT_87").node.is_running(),
            8000
        ),
        "protection plane did not settle"
    );

    // The trip is a veto gate: any function's pickup asserts it, no single
    // release clears it while another bit is up, and the trip matrix
    // publishes GOOSE for as long as it is asserted.
    assert!(!trip().is_asserted(), "no pickup at boot");
    pickup("PROT_87", true);
    assert!(settle(|| trip().is_asserted(), 2000), "one pickup trips");
    assert!(
        settle(
            || by_name("TRIP_LOGIC").node.status() == Some("vetoed"),
            2000
        ),
        "the trip matrix reacts"
    );
    pickup("PROT_5051", true);
    assert!(
        settle(|| trip().contributors().count_ones() == 2, 2000),
        "two contributor bits"
    );
    let goose = writes_of("signals::GOOSE");
    advance_ms(1000);
    assert!(
        writes_of("signals::GOOSE") > goose + 2,
        "GOOSE keeps publishing while tripped"
    );
    pickup("PROT_87", false);
    assert!(
        settle(|| trip().contributors().count_ones() == 1, 2000),
        "a release drops one bit"
    );
    assert!(
        trip().is_asserted(),
        "the other function still holds the trip"
    );

    // Lose PTP lock with differential tripping: it bound-stops (cross-bay
    // comparison needs matching sync) while overcurrent keeps protecting on
    // magnitude alone — and the stopped function's bit stays up. A dead
    // protection function never releases a trip: fail-safe.
    pickup("PROT_87", true);
    assert!(
        settle(|| trip().contributors().count_ones() == 2, 2000),
        "differential picks up again"
    );
    by_name("PTP_SLAVE")
        .input
        .store(0.0f32.to_bits(), Ordering::Relaxed);
    assert!(
        settle(|| by_name("PROT_87").node.is_bound_stopped(), 5000),
        "differential follows PTP down"
    );
    assert!(
        by_name("PROT_5051").node.is_running(),
        "overcurrent keeps protecting"
    );
    assert_eq!(
        trip().contributors().count_ones(),
        2,
        "a stopped writer's bit stays asserted"
    );
    pickup("PROT_5051", false);
    advance_ms(1000);
    assert!(
        trip().is_asserted(),
        "overcurrent's release cannot clear the stopped function's bit"
    );
    let goose = writes_of("signals::GOOSE");
    advance_ms(1000);
    assert!(
        writes_of("signals::GOOSE") > goose + 2,
        "the trip stream never pauses"
    );
    pickup("PROT_87", false);
    by_name("PTP_SLAVE")
        .input
        .store(1.0f32.to_bits(), Ordering::Relaxed);
    assert!(
        settle(|| by_name("PROT_87").node.is_running(), 6000),
        "differential resumes on lock"
    );
    assert!(
        settle(|| !trip().is_asserted(), 2000),
        "the restarted function re-evaluates and releases its bit"
    );
    assert!(
        settle(
            || by_name("TRIP_LOGIC").node.status() == Some("clear"),
            2000
        ),
        "the trip matrix clears"
    );

    // Lose the station bus: the MMS sessions stop — and nothing in the
    // protection plane moves.
    assert!(
        settle(|| by_name("MMS#0").node.is_running(), 6000),
        "one warm SCADA session"
    );
    let epoch_5051 = by_name("PROT_5051").node.epoch();
    let epoch_87 = by_name("PROT_87").node.epoch();
    by_name("STATION_LINK")
        .input
        .store(0.0f32.to_bits(), Ordering::Relaxed);
    assert!(
        settle(|| by_name("MMS#0").node.is_bound_stopped(), 5000),
        "SCADA sessions follow the station bus down"
    );
    pickup("PROT_5051", true);
    assert!(settle(|| trip().is_asserted(), 2000), "the relay trips");
    let goose = writes_of("signals::GOOSE");
    advance_ms(1000);
    assert!(
        writes_of("signals::GOOSE") > goose + 2,
        "the relay keeps tripping without SCADA"
    );
    assert_eq!(
        by_name("PROT_5051").node.epoch(),
        epoch_5051,
        "protection untouched"
    );
    pickup("PROT_5051", false);
    assert_eq!(
        by_name("PROT_87").node.epoch(),
        epoch_87,
        "protection untouched"
    );

    // Breaker failure arms on demand: an OnDemand start, the way a real
    // relay arms 50BF from the first trip.
    assert!(!by_name("PROT_50BF").node.is_running());
    START.try_send(by_name("PROT_50BF").node).unwrap();
    assert!(
        settle(|| by_name("PROT_50BF").node.is_running(), 4000),
        "50BF armed on demand"
    );

    // Bus back up: the bound-stopped sessions recover.
    by_name("STATION_LINK")
        .input
        .store(1.0f32.to_bits(), Ordering::Relaxed);
    assert!(
        settle(|| by_name("MMS#0").node.is_running(), 6000),
        "SCADA sessions recover with the bus"
    );

    // Restart the station link: the down-wave stops the sessions and the
    // up-wave excludes OnDemand members — the respawned link's readiness
    // must regrow the pool on its own (supervisor 0.7.0; the parked-pool
    // bug this scenario first surfaced).
    try_request_control(by_name("STATION_LINK").node, ControlOp::Restart).unwrap();
    assert!(
        settle(
            || by_name("STATION_LINK").node.is_running() && by_name("MMS#0").node.is_running(),
            8000
        ),
        "the session pool regrows after a restart of the link"
    );

    // Deactivate the link: the sessions stop under the collateral hold, not
    // the disabled latch — so activating the link releases and regrows them
    // (the permanently-disabled-members bug, also fixed in 0.7.0).
    try_request_control(by_name("STATION_LINK").node, ControlOp::Deactivate).unwrap();
    assert!(
        settle(
            || !by_name("MMS#0").node.is_running() && !by_name("STATION_LINK").node.is_running(),
            6000
        ),
        "deactivate stops the sessions and the link"
    );
    assert!(by_name("STATION_LINK").node.is_disabled());
    assert!(
        by_name("MMS#0").node.is_collateral() && !by_name("MMS#0").node.is_disabled(),
        "a session is held as a dependent, not deactivated in its own right"
    );
    try_request_control(by_name("STATION_LINK").node, ControlOp::Activate).unwrap();
    assert!(
        settle(
            || by_name("STATION_LINK").node.is_running() && by_name("MMS#0").node.is_running(),
            8000
        ),
        "activate releases the held sessions and demand regrows the pool"
    );
    assert!(!by_name("MMS#0").node.is_collateral());
}
