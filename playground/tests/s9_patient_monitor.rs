//! S9 patient monitor: context before detectors (ready_on_write as a hazard
//! gate), the arrhythmia learning phase, runtime module insert/remove
//! through start_node/stop_node, and the consumed NIBP pneumatics.

use std::sync::atomic::Ordering;
use std::time::Duration as StdDuration;

use embassy_executor::{Executor, Spawner};
use embassy_supervisor::ResourceGate;
use embassy_supervisor_playground::{build, parse, registry};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, MockDriver};

const DSL: &str = r#"
supervisor_graph! {
    node PATIENT_CONTEXT = Terminate,
        task: crate::adm::context_task,
        ready_on_write, beat_timeout: 1000,
        writes: [signals::CONTEXT observed beat];
    node AUDIO_CODEC = Terminate, task: crate::audio::codec_task,
        writes: [signals::AUDIO];
    node ALARM_ARBITER = Terminate,
        deps: [PATIENT_CONTEXT ready], slot_timeout: 4000,
        task: crate::alarm::arbiter_task,
        reads: [signals::ALARM_EVTS, signals::AUDIO],
        writes: [signals::ANNUNCIATE observed];
    node ECG_ACQ = Terminate,
        deps: [PATIENT_CONTEXT ready], slot_timeout: 4000,
        task: crate::ecg::acquire_task,
        writes: [signals::ECG observed];
    node ECG_DSP = Terminate, deps: [ECG_ACQ],
        task: crate::ecg::dsp_task,
        reads: [signals::ECG], writes: [signals::HR observed];
    node ECG_ALARM = Terminate,
        deps: [ECG_DSP, ALARM_ARBITER ready], slot_timeout: 4000,
        task: crate::ecg::alarm_task,
        reads: [signals::HR], writes: [signals::ALARM_EVTS observed];
    node ARRHYTHMIA = Terminate, deps: [ECG_DSP],
        task: crate::ecg::arrhythmia_task,
        reads: [signals::HR], writes: [signals::RHYTHM observed];
    node SPO2_ACQ = OnDemand, task: crate::spo2::acquire_task,
        writes: [signals::PLETH observed];
    node SPO2_DSP = OnDemand, deps: [SPO2_ACQ ready bound],
        slot_timeout: 2000,
        task: crate::spo2::dsp_task,
        reads: [signals::PLETH], writes: [signals::SPO2 observed];
    node SPO2_ALARM = OnDemand,
        deps: [SPO2_DSP ready bound, ALARM_ARBITER ready], slot_timeout: 2000,
        task: crate::spo2::alarm_task,
        reads: [signals::SPO2], writes: [signals::ALARM_EVTS observed];
    node NIBP_SM = OnDemand,
        task: crate::nibp::measure_task,
        resources: [NIBP_PNEUMATICS: consume crate::nibp::Pneumatics],
        writes: [signals::NIBP observed];
}
"#;

const BEHAVIORS: &str = r#"{
    "PATIENT_CONTEXT": { "kind": "periodic", "period_ms": 800 },
    "AUDIO_CODEC": { "kind": "periodic", "period_ms": 500 },
    "ALARM_ARBITER": { "kind": "lease_user", "lease": "signals::AUDIO", "hold_ms": 600 },
    "ECG_ACQ": { "kind": "periodic", "period_ms": 100 },
    "ECG_DSP": { "kind": "pipeline", "work_ms": 150 },
    "ECG_ALARM": { "kind": "control_loop", "period_ms": 300 },
    "ARRHYTHMIA": { "kind": "provider", "startup_ms": 2000 },
    "SPO2_ACQ": { "kind": "periodic", "period_ms": 150 },
    "SPO2_DSP": { "kind": "pipeline", "work_ms": 200 },
    "SPO2_ALARM": { "kind": "control_loop", "period_ms": 300 },
    "NIBP_SM": { "kind": "oneshot", "run_ms": 1500 }
}"#;

enum Cmd {
    Start(&'static embassy_supervisor::TaskNode),
    Stop(&'static embassy_supervisor::TaskNode),
}

impl core::fmt::Debug for Cmd {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Cmd")
    }
}

static CMDS: Channel<CriticalSectionRawMutex, Cmd, 4> = Channel::new();

#[embassy_executor::task]
async fn commander(spawner: Spawner) {
    loop {
        let sup = build::built().unwrap().sup;
        match CMDS.receive().await {
            Cmd::Start(n) => {
                let _ = sup.start_node(n, &spawner).await;
            }
            Cmd::Stop(n) => {
                let _ = sup.stop_node(n).await;
            }
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

fn writes_of(name: &str) -> u32 {
    registry::signals()
        .iter()
        .find(|s| s.name == name)
        .unwrap()
        .writes
        .load(Ordering::Relaxed)
}

#[test]
fn modules_and_hazard_gates() {
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
            sp.spawn(commander(sp).unwrap());
            sp.spawn(supervise(sp).unwrap());
        });
    });

    // Bring-up order is the hazard analysis: context publishes first, then
    // the arbiter, then the detectors gated on it. The arrhythmia analyzer
    // may not publish until its learning phase completes.
    assert!(
        settle(
            || by_name("PATIENT_CONTEXT").node.is_ready()
                && by_name("ALARM_ARBITER").node.is_running()
                && by_name("ECG_ALARM").node.is_running(),
            8000
        ),
        "context -> arbiter -> detectors did not settle"
    );
    assert!(settle(|| by_name("ARRHYTHMIA").node.is_running(), 3000));
    if !by_name("ARRHYTHMIA").node.is_ready() {
        assert_eq!(
            writes_of("signals::RHYTHM"),
            0,
            "no rhythm calls during the learning phase"
        );
    }
    assert!(
        settle(|| by_name("ARRHYTHMIA").node.is_ready(), 5000),
        "learning completes"
    );

    // The audio codec is leased per burst.
    let audio = registry::signals()
        .iter()
        .find(|s| s.name == "signals::AUDIO")
        .unwrap();
    let registry::Gate::Leased(leased) = &audio.gate else {
        panic!("AUDIO should be Leased");
    };
    assert!(
        settle(|| leased.leases() > 0, 4000),
        "the arbiter holds the codec per burst"
    );

    // Insert the SpO2 module: the subtree comes up stage by stage, and its
    // alarm source registers with the arbiter (events flow).
    assert!(
        !by_name("SPO2_ACQ").node.is_running(),
        "OnDemand modules start absent"
    );
    CMDS.try_send(Cmd::Start(by_name("SPO2_ACQ").node)).unwrap();
    assert!(
        settle(|| by_name("SPO2_ACQ").node.is_running(), 4000),
        "sensor inserted"
    );
    CMDS.try_send(Cmd::Start(by_name("SPO2_DSP").node)).unwrap();
    assert!(
        settle(|| by_name("SPO2_DSP").node.is_running(), 4000),
        "processing up"
    );
    CMDS.try_send(Cmd::Start(by_name("SPO2_ALARM").node))
        .unwrap();
    assert!(
        settle(|| by_name("SPO2_ALARM").node.is_running(), 4000),
        "alarms registered"
    );
    let evts = writes_of("signals::ALARM_EVTS");
    assert!(
        settle(|| writes_of("signals::ALARM_EVTS") > evts + 2, 4000),
        "the new module contributes alarm events"
    );

    // Remove it: one stop at the acquisition and the whole subtree follows
    // through the bound edges — the absence is announced, not silent.
    CMDS.try_send(Cmd::Stop(by_name("SPO2_ACQ").node)).unwrap();
    assert!(
        settle(|| !by_name("SPO2_ACQ").node.is_running(), 4000),
        "module removed"
    );
    assert!(
        settle(|| by_name("SPO2_DSP").node.is_bound_stopped(), 4000),
        "the DSP follows the removed sensor down"
    );
    assert!(
        settle(|| by_name("SPO2_ALARM").node.is_bound_stopped(), 4000),
        "the alarm detector retracts with its source"
    );
    advance_ms(1000);
    assert!(
        !by_name("SPO2_ACQ").node.is_running(),
        "an absent module stays absent"
    );

    // NIBP: the measurement cycle consumes the pneumatics and runs to
    // completion; the next cycle fails closed until the cuff is re-armed.
    let pneumatics = registry::resources()
        .iter()
        .find(|r| r.name == "NIBP_PNEUMATICS")
        .unwrap();
    assert!(pneumatics.slot.is_filled(), "armed at boot");
    CMDS.try_send(Cmd::Start(by_name("NIBP_SM").node)).unwrap();
    assert!(
        settle(|| by_name("NIBP_SM").node.is_running(), 4000),
        "cycle started"
    );
    assert!(
        !pneumatics.slot.is_filled(),
        "the cycle owns the pneumatics"
    );
    assert!(
        settle(|| by_name("NIBP_SM").node.has_exited(), 5000),
        "cycle completes"
    );
    assert!(
        !pneumatics.slot.is_filled(),
        "consumed: the cuff needs re-arming"
    );
    CMDS.try_send(Cmd::Start(by_name("NIBP_SM").node)).unwrap();
    advance_ms(1000); // past the default gate budget
    assert!(
        !by_name("NIBP_SM").node.is_running(),
        "no pneumatics, no cycle"
    );
    pneumatics.slot.provide(1);
    CMDS.try_send(Cmd::Start(by_name("NIBP_SM").node)).unwrap();
    assert!(
        settle(|| by_name("NIBP_SM").node.is_running(), 4000),
        "re-armed cycle runs"
    );
}
