//! S7 EV charging site: the divisible budget re-divides as sessions join
//! (shrink fast), and the store-and-forward queue holds transaction events
//! in causal order across a CSMS outage, draining on reconnect.

use std::sync::atomic::Ordering;
use std::time::Duration as StdDuration;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor_playground::{build, parse, registry};
use embassy_time::{Duration, MockDriver};

const DSL: &str = r#"
supervisor_graph! {
    node GRID_METER = Terminate, task: crate::site::meter_task,
        writes: [signals::SITE_LOAD observed];
    node ENERGY_MGR = Terminate, deps: [GRID_METER],
        task: crate::site::energy_task,
        reads: [signals::SITE_LOAD],
        writes: [signals::AMPS_BUDGET observed];
    pool EVSE = [Terminate, OnDemand, OnDemand, OnDemand],
        deps: [ENERGY_MGR], slot_timeout: 3000,
        task: crate::evse::session_task,
        policy: DeferredShrink::new(Duration::from_secs(2)),
        min: 1, max: 4,
        reads: [signals::AMPS_BUDGET],
        writes: [signals::SESSION_EVTS observed];
    node SAF_Q = Terminate, deps: [EVSE],
        task: crate::ocpp::saf_task,
        reads: [signals::SESSION_EVTS],
        writes: [signals::OCPP_OUT observed];
    node OCPP = Terminate, task: crate::ocpp::client_task,
        provides: [CSMS], slot_timeout: 5000;
    node OCPP_TX = Terminate, deps: [OCPP ready bound], slot_timeout: 3000,
        task: crate::ocpp::tx_task,
        resources: [CSMS: shared crate::ocpp::Csms],
        reads: [signals::OCPP_OUT];
}
"#;

const BEHAVIORS: &str = r#"{
    "GRID_METER": { "kind": "periodic", "period_ms": 400 },
    "ENERGY_MGR": { "kind": "budget", "total": 32, "period_ms": 300 },
    "EVSE": { "kind": "session", "busy_ms": 400 },
    "SAF_Q": { "kind": "queue", "capacity": 10, "policy": "backpressure", "drain_ms": 120 },
    "OCPP": { "kind": "link", "initially_up": true },
    "OCPP_TX": { "kind": "pipeline", "work_ms": 300 }
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

fn budget_value() -> f32 {
    let s = registry::signals()
        .iter()
        .find(|s| s.name == "signals::AMPS_BUDGET")
        .unwrap();
    f32::from_bits(s.value.load(Ordering::Relaxed))
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
fn budget_division_and_offline_queue() {
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

    // Bring-up: one session at the floor gets (asymptotically) the whole
    // site budget.
    assert!(
        settle(
            || by_name("EVSE#0").node.is_running() && by_name("OCPP_TX").node.is_running(),
            8000
        ),
        "bring-up did not settle"
    );
    assert!(
        settle(|| budget_value() > 20.0, 15000),
        "a lone session gets most of the site limit"
    );

    // Cars plug in: the pool grows and every grant shrinks the instant the
    // claimant count rises — the safety asymmetry (shed load now, ramp back
    // slowly).
    let before = budget_value();
    for m in ["EVSE#0", "EVSE#1", "EVSE#2", "EVSE#3"] {
        by_name(m).input.store(3.0f32.to_bits(), Ordering::Relaxed);
    }
    assert!(
        settle(|| by_name("EVSE#2").node.is_running(), 10000),
        "three sessions active"
    );
    assert!(
        settle(|| budget_value() < before / 2.0, 5000),
        "each grant shrinks as claimants join"
    );

    // CSMS drops: the transmitter bound-stops and transaction events queue
    // in the back-pressured store-and-forward instead of being lost.
    by_name("OCPP")
        .input
        .store(0.0f32.to_bits(), Ordering::Relaxed);
    assert!(
        settle(|| by_name("OCPP_TX").node.is_bound_stopped(), 5000),
        "the transmitter follows the CSMS link down"
    );
    assert!(
        settle(|| depth_of("signals::OCPP_OUT") >= 3, 12000),
        "events queue offline"
    );
    assert!(
        depth_of("signals::OCPP_OUT") <= 10,
        "back-pressure bounds the queue"
    );

    // Reconnect and unplug: the transmitter resumes, the sessions wind
    // down, and the store-and-forward drains completely — the causal record
    // reaches the CSMS before the site goes quiet.
    by_name("OCPP")
        .input
        .store(1.0f32.to_bits(), Ordering::Relaxed);
    assert!(
        settle(|| by_name("OCPP_TX").node.is_running(), 6000),
        "transmitter resumes"
    );
    for m in ["EVSE#0", "EVSE#1", "EVSE#2", "EVSE#3"] {
        by_name(m).input.store(0.0f32.to_bits(), Ordering::Relaxed);
    }
    assert!(
        settle(|| depth_of("signals::OCPP_OUT") == 0, 25000),
        "the backlog drains"
    );

    // Grants grow back after the claimants leave — slowly, never as a step.
    let low = budget_value();
    assert!(
        settle(|| budget_value() > low + 1.0, 20000),
        "grants ramp back after claimants leave"
    );
}
