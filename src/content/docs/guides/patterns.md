---
title: Pattern gallery
description: "Working shapes from real devices: provider nodes, control chains, watchdogs, OTA, sleep coordinators, self-checks."
---

<p class="eyebrow">Guides</p>

# Pattern gallery

Declarations that earn their keep, drawn from devices running this stack.
Names are illustrative; swap in your workers.

## The radio provider and its court

One async bring-up builds several correlated objects (a radio: two runners,
a control handle, a network stack handle) that different nodes consume, and
it must re-run every wake cycle. Make the builder a **provider node** and
let gates do the sequencing:

```rust
supervisor_graph! {
    node WIFI_HW = Terminate, task: wifi_hw_task,
        provides: [CYW_RUNNER, NET_RUNNER, WIFI_CONTROL, STACK];

    node CYW43 = Terminate, deps: [WIFI_HW], slot_timeout: 5000,
        task: cyw_runner_task,
        resources: [CYW_RUNNER: local consume CywRunner];

    node NET = Terminate, deps: [WIFI_HW], slot_timeout: 5000,
        task: net_runner_task,
        resources: [NET_RUNNER: local consume NetRunner];

    node WIFI_CTRL = Terminate, deps: [CYW43, NET], slot_timeout: 5000,
        task: wifi_control_task,
        resources: [WIFI_CONTROL: local consume Control<'static>,
                    STACK: shared local Stack<'static>];

    node HTTP = Terminate, deps: [WIFI_CTRL], slot_timeout: 5000,
        task: http_task,
        resources: [STACK: shared local Stack<'static>];
}
```

Why it holds together:

- The provider's worker builds once, `provide()`s into the four slots, then
  parks on `wait_shutdown()`. `provides:` ties the slots to its lifetime.
- The two runners are `local consume`: they are `!Send`, they live on the
  core that built them, and they are dropped at teardown so the pins and DMA
  come back.
- `STACK` is `shared`: one `Copy` handle fanned out to every network user.
  A missing stack is a `ResourceMissing` fault, not a panicking accessor.
- `slot_timeout: 5000` on the consumers covers the radio's bring-up time;
  the 100 ms default assumes provided-before-start.

This is the shape to reach for whenever construction is async, correlated
and repeatable.

## A control chain with a parameter store

Flight-control-style stacks have one ancestor everything depends on (a
parameter store) and a sensor→estimate→control chain where each stage must
be *actually producing* before the next spawns:

```rust
supervisor_graph! {
    executor HIGH;

    node PARAM_STORAGE = Terminate, task: param_storage_task;
    node IMU = Terminate, deps: [PARAM_STORAGE ready],
        executor: HIGH, task: imu_reader_task, beat_timeout: 500;
    node ESTIMATOR = Terminate, deps: [PARAM_STORAGE ready, IMU ready],
        task: estimator_task, slot_timeout: 500, beat_timeout: 1000,
        writes: [crate::signals::ESTIMATE observed beat], ready_on_write;
    node CONTROLLER = Terminate,
        deps: [PARAM_STORAGE ready, ESTIMATOR ready, RC],
        task: controller_task, slot_timeout: 500;
    node MOTORS = Terminate, deps: [PARAM_STORAGE ready],
        task: motor_governor_task;
}
```

Reading it: the IMU runs on the preemptive tier; the estimator's readiness
is its **first published estimate**, not a line of code (`ready_on_write` +
`observed beat` over the estimate's send counter); the controller waits for
both the parameters and a live estimate, and its spawn budget covers the
estimator's convergence. Nobody hand-sequenced anything: each edge states
exactly what it waits for.

## A supervised watchdog

The hardware watchdog catches crashes; the graph feeds it honest inputs. A
watchdog node pets the IWDG only while it should:

```rust
async fn watchdog_task(node: &'static TaskNode) {
    let mut wdt = Iwdg::new(unsafe { Peripherals::steal() }, embassy_time::Duration::from_secs(2));
    loop {
        node.beat();
        // Pet while disarmed, or while the control loop is fresh.
        if !armed() || !crate::CONTROLLER_RATE.is_stale(embassy_time::Duration::from_millis(500)) {
            wdt.pet();
        }
        embassy_time::Timer::after(embassy_time::Duration::from_millis(250)).await;
    }
}
```

`CONTROLLER_RATE` is an ordinary node static the graph emitted; reading it
from another task is a cheap atomic load. When the control loop wedges, the
watchdog stops petting and the hardware resets the board: the escalation
this particular domain deserves, wired with three lines.

## OTA as a control-started subsystem

An updater has no business running until asked, and must coordinate the
things it replaces:

```rust
supervisor_graph! {
    node NET = Terminate, task: net_task;
    node OTA = Terminate, deps: [NET], task: ota_task, disabled,
        resources: [FLASH_DEV: consume Flash<'static>];
    node OTA_CONFIRM = Terminate, deps: [OTA], task: confirm_task;
}
```

An HTTP handler or a button starts it with
`request_control(&OTA, ControlOp::Activate)`; the cascade pulls `NET` up if
it was down. The updater task itself is detached-in-spirit: it drives its
own drain sequence (deactivating what it needs the memory of), flashes,
and reboots. A run-once confirm task after it (see below) reports the
result and exits.

## The sleep and wake coordinator

A detached power task drives the whole graph's sleep cycle, and survives
its own teardowns:

```rust
#[embassy_executor::task]
async fn power_task(node: &'static TaskNode, spawner: Spawner) {
    node.set_detached(true);          // survives the teardown it drives
    loop {
        wait_for_idle().await;
        SUP.teardown().await.ok();    // reverse order; Pause nodes park
        enter_deep_sleep().await;
        SUP.resume_pausable();        // thaw what keeps its state
        SUP.respawn_terminate(&spawner).await.ok(); // fresh state for the rest
    }
}
```

`Pause` nodes keep their bus handles and calibration across the sleep;
`Terminate` nodes come back clean; `disabled` nodes stay down (the latch
survives the wake); the coordinator itself is never a target of its own
verbs.

## Run-once, ordered last

A post-boot self-check that must run after *everything* is up, exactly once
per power cycle:

```rust
supervisor_graph! {
    node NET = Terminate, task: net_task;
    node POOL = Terminate, deps: [NET], task: worker /* ... */;
    node SELF_CHECK = Terminate, deps: [POOL], task: self_check_task, exit: Report;
}
```

```rust
async fn self_check_task(node: &'static TaskNode) -> Result<Report, Aborted> {
    node.set_detached(true);   // run once EVER: the wake respawn skips it
    node.run_cancellable_acked(run_checks()).await
}
```

Being a leaf of everything makes it last in topological order; detaching
keeps the next wake cycle from re-running it; `exit:` parks its report in
`SELF_CHECK_EXIT` for whoever asks.

## One worker, N drivers

The same sensor worker over different chips, without duplicating the body:

```rust
pub async fn poll_sensor<D: Driver>(node: &'static TaskNode, dev: D) {
    while let Ok(v) = node.run_cancellable_acked(dev.sample()).await {
        publish(v);
    }
}

supervisor_graph! {
    node BUS = Terminate, task: bus_worker;
    node BME = Terminate, deps: [BUS], task: poll_sensor::<Bme280>(bme());
    node SHT = Terminate, deps: [BUS], task: poll_sensor(sht());
}
```

Each declaration stamps its own monomorphized shell; the args evaluate at
first poll, on the node's own executor.

## Choosing between shapes

- Async, correlated, repeatable construction → **provider node**.
- "Start only when actually serving" → **`ready` edges**, or `ready_on_write`
  when production itself is the proof.
- Latency-critical stage → **`executor:` tier**, deps still honored.
- On-demand or destructive subsystem → **`disabled` + control**.
- Whole-graph phases → **teardown / respawn pair**, driven by a detached
  coordinator.

## Next

[Testing on your desktop](/guides/testing/) shows
these graphs running under a mock clock, assertions included.
