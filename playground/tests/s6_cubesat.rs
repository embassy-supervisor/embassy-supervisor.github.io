//! S6 CubeSat flight software: the limit checker demand-starts the stored
//! command sequence through a gated read, TO parks bound until the comm
//! window opens, the attitude chain gates on a real estimate, and health
//! services answer a stalled app with an automatic restart.

use std::time::Duration as StdDuration;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor::Fault;
use embassy_supervisor_playground::{build, health, parse, registry};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, MockDriver};

const DSL: &str = r#"
supervisor_graph! {
    node TIME = Terminate, task: crate::cfe::time_task,
        writes: [signals::TONE observed];
    node SCH = Terminate, deps: [TIME ready], slot_timeout: 6000,
        task: crate::cfe::sch_task,
        reads: [signals::TONE], writes: [signals::WAKEUP observed];
    node ADCS_SENSE = Terminate, deps: [SCH],
        task: crate::adcs::sense_task, beat_timeout: 800,
        reads: [signals::WAKEUP], writes: [signals::GYRO observed beat];
    node ADCS_EST = Terminate, deps: [ADCS_SENSE],
        task: crate::adcs::estimate_task,
        ready_on_write, beat_timeout: 1000,
        reads: [signals::GYRO], writes: [signals::ATT observed beat];
    node ADCS_CTRL = Terminate, deps: [ADCS_EST ready], slot_timeout: 4000,
        task: crate::adcs::control_task,
        reads: [signals::ATT], writes: [signals::TORQUE observed];
    node HK = Terminate, deps: [ADCS_EST],
        task: crate::cfe::hk_task,
        reads: [signals::ATT], writes: [signals::HK_TLM observed];
    node SB_PIPE = Terminate, deps: [HK],
        task: crate::cfe::sb_task,
        reads: [signals::HK_TLM], writes: [signals::DOWN observed];
    node TO = Terminate, deps: [COMM ready bound], slot_timeout: 3000,
        task: crate::cfe::to_task,
        reads: [signals::DOWN];
    node COMM = OnDemand, task: crate::com::radio_task;
    node LC = Terminate, deps: [SCH],
        task: crate::cfs::lc_task,
        reads: [signals::SEQ];
    node SC = Terminate, disabled,
        task: crate::cfs::sc_task,
        writes: [signals::SEQ observed];
    node FM = OnDemand, task: crate::cfs::fm_task;
}
"#;

const BEHAVIORS: &str = r#"{
    "nodes": {
        "TIME": { "kind": "periodic", "period_ms": 1000 },
        "SCH": { "kind": "pipeline", "work_ms": 100 },
        "ADCS_SENSE": { "kind": "pipeline", "work_ms": 200 },
        "ADCS_EST": { "kind": "pipeline", "work_ms": 250 },
        "ADCS_CTRL": { "kind": "control_loop", "period_ms": 200 },
        "HK": { "kind": "pipeline", "work_ms": 400 },
        "SB_PIPE": { "kind": "queue", "capacity": 6, "policy": "reject", "drain_ms": 300 },
        "TO": { "kind": "pipeline", "work_ms": 300 },
        "COMM": { "kind": "link", "initially_up": true },
        "LC": { "kind": "gated_consumer", "open": "signals::SEQ", "period_ms": 400 },
        "SC": { "kind": "periodic", "period_ms": 600 },
        "FM": { "kind": "oneshot", "run_ms": 1200 }
    },
    "escalation": {
        "ADCS_SENSE": "restart"
    }
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
async fn health_driver() {
    health::drive().await
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

fn by_name(name: &str) -> &'static registry::NodeRt {
    registry::nodes()
        .iter()
        .find(|rt| rt.model.name == name)
        .unwrap_or_else(|| panic!("no node {name}"))
}

#[test]
fn scheduler_demand_start_and_hs_ladder() {
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
            sp.spawn(starter(sp).unwrap());
            sp.spawn(health_driver().unwrap());
            sp.spawn(supervise(sp).unwrap());
        });
    });

    // Bring-up: the scheduler waits out its first major-frame sync, the
    // attitude chain gates on a real estimate, and TO parks bound because
    // the comm window is closed.
    assert!(
        settle(
            || by_name("SCH").node.is_running()
                && by_name("ADCS_EST").node.is_ready()
                && by_name("ADCS_CTRL").node.is_running(),
            10000
        ),
        "core bring-up did not settle"
    );
    assert!(
        by_name("TO").node.is_bound_stopped() || !by_name("TO").node.is_running(),
        "TO parks until the comm window opens"
    );

    // The purest demand-start edge: the limit checker's gated read opens
    // the sequence signal, which activates the disabled SC through the real
    // control queue. No deps: anywhere on that edge.
    assert!(
        settle(|| by_name("SC").node.is_running(), 6000),
        "LC's open() demand-starts the stored-command app"
    );

    // Ground command: an OnDemand app starts on request and runs its job.
    assert!(!by_name("FM").node.is_running());
    START.try_send(by_name("FM").node).unwrap();
    assert!(
        settle(|| by_name("FM").node.is_running(), 4000),
        "FM starts on command"
    );
    assert!(
        settle(|| by_name("FM").node.has_exited(), 4000),
        "FM completes"
    );

    // AOS: start the comm window; the bound TO comes back with it.
    START.try_send(by_name("COMM").node).unwrap();
    assert!(
        settle(|| by_name("COMM").node.is_running(), 4000),
        "radio up for the pass"
    );
    assert!(
        settle(|| by_name("TO").node.is_running(), 6000),
        "TO follows the window open"
    );

    // HS ladder, first rung: a stalled app is restarted automatically.
    let epoch = by_name("ADCS_SENSE").node.epoch();
    by_name("ADCS_SENSE").node.inject(Fault::Stall).unwrap();
    assert!(
        settle(
            || by_name("ADCS_SENSE").node.epoch() > epoch
                && by_name("ADCS_SENSE").node.is_running(),
            10000
        ),
        "the escalation restarts the stalled app"
    );
    by_name("ADCS_SENSE").node.clear_fault();
}
