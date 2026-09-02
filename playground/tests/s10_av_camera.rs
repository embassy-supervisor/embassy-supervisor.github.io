//! S10 edge A/V streaming head: the two-phase frame-pool drain (stopping the
//! allocator refuses new leases and waits out the holders instead of
//! deadlocking), the consume encoder channel (killed -> fail-closed until
//! rebuilt, sessions surviving), and the clock servo holding last-good when
//! PTP goes quiet.

use std::sync::atomic::Ordering;
use std::time::Duration as StdDuration;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor::{ControlOp, try_request_control};
use embassy_supervisor_playground::{build, parse, registry};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, MockDriver};

const DSL: &str = r#"
supervisor_graph! {
    executor CAPTURE;
    executor MEDIA;
    node VI_CAPTURE = Terminate, executor: CAPTURE,
        task: crate::vi::capture_task,
        resources: [VI_PIPE: consume crate::vi::ViPipe],
        beat_timeout: 400,
        writes: [signals::RAW_FRAMES observed beat];
    node PTP_TS = Terminate, executor: CAPTURE, task: crate::ptp::hw_stamp_task,
        writes: [signals::PHC observed];
    node VB_ALLOC = Terminate, executor: MEDIA, task: crate::vb::pool_task,
        writes: [signals::FRAMES];
    node AAA_LOOP = Terminate, executor: MEDIA, deps: [VI_CAPTURE],
        task: crate::isp::aaa_task,
        resources: [SENSOR_I2C: crate::isp::SensorI2c],
        reads: [signals::RAW_FRAMES], writes: [signals::EXPOSURE observed];
    node VPSS_MAIN = Terminate, executor: MEDIA, deps: [VI_CAPTURE, VB_ALLOC],
        task: crate::vpss::scale_task,
        reads: [signals::RAW_FRAMES, signals::FRAMES],
        writes: [signals::SCALED observed];
    node VENC_PRIMARY = Terminate, executor: MEDIA,
        deps: [AAA_LOOP ready, VPSS_MAIN], slot_timeout: 4000,
        task: crate::venc::encode_task,
        resources: [VENC_CH: consume crate::venc::Channel],
        ready_on_write, beat_timeout: 800,
        reads: [signals::SCALED], writes: [signals::BITSTREAM observed beat];
    node PTP_SERVO = Terminate, deps: [PTP_TS], task: crate::ptp::servo_task,
        reads: [signals::PHC], writes: [signals::CLOCK observed];
    node AV_TIMESTAMPER = Terminate, executor: MEDIA,
        deps: [PTP_SERVO ready], slot_timeout: 3000,
        task: crate::av::timestamp_task,
        ready_on_write, beat_timeout: 800,
        reads: [signals::BITSTREAM, signals::CLOCK],
        writes: [signals::AV observed beat];
    node SESSION_Q = Terminate, deps: [AV_TIMESTAMPER],
        task: crate::rtp::session_queue_task,
        reads: [signals::AV], writes: [signals::TX observed];
    pool RTP_SESSION = [OnDemand, OnDemand, OnDemand, OnDemand],
        deps: [], slot_timeout: 3000,
        task: crate::rtp::sender_task,
        policy: DeferredShrink::new(Duration::from_secs(2)),
        min: 0, max: 4, reads: [signals::TX];
}
"#;

const BEHAVIORS: &str = r#"{
    "VI_CAPTURE": { "kind": "periodic", "period_ms": 100 },
    "PTP_TS": { "kind": "periodic", "period_ms": 500 },
    "VB_ALLOC": { "kind": "periodic", "period_ms": 200 },
    "AAA_LOOP": { "kind": "control_loop", "period_ms": 150 },
    "VPSS_MAIN": { "kind": "lease_user", "lease": "signals::FRAMES", "hold_ms": 120 },
    "VENC_PRIMARY": { "kind": "pipeline", "work_ms": 120 },
    "PTP_SERVO": { "kind": "control_loop", "period_ms": 400 },
    "AV_TIMESTAMPER": { "kind": "pipeline", "work_ms": 100 },
    "SESSION_Q": { "kind": "queue", "capacity": 8, "policy": "drop_oldest", "drain_ms": 150 },
    "RTP_SESSION": { "kind": "session", "busy_ms": 300 }
}"#;

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

fn writes_of(name: &str) -> u32 {
    registry::signals()
        .iter()
        .find(|s| s.name == name)
        .unwrap()
        .writes
        .load(Ordering::Relaxed)
}

#[test]
fn frame_pool_and_encoder_lifecycle() {
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
            sp.spawn(stopper().unwrap());
            sp.spawn(supervise(sp).unwrap());
        });
    });

    // Bring-up: the encoder asserts readiness through its first bitstream
    // write, the timestamper through its first stamped frame; the frame pool
    // hands out leases.
    assert!(
        settle(
            || by_name("VENC_PRIMARY").node.is_ready() && by_name("AV_TIMESTAMPER").node.is_ready(),
            10000
        ),
        "media chain did not settle"
    );
    let frames = registry::signals()
        .iter()
        .find(|s| s.name == "signals::FRAMES")
        .unwrap();
    let registry::Gate::Leased(leased) = &frames.gate else {
        panic!("FRAMES should be Leased");
    };
    assert!(
        settle(|| leased.leases() > 0, 4000),
        "the scaler holds frame leases"
    );

    // Two-phase pool teardown: stopping the allocator drains first — new
    // leases refused, live ones waited out — then the stop acks. A naive
    // drop would deadlock a blocked acquirer instead.
    STOP.try_send(by_name("VB_ALLOC").node).unwrap();
    assert!(
        settle(|| !by_name("VB_ALLOC").node.is_running(), 8000),
        "the allocator stops once the holders released"
    );
    assert!(
        leased.is_drained(),
        "the pool refuses new frames after teardown"
    );
    assert!(leased.lease().is_none(), "a drained pool hands out nothing");

    // Restarting the allocator reopens the pool.
    try_request_control(by_name("VB_ALLOC").node, ControlOp::Restart).unwrap();
    assert!(settle(|| by_name("VB_ALLOC").node.is_running(), 5000));
    assert!(
        settle(|| leased.leases() > 0, 5000),
        "the reopened pool hands out leases again"
    );

    // The restart was rest_for_one: it cascaded through the scaler into the
    // encoder — whose consume channel was spent at its original spawn. The
    // cascade's respawn therefore FAILED CLOSED: restarting the frame pool
    // forces an encoder-channel rebuild, exactly the real MPP contract.
    let venc_ch = registry::resources()
        .iter()
        .find(|r| r.name == "VENC_CH")
        .unwrap();
    assert!(
        !venc_ch.slot.is_filled(),
        "the channel was consumed at the original spawn"
    );
    advance_ms(5000); // past the 4000 ms slot_timeout
    assert!(
        !by_name("VENC_PRIMARY").node.is_running(),
        "no channel, no encoder: the cascade fail-closed"
    );
    assert!(
        settle(|| by_name("RTP_SESSION#0").node.is_running(), 8000),
        "the sessions survive the outage"
    );
    venc_ch.slot.provide(1);
    try_request_control(by_name("VENC_PRIMARY").node, ControlOp::Restart).unwrap();
    assert!(
        settle(|| by_name("VENC_PRIMARY").node.is_running(), 8000),
        "a rebuilt channel unblocks the encoder"
    );

    // Lose PTP: the servo holds last-good and the streams stay smooth — the
    // slow failure a heartbeat cannot catch.
    by_name("PTP_TS")
        .fault
        .store(registry::fault::STALL, Ordering::Relaxed);
    advance_ms(1000);
    let av = writes_of("signals::AV");
    advance_ms(1500);
    assert!(
        writes_of("signals::AV") > av + 3,
        "streams stay smooth on a drifting clock"
    );
    assert!(
        by_name("PTP_SERVO").node.is_running(),
        "the servo never looks unhealthy"
    );
}
