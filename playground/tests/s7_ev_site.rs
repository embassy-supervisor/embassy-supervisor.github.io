//! S7 EV charging site: one `divisible` site limit, a real `Budget`, divided
//! over the sessions' claims (shrink fast, grow slow), released by the
//! supervisor when a session is stopped, cut at once on a derate; and the
//! store-and-forward queue holding transaction events across a CSMS outage.

use std::sync::atomic::Ordering;
use std::time::Duration as StdDuration;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor_playground::{build, parse, registry};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
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

const STEP: u32 = 4;

/// Single-node stops (`stop_node`): what a session's own "unplug" button
/// drives. A control-queue `Deactivate` would seed the whole pool.
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

fn want(name: &str) -> u32 {
    let rt = by_name(name);
    budget().want_of(rt.claims[0].1.slot())
}

fn plug(cars: f32) {
    for m in ["EVSE#0", "EVSE#1", "EVSE#2", "EVSE#3"] {
        by_name(m).input.store(cars.to_bits(), Ordering::Relaxed);
    }
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
        executor.run(|sp| {
            sp.spawn(stopper().unwrap());
            sp.spawn(supervise(sp).unwrap());
        });
    });

    // Bring-up: the allocator provides the site limit, the floor session
    // spawns through the budget's gate.
    assert!(
        settle(
            || by_name("EVSE#0").node.is_running() && by_name("OCPP_TX").node.is_running(),
            8000
        ),
        "bring-up did not settle"
    );
    assert_eq!(
        budget().capacity(),
        32,
        "the allocator provided the site limit"
    );
    assert_eq!(grant("EVSE#0"), 0, "an idle session claims nothing");

    // One car: the lone claimant ramps up to the whole limit, `step` units
    // per period, never as a jump.
    plug(1.0);
    assert!(
        settle(|| want("EVSE#0") == 32, 3000),
        "a session states its want"
    );
    assert!(
        settle(|| grant("EVSE#0") > 0, 3000),
        "the first grant lands"
    );
    assert!(
        grant("EVSE#0") <= STEP,
        "growth is stepped: {}",
        grant("EVSE#0")
    );
    assert!(
        settle(|| grant("EVSE#0") == 32, 8000),
        "a lone session gets the whole limit"
    );

    // Three cars: the pool grows and the first grant is cut the instant the
    // new claims land — the safety asymmetry (shed now, ramp back slowly).
    plug(3.0);
    assert!(
        settle(|| by_name("EVSE#2").node.is_running(), 10000),
        "three sessions active"
    );
    assert!(
        settle(|| grant("EVSE#0") <= 11, 3000),
        "each grant shrinks as claimants join: {}",
        grant("EVSE#0")
    );
    assert!(
        budget().total_granted() <= 32,
        "the division never exceeds the limit"
    );
    assert!(
        settle(|| grant("EVSE#1") > 0 && grant("EVSE#2") > 0, 5000),
        "the newcomers get their shares"
    );

    // One car leaves: the highest-numbered session closes, the others stay.
    plug(2.0);
    assert!(
        settle(|| want("EVSE#2") == 0 && want("EVSE#1") > 0, 3000),
        "a dial step down ends exactly one session"
    );

    // Stop a session from the outside while its claim is up: the supervisor
    // releases the slot on the ack — the worker never touched its claim. The
    // orphaned car is picked up by an idle member, so two claimants remain.
    let evse1 = by_name("EVSE#1");
    assert!(want("EVSE#1") > 0);
    STOP.try_send(evse1.node).expect("stop queue");
    assert!(
        settle(
            || !evse1.node.is_running() && want("EVSE#1") == 0 && grant("EVSE#1") == 0,
            5000
        ),
        "a stopped holder's share is released by the supervisor"
    );
    // ...and the survivors grow into it, one step at a time.
    let low = grant("EVSE#0");
    assert!(settle(|| grant("EVSE#0") > low, 5000), "grants ramp back");
    assert!(
        grant("EVSE#0") <= low + STEP,
        "growth is stepped: {low} -> {}",
        grant("EVSE#0")
    );
    assert!(
        settle(|| grant("EVSE#0") == 16, 8000),
        "two claimants split the limit"
    );

    // A derate: the site limit dial halves the provided capacity and every
    // grant above the new fair share is cut on the next division.
    by_name("ENERGY_MGR")
        .input
        .store(0.5f32.to_bits(), Ordering::Relaxed);
    assert!(
        settle(|| budget().capacity() == 16, 2000),
        "the capacity follows the dial"
    );
    assert!(
        settle(|| grant("EVSE#0") == 8, 2000),
        "a derate cuts at once"
    );
    by_name("ENERGY_MGR")
        .input
        .store(1.0f32.to_bits(), Ordering::Relaxed);
    assert!(
        settle(|| grant("EVSE#0") == 16, 8000),
        "and the grants ramp back"
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
    // down releasing their claims, and the store-and-forward drains
    // completely — the causal record reaches the CSMS before the site goes
    // quiet.
    by_name("OCPP")
        .input
        .store(1.0f32.to_bits(), Ordering::Relaxed);
    assert!(
        settle(|| by_name("OCPP_TX").node.is_running(), 6000),
        "transmitter resumes"
    );
    plug(0.0);
    assert!(
        settle(|| budget().total_granted() == 0, 5000),
        "unplugged sessions release their shares"
    );
    assert!(
        settle(|| depth_of("signals::OCPP_OUT") == 0, 25000),
        "the backlog drains"
    );
    advance_ms(500);
    assert_eq!(
        budget().capacity(),
        32,
        "the limit stays provided while the allocator runs"
    );
}
