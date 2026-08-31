//! Native guard for the data-deps path: a graph with NO `deps:` edges where
//! a gated consumer's first read demand-starts its producer through the real
//! `Backed::ensure` -> control queue -> supervisor path, plus `Leased`
//! drain/reopen.

use std::sync::atomic::Ordering;
use std::time::Duration as StdDuration;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor_playground::{build, parse, registry};
use embassy_time::{Duration, MockDriver};

// No deps: anywhere — ordering emerges from data. TELEMETRY is Backed, so
// CLOUD_SYNC's open() demand-starts SENSOR_HUB (Terminate + disabled).
const DSL: &str = r#"
supervisor_graph! {
    node SENSOR_HUB = Terminate, task: hub_task, disabled,
        writes: [TELEMETRY observed];
    node CLOUD_SYNC = Terminate, task: cloud_task,
        reads: [TELEMETRY];
    node CONFIG_STORE = Terminate, task: config_task,
        writes: [CONFIG];
    node WORKER_A = Terminate, task: worker_task, reads: [CONFIG];
    node WORKER_B = Terminate, task: worker_task, reads: [CONFIG];
}
"#;

const BEHAVIORS: &str = r#"{
    "SENSOR_HUB": { "kind": "periodic", "period_ms": 100 },
    "CLOUD_SYNC": { "kind": "gated_consumer", "open": "TELEMETRY", "period_ms": 100 },
    "CONFIG_STORE": { "kind": "periodic", "period_ms": 500 },
    "WORKER_A": { "kind": "lease_user", "lease": "CONFIG", "hold_ms": 300 },
    "WORKER_B": { "kind": "lease_user", "lease": "CONFIG", "hold_ms": 300 }
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
        .unwrap()
}

#[test]
fn demand_start_and_leases() {
    let mut outcome = parse::parse(DSL);
    assert!(
        outcome.ok,
        "parse errors: {:?}",
        outcome.errors.iter().map(|e| &e.msg).collect::<Vec<_>>()
    );
    let built = build::build(outcome.model.take().unwrap(), BEHAVIORS).expect("build");
    assert!(built.named_executors.is_empty());

    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|sp| sp.spawn(supervise(sp).unwrap()));
    });

    // SENSOR_HUB is disabled at boot; nothing else references it via deps:.
    // CLOUD_SYNC's open() must Activate it through the control queue.
    assert!(
        settle(|| by_name("SENSOR_HUB").node.is_running(), 5000),
        "gated open should demand-start the disabled producer"
    );
    let telemetry = registry::signals()
        .iter()
        .find(|s| s.name == "TELEMETRY")
        .unwrap();
    assert!(
        settle(|| telemetry.reads.load(Ordering::Relaxed) > 2, 3000),
        "consumer should be reading"
    );

    // Leases: both workers hold and release; the gauge moves.
    let config = registry::signals()
        .iter()
        .find(|s| s.name == "CONFIG")
        .unwrap();
    let registry::Gate::Leased(leased) = &config.gate else {
        panic!("CONFIG should be Leased");
    };
    assert!(
        settle(|| leased.leases() > 0, 3000),
        "workers should hold leases"
    );

    // Drain (the wasm command task's path), driven from a helper thread
    // while the main thread keeps advancing virtual time.
    let l2: &'static embassy_supervisor::Leased<std::sync::atomic::AtomicU32> = leased;
    std::thread::spawn(move || futures_executor_block_on_drain(l2));
    assert!(
        settle(|| leased.is_drained(), 5000),
        "drain should empty the lease gauge"
    );
    assert!(
        leased.lease().is_none(),
        "a drained signal hands out no leases"
    );

    leased.reopen();
    assert!(
        settle(|| leased.leases() > 0, 3000),
        "reopen should restore lease handout"
    );
}

/// Minimal block_on for the async drain (no futures crate dependency).
fn futures_executor_block_on_drain(
    l: &'static embassy_supervisor::Leased<std::sync::atomic::AtomicU32>,
) {
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};
    struct T(std::thread::Thread);
    impl Wake for T {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(T(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut fut = std::pin::pin!(l.drain());
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(()) => return,
            Poll::Pending => std::thread::park_timeout(StdDuration::from_millis(1)),
        }
    }
}
