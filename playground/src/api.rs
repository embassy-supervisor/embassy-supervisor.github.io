//! The wasm <-> JS surface.
//!
//! Contract: one `start_run()` per wasm instance (the page re-instantiates
//! the module for every run — statics are never reset in place), `tick()`
//! advances the virtual mock clock from the page's rAF loop, and
//! `drain_events()` returns one snapshot object per frame.

use embassy_executor::{Executor, Spawner};
use embassy_supervisor::{ControlOp, Fault, InjectError, try_request_control};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, MockDriver};
use std::sync::atomic::Ordering;
use wasm_bindgen::prelude::*;

use crate::behavior::Behavior;
use crate::registry::Gate;
use crate::{build, events, parse, registry};

fn js_err(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

/// Parse DSL text: the real syntax-crate parser plus the playground's badge,
/// signal and cycle analysis. Pure; safe to call on every editor debounce.
#[wasm_bindgen]
pub fn parse_dsl(src: &str) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&parse::parse(src)).map_err(js_err)
}

enum Cmd {
    Drain(usize),
    Reopen(usize),
    NodeStart(usize),
    NodeStop(usize),
    NodeResume(usize),
}

static CMDS: Channel<CriticalSectionRawMutex, Cmd, 8> = Channel::new();

/// The power coordinator's mailbox: `true` = wake, `false` = sleep.
static POWER: Channel<CriticalSectionRawMutex, bool, 2> = Channel::new();

#[embassy_executor::task]
async fn command_task(spawner: Spawner) {
    loop {
        match CMDS.receive().await {
            Cmd::Drain(i) => {
                if let Some(s) = registry::signals().get(i)
                    && let Gate::Leased(l) = &s.gate
                {
                    log::info!("{}: draining leases", s.name);
                    l.drain().await;
                    log::info!("{}: drained", s.name);
                }
            }
            Cmd::Reopen(i) => {
                if let Some(s) = registry::signals().get(i)
                    && let Gate::Leased(l) = &s.gate
                {
                    l.reopen();
                    log::info!("{}: reopened", s.name);
                }
            }
            Cmd::NodeStart(i) => {
                let sup = build::built().expect("built").sup;
                if let Some(rt) = registry::nodes().get(i)
                    && let Err(e) = sup.start_node(rt.node, &spawner).await
                {
                    log::warn!("start_node {}: {e}", rt.node.name());
                }
            }
            Cmd::NodeStop(i) => {
                let sup = build::built().expect("built").sup;
                if let Some(rt) = registry::nodes().get(i)
                    && let Err(e) = sup.stop_node(rt.node).await
                {
                    log::warn!("stop_node {}: {e}", rt.node.name());
                }
            }
            Cmd::NodeResume(i) => {
                let sup = build::built().expect("built").sup;
                if let Some(rt) = registry::nodes().get(i) {
                    sup.resume_node(rt.node);
                }
            }
        }
    }
}

#[embassy_executor::task]
async fn health_task() {
    crate::health::drive().await
}

/// The detached power coordinator: the README's own recipe, driving the
/// whole-graph sleep/wake verbs from a hand-spawned task that teardown and
/// respawn both skip (it is detached).
#[embassy_executor::task]
async fn power_task(idx: usize, spawner: Spawner) {
    let rt = registry::node_rt(idx);
    rt.node.set_detached(true);
    rt.node.adopt_current().await;
    rt.node.report_status("awake");
    let sup = build::built().expect("built").sup;
    loop {
        let wake = POWER.receive().await;
        if !wake {
            rt.node.report_status("sleeping");
            log::info!("power: sleep — tearing down");
            if let Err(e) = sup.teardown().await {
                log::error!("power: teardown fault: {e}");
            }
            log::info!("power: graph quiesced");
        } else {
            rt.node.report_status("awake");
            log::info!("power: wake — resuming parked nodes, respawning");
            sup.resume_pausable();
            if let Err(e) = sup.respawn_terminate(&spawner).await {
                log::error!("power: respawn fault: {e}");
            }
            log::info!("power: graph back up");
        }
    }
}

#[embassy_executor::task]
async fn supervise(spawner: Spawner) {
    loop {
        let fault = build::drive_supervisor(&spawner).await;
        events::push_fault(&fault);
        log::error!("supervisor faulted: {fault}");
    }
}

/// Parse + build + start. Returns the `ParseOutcome` JSON either way; the
/// graph only starts when it parses clean. Call once per wasm instance.
#[wasm_bindgen]
pub fn start_run(src: &str, behaviors_json: &str) -> Result<JsValue, JsValue> {
    console_error_panic_hook::set_once();
    events::install_logger();

    let mut outcome = parse::parse(src);
    if !outcome.ok {
        return serde_wasm_bindgen::to_value(&outcome).map_err(js_err);
    }
    let model = outcome.model.take().expect("ok outcome has a model");
    let mut out = parse::parse(src);
    let built = build::build(model, behaviors_json).map_err(js_err)?;
    // Sharp edges only visible once behaviors are known.
    out.badges.extend(built.badges.iter().cloned());
    let out_js = serde_wasm_bindgen::to_value(&out).map_err(js_err)?;

    // Named executors first, so their SpawnerSlots are filled before the
    // supervisor spawns onto them.
    for (_, slot) in &built.named_executors {
        let slot = *slot;
        let executor = Box::leak(Box::new(Executor::new()));
        executor.start(move |sp| slot.set(sp.make_send()));
    }
    let executor = Box::leak(Box::new(Executor::new()));
    executor.start(|spawner| {
        spawner.spawn(command_task(spawner).unwrap());
        spawner.spawn(health_task().unwrap());
        // A parked node with a power_coordinator behavior gets the real
        // coordinator, spawned by hand where the Spawner is in reach.
        for (i, rt) in registry::nodes().iter().enumerate() {
            if matches!(rt.behavior, Behavior::PowerCoordinator) {
                spawner.spawn(power_task(i, spawner).unwrap());
            }
        }
        spawner.spawn(supervise(spawner).unwrap());
    });
    Ok(out_js)
}

/// Advance virtual time. Timers only fire through this; the page's rAF loop
/// calls it with the (speed-scaled) frame delta.
#[wasm_bindgen]
pub fn tick(advance_us: f64) {
    MockDriver::get().advance(Duration::from_micros(advance_us as u64));
}

/// Drain everything that happened since the last call: logs, node states,
/// signal counters, pool stats, resources, executor rows, health events,
/// faults.
#[wasm_bindgen]
pub fn drain_events() -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&events::snapshot()).map_err(js_err)
}

/// Queue a control operation for a node, addressed by its snapshot index.
#[wasm_bindgen]
pub fn control(node_idx: u8, op: &str) -> Result<(), JsValue> {
    let rt = registry::nodes()
        .get(node_idx as usize)
        .ok_or_else(|| js_err("no node at that index"))?;
    let op = match op {
        "activate" => ControlOp::Activate,
        "deactivate" => ControlOp::Deactivate,
        "restart" => ControlOp::Restart,
        other => return Err(js_err(format!("unknown control op: {other}"))),
    };
    try_request_control(rt.node, op).map_err(|_| js_err("control queue full"))
}

/// Single-node lifecycle verbs: `start` (`start_node` — how a standalone
/// OnDemand node comes up), `stop` (`stop_node` — a Pause node parks), and
/// `resume` (`resume_node` — a parked Pause node picks up in place).
#[wasm_bindgen]
pub fn node_command(node_idx: u8, op: &str) -> Result<(), JsValue> {
    let i = node_idx as usize;
    registry::nodes()
        .get(i)
        .ok_or_else(|| js_err("no node at that index"))?;
    let cmd = match op {
        "start" => Cmd::NodeStart(i),
        "stop" => Cmd::NodeStop(i),
        "resume" => Cmd::NodeResume(i),
        other => return Err(js_err(format!("unknown node command: {other}"))),
    };
    CMDS.try_send(cmd).map_err(|_| js_err("command queue full"))
}

/// Drive the power coordinator: `sleep` tears the graph down (the parked
/// Pause nodes park, `consume` slots empty), `wake` resumes and respawns.
#[wasm_bindgen]
pub fn power(cmd: &str) -> Result<(), JsValue> {
    let wake = match cmd {
        "wake" => true,
        "sleep" => false,
        other => return Err(js_err(format!("unknown power command: {other}"))),
    };
    POWER.try_send(wake).map_err(|_| js_err("power queue full"))
}

/// Drive a resource slot by hand: `provide` refills it (the rebuild step a
/// `consume` teardown demands), `clear` empties it.
#[wasm_bindgen]
pub fn resource_command(name: &str, cmd: &str) -> Result<(), JsValue> {
    let rt = registry::resources()
        .iter()
        .find(|r| r.name == name)
        .ok_or_else(|| js_err(format!("unknown resource `{name}`")))?;
    match cmd {
        "provide" => {
            // A budget comes back at its last provided capacity; a slot
            // takes the unit value main would hand over.
            let v = match rt.kind {
                registry::ResKind::Divisible => rt.capacity.load(Ordering::Relaxed).max(1),
                _ => 1,
            };
            rt.slot.provide(v);
            log::info!("{name}: provided by hand");
        }
        "clear" => {
            rt.slot.clear();
            log::info!("{name}: cleared by hand");
        }
        other => return Err(js_err(format!("unknown resource command: {other}"))),
    }
    Ok(())
}

/// Widget input: sensor value, link up/down (>= 0.5 is up), pool load dial.
#[wasm_bindgen]
pub fn set_input(node: &str, value: f64) -> Result<(), JsValue> {
    let mut hit = false;
    let mut pool_dial = false;
    for rt in registry::nodes() {
        // A pool's dial addresses every member by the pool name.
        if rt.model.name == node || rt.model.pool.as_deref() == Some(node) {
            rt.input.store((value as f32).to_bits(), Ordering::Relaxed);
            pool_dial |= rt.model.pool.is_some();
            hit = true;
        }
    }
    // A pool dial is demand: re-evaluate scaling even when no member runs
    // (a min:0 pool has nobody to mark itself busy).
    if pool_dial {
        embassy_supervisor::request_scale();
    }
    hit.then_some(())
        .ok_or_else(|| js_err(format!("unknown node `{node}`")))
}

/// Inject a fault through the crate's `fault-inject`.
#[wasm_bindgen]
pub fn inject(node_idx: u8, kind: &str) -> Result<(), JsValue> {
    let rt = registry::nodes()
        .get(node_idx as usize)
        .ok_or_else(|| js_err("no node at that index"))?;
    let fault = match kind {
        "stall" => Fault::Stall,
        "wedge" => Fault::Wedge,
        "crash" => Fault::Crash,
        "clear" => Fault::None,
        "hog" => {
            return Err(js_err(
                "hog is not available in the playground: every executor polls on the browser's one thread against a virtual clock, so a spin would never end",
            ));
        }
        other => return Err(js_err(format!("unknown fault: {other}"))),
    };
    rt.node.inject(fault).map_err(|e| {
        js_err(match e {
            InjectError::NoShell => format!(
                "{}: no shell; a node without a `task:` only takes wedge",
                rt.node.name()
            ),
            _ => format!("{}: {e:?}", rt.node.name()),
        })
    })
}

#[wasm_bindgen]
pub fn signal_command(signal_idx: usize, cmd: &str) -> Result<(), JsValue> {
    let cmd = match cmd {
        "drain" => Cmd::Drain(signal_idx),
        "reopen" => Cmd::Reopen(signal_idx),
        other => return Err(js_err(format!("unknown signal command: {other}"))),
    };
    CMDS.try_send(cmd).map_err(|_| js_err("command queue full"))
}
