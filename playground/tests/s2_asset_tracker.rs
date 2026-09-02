//! S2 cellular asset tracker: the sleep/wake cycle. Teardown parks the Pause
//! nodes and empties the consumed modem slot; a wake without rebuilding the
//! runner fails closed; rebuild + wake brings the graph back; the disabled
//! FOTA latch survives the whole cycle.

use std::sync::atomic::Ordering;
use std::time::Duration as StdDuration;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor_playground::{build, parse, registry};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, MockDriver};

const DSL: &str = r#"
supervisor_graph! {
    name: TRACKER;
    node MOTION = Pause, task: crate::sense::motion_task,
        writes: [signals::MOTION observed];
    node GNSS = Terminate, task: crate::sense::gnss_task,
        ready_on_write, beat_timeout: 1500,
        writes: [signals::FIX observed beat];
    node BUFFER = Pause, deps: [MOTION],
        task: crate::store::buffer_task,
        reads: [signals::MOTION, signals::FIX],
        writes: [signals::RECORDS observed];
    node MODEM = Terminate, task: crate::modem::runner_task,
        resources: [MODEM_HW: consume crate::modem::ModemHw],
        provides: [NET], slot_timeout: 500;
    node UPLINK = Terminate, deps: [MODEM ready bound, BUFFER],
        task: crate::uplink::uplink_task,
        resources: [NET: shared embassy_net::Stack<'static>],
        slot_timeout: 3000,
        reads: [signals::RECORDS];
    node FOTA = Terminate, deps: [MODEM ready],
        task: crate::fota::fota_task, disabled, slot_timeout: 3000;
}
"#;

const BEHAVIORS: &str = r#"{
    "MOTION": { "kind": "periodic", "period_ms": 400, "scaled": true },
    "GNSS": { "kind": "periodic", "period_ms": 600 },
    "BUFFER": { "kind": "queue", "capacity": 16, "policy": "drop_oldest", "drain_ms": 250 },
    "MODEM": { "kind": "provider", "startup_ms": 300 },
    "UPLINK": { "kind": "pipeline", "work_ms": 400 },
    "FOTA": { "kind": "oneshot", "run_ms": 2500 }
}"#;

/// The power verbs must run on the embassy executor (they await); this
/// mailbox stands in for the wasm page's power coordinator.
static POWER: Channel<CriticalSectionRawMutex, bool, 2> = Channel::new();

#[embassy_executor::task]
async fn coordinator(spawner: Spawner) {
    let sup = build::built().unwrap().sup;
    loop {
        let wake = POWER.receive().await;
        if wake {
            sup.resume_pausable();
            let _ = sup.respawn_terminate(&spawner).await;
        } else {
            sup.teardown().await.expect("teardown");
        }
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
fn sleep_wake_cycle() {
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
            sp.spawn(coordinator(sp).unwrap());
            sp.spawn(supervise(sp).unwrap());
        });
    });

    let modem_hw = registry::resources()
        .iter()
        .find(|r| r.name == "MODEM_HW")
        .unwrap();
    let records = registry::signals()
        .iter()
        .find(|s| s.name == "signals::RECORDS")
        .unwrap();

    // Bring-up. GNSS readiness is asserted by the sweep seeing its first
    // observed-beat write, not by the body.
    assert!(
        settle(
            || by_name("MODEM").node.is_ready()
                && by_name("UPLINK").node.is_running()
                && by_name("GNSS").node.is_ready(),
            6000
        ),
        "bring-up did not settle"
    );
    assert!(
        !modem_hw.slot.is_filled(),
        "the runner was consumed at spawn"
    );
    assert!(!by_name("FOTA").node.is_running(), "FOTA holds disabled");
    assert!(
        settle(|| records.depth.load(Ordering::Relaxed) > 0, 6000),
        "records accumulate"
    );

    // Sleep: reverse-order teardown. Pause nodes park; Terminate nodes exit;
    // the consumed slot stays empty.
    POWER.try_send(false).unwrap();
    assert!(
        settle(
            || !by_name("UPLINK").node.is_running()
                && !by_name("MODEM").node.is_running()
                && !by_name("MOTION").node.is_running(),
            6000
        ),
        "teardown did not quiesce"
    );
    assert!(
        !by_name("MOTION").node.has_exited(),
        "MOTION parked, not exited"
    );
    assert!(
        !by_name("BUFFER").node.has_exited(),
        "BUFFER parked, not exited"
    );
    assert!(
        !modem_hw.slot.is_filled(),
        "the consumed runner does not come back"
    );
    let kept_depth = records.depth.load(Ordering::Relaxed);
    assert!(kept_depth > 0, "the record FIFO survives the sleep");

    // Wake without rebuilding the runner: the modem fails closed on its
    // empty consume slot; the parked nodes resume in place regardless.
    POWER.try_send(true).unwrap();
    assert!(
        settle(|| by_name("MOTION").node.is_running(), 5000),
        "parked MOTION resumes"
    );
    assert!(
        settle(|| by_name("GNSS").node.is_running(), 5000),
        "GNSS respawns"
    );
    advance_ms(1500); // past the 500 ms slot_timeout
    assert!(
        !by_name("MODEM").node.is_running(),
        "the modem fails closed without its runner"
    );
    assert!(
        records.depth.load(Ordering::Relaxed) >= kept_depth,
        "the resumed buffer picked up its kept FIFO"
    );

    // Rebuild the runner, wake again: the whole radio side comes back.
    modem_hw.slot.provide(1);
    POWER.try_send(true).unwrap();
    assert!(
        settle(
            || by_name("MODEM").node.is_ready() && by_name("UPLINK").node.is_running(),
            8000
        ),
        "rebuild + wake brings the radio back"
    );
    assert!(
        !modem_hw.slot.is_filled(),
        "the respawned modem consumed the rebuilt runner"
    );
    assert!(
        !by_name("FOTA").node.is_running(),
        "the disabled latch survived the cycle"
    );
}
