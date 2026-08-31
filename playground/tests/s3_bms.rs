//! S3 battery management system: the escalation ladder. A stalled protection
//! loop loses its readiness (the contactors open through the bound edge —
//! the safe state, not a restart); a stalled SoC estimator activates the
//! LIMP limiter; the min:0 balancing pool follows the cell-imbalance dial.

use std::sync::atomic::Ordering;
use std::time::Duration as StdDuration;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor_playground::{build, health, parse, registry};
use embassy_time::{Duration, MockDriver};

const DSL: &str = r#"
supervisor_graph! {
    executor SAFETY;
    node CURRENT = Terminate, executor: SAFETY, task: crate::sense::current_task,
        beat_timeout: 200, beat_window: 2,
        writes: [signals::PACK_CURRENT observed beat];
    node VOLTAGE = Terminate, executor: SAFETY, task: crate::sense::cell_volt_task,
        beat_timeout: 200, beat_window: 2,
        writes: [signals::CELL_VOLTS observed beat];
    node PROTECT = Terminate, executor: SAFETY, deps: [CURRENT, VOLTAGE],
        task: crate::protect::protect_task,
        beat_timeout: 200, beat_window: 2,
        reads: [signals::PACK_CURRENT, signals::CELL_VOLTS],
        writes: [signals::LIMITS observed beat];
    node PRECHARGE = Terminate, task: crate::hv::precharge_task,
        provides: [HV_BUS], slot_timeout: 1500;
    node CONTACTORS = Terminate, deps: [PRECHARGE ready, PROTECT ready bound],
        task: crate::hv::contactor_task,
        resources: [HV_BUS: shared crate::hv::HvBus],
        slot_timeout: 3000,
        reads: [signals::LIMITS];
    pool BALANCE = [OnDemand, OnDemand, OnDemand, OnDemand],
        deps: [VOLTAGE], task: crate::balance::bleed_task,
        policy: DeferredShrink::new(Duration::from_secs(2)),
        min: 0, max: 4, reads: [signals::CELL_VOLTS];
    node SOC = Terminate, deps: [CURRENT, VOLTAGE],
        task: crate::soc::estimate_task, beat_timeout: 400,
        reads: [signals::PACK_CURRENT, signals::CELL_VOLTS],
        writes: [signals::SOC observed beat];
    node LIMP = Terminate, disabled, task: crate::soc::limp_task,
        writes: [signals::LIMITS];
}
"#;

const BEHAVIORS: &str = r#"{
    "nodes": {
        "CURRENT": { "kind": "periodic", "period_ms": 100 },
        "VOLTAGE": { "kind": "periodic", "period_ms": 100 },
        "PROTECT": { "kind": "control_loop", "period_ms": 100 },
        "PRECHARGE": { "kind": "provider", "startup_ms": 800 },
        "CONTACTORS": { "kind": "control_loop", "period_ms": 200 },
        "BALANCE": { "kind": "session", "busy_ms": 400 },
        "SOC": { "kind": "control_loop", "period_ms": 200 },
        "LIMP": { "kind": "periodic", "period_ms": 300 }
    },
    "escalation": {
        "PROTECT": "clear_ready",
        "SOC": "activate:LIMP"
    }
}"#;

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
fn safety_ladder() {
    let mut outcome = parse::parse(DSL);
    assert!(
        outcome.ok,
        "parse errors: {:?}",
        outcome.errors.iter().map(|e| &e.msg).collect::<Vec<_>>()
    );
    let built = build::build(outcome.model.take().unwrap(), BEHAVIORS).expect("build");
    assert_eq!(built.escalations.len(), 2, "the escalation wrapper parsed");

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
            sp.spawn(health_driver().unwrap());
            sp.spawn(supervise(sp).unwrap());
        });
    });

    // Bring-up: precharge reaches threshold, then the contactors close.
    assert!(
        settle(
            || by_name("PRECHARGE").node.is_ready() && by_name("CONTACTORS").node.is_running(),
            6000
        ),
        "precharge then contactors did not settle"
    );
    assert!(settle(|| by_name("PROTECT").node.is_ready(), 3000));

    // The min:0 balancing pool warms one member, then follows the dial.
    assert!(
        settle(|| by_name("BALANCE#0").node.is_running(), 6000),
        "one warm bleed channel"
    );
    for m in ["BALANCE#0", "BALANCE#1", "BALANCE#2", "BALANCE#3"] {
        by_name(m).input.store(3.0f32.to_bits(), Ordering::Relaxed);
    }
    assert!(
        settle(|| by_name("BALANCE#2").node.is_running(), 10000),
        "three unbalanced cells open three channels"
    );
    for m in ["BALANCE#0", "BALANCE#1", "BALANCE#2", "BALANCE#3"] {
        by_name(m).input.store(0.0f32.to_bits(), Ordering::Relaxed);
    }

    // Stall the protection loop: the policy withdraws its readiness and the
    // contactors OPEN through the bound edge — no restart of a wedged loop.
    let protect_epoch = by_name("PROTECT").node.epoch();
    by_name("PROTECT")
        .fault
        .store(registry::fault::STALL, Ordering::Relaxed);
    assert!(
        settle(|| !by_name("PROTECT").node.is_ready(), 5000),
        "the policy withdraws PROTECT's readiness"
    );
    assert!(
        settle(|| by_name("CONTACTORS").node.is_bound_stopped(), 5000),
        "the contactors open (bound-stop) — the safe state"
    );
    assert!(
        by_name("PROTECT").node.is_running(),
        "the wedged loop was not restarted"
    );
    assert_eq!(
        by_name("PROTECT").node.epoch(),
        protect_epoch,
        "no respawn happened"
    );

    // Stall the SoC estimator: the policy activates the disabled LIMP
    // limiter, a second writer of the same limits signal.
    assert!(!by_name("LIMP").node.is_running());
    by_name("SOC")
        .fault
        .store(registry::fault::STALL, Ordering::Relaxed);
    assert!(
        settle(|| by_name("LIMP").node.is_running(), 8000),
        "the policy activates LIMP"
    );
    let limits = registry::signals()
        .iter()
        .find(|s| s.name == "signals::LIMITS")
        .unwrap();
    let w = limits.writes.load(Ordering::Relaxed);
    assert!(
        settle(|| limits.writes.load(Ordering::Relaxed) > w + 2, 3000),
        "LIMP keeps the limits stream alive"
    );
}
