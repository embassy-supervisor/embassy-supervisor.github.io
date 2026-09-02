//! S7 EV charging site, the shrink cycle: sessions are dealt out across the
//! members that are running, so after the pool has folded back the surviving
//! spare (whichever member it is) takes the next car and the pool scales out
//! again. Before this rule member `k` served only the `k+1`-th car, and a
//! site left with `EVSE#0` and `EVSE#3` ignored the second and third car.

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
        provides: [SITE_AMPS],
        reads: [signals::SITE_LOAD];
    pool EVSE = [Terminate, OnDemand, OnDemand, OnDemand],
        deps: [ENERGY_MGR], slot_timeout: 3000,
        task: crate::evse::session_task,
        policy: DeferredShrink::new(Duration::from_secs(2)),
        min: 1, max: 4,
        resources: [SITE_AMPS: divisible],
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
    "ENERGY_MGR": { "kind": "budget", "total": 32, "period_ms": 300, "step": 4 },
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

fn budget()
-> &'static embassy_supervisor::Budget<{ embassy_supervisor_playground::model::MAX_NODES }> {
    registry::resources()
        .iter()
        .find(|r| r.name == "SITE_AMPS")
        .expect("SITE_AMPS")
        .slot
        .budget()
        .expect("SITE_AMPS is a budget")
}

fn grant(name: &str) -> u32 {
    by_name(name).claims[0].1.grant()
}

fn plug(cars: f32) {
    for m in ["EVSE#0", "EVSE#1", "EVSE#2", "EVSE#3"] {
        by_name(m).input.store(cars.to_bits(), Ordering::Relaxed);
    }
}

fn busy() -> usize {
    ["EVSE#0", "EVSE#1", "EVSE#2", "EVSE#3"]
        .iter()
        .filter(|m| by_name(m).node.is_busy())
        .count()
}

fn running() -> usize {
    ["EVSE#0", "EVSE#1", "EVSE#2", "EVSE#3"]
        .iter()
        .filter(|m| by_name(m).node.is_running())
        .count()
}

#[test]
fn sessions_follow_the_dial_after_a_shrink() {
    let mut outcome = parse::parse(DSL);
    assert!(
        outcome.ok,
        "parse errors: {:?}",
        outcome.errors.iter().map(|e| &e.msg).collect::<Vec<_>>()
    );
    build::build(outcome.model.take().unwrap(), BEHAVIORS).expect("build");

    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|sp| {
            sp.spawn(supervise(sp).unwrap());
        });
    });

    assert!(
        settle(|| by_name("EVSE#0").node.is_running(), 8000),
        "bring-up did not settle"
    );

    // Four cars: every member opens a session and claims its share.
    plug(4.0);
    assert!(settle(|| busy() == 4, 12000), "four sessions active");
    assert!(
        settle(|| budget().total_granted() == 32, 8000),
        "the whole limit is granted"
    );

    // Down to one car: three sessions close and the pool folds back to the
    // busy member plus one idle spare, retired from the low end.
    plug(1.0);
    assert!(settle(|| busy() == 1, 3000), "one session left");
    assert!(
        settle(|| running() == 2, 12000),
        "the pool shrinks to one busy member and one spare: {} running",
        running()
    );
    assert!(
        settle(|| budget().total_granted() == 32, 12000),
        "the lone session regrows into the whole limit"
    );

    // A second car: the spare takes it, whichever member it is, and the
    // pool scales out again. This was the dead dial.
    plug(2.0);
    assert!(settle(|| busy() == 2, 3000), "the spare opens a session");
    assert!(
        settle(|| running() == 3, 6000),
        "a new spare is started behind it"
    );
    assert!(
        settle(
            || budget().total_granted() == 32 && grant("EVSE#0") == 16,
            12000
        ),
        "two claimants split the limit: {}",
        grant("EVSE#0")
    );

    plug(3.0);
    assert!(settle(|| busy() == 3, 3000), "the third car is served");
    plug(4.0);
    assert!(settle(|| busy() == 4, 6000), "and the fourth");
    assert_eq!(running(), 4);

    // Back to none: every share is released and the pool folds again.
    plug(0.0);
    assert!(
        settle(|| busy() == 0 && budget().total_granted() == 0, 6000),
        "unplugged sessions release their shares"
    );
    assert!(
        settle(|| running() == 1, 12000),
        "the pool folds to its floor"
    );
    advance_ms(100);
}
