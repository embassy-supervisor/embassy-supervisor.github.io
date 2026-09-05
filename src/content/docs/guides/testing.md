---
title: Testing on your desktop
description: Run the same supervised graph on your workstation with a mock clock, and assert on lifecycle behavior.
---

<p class="eyebrow">Guides</p>

# Testing on your desktop

The library is HAL-free, so a graph runs on a desktop: same macro, same
nodes, same lifecycle semantics, under `cargo test`. embassy-executor's std
support provides the executor thread, and embassy-time's mock driver
provides the clock.

## The harness

About fifteen lines:

```rust
// tests/graph.rs
static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[embassy_executor::task]
async fn driver(spawner: embassy_executor::Spawner) {
    let sup = embassy_supervisor::Supervisor::new(&GRAPH);
    sup.start(&spawner).await.expect("bring-up");

    // ... assertions, control calls, teardown/start cycles ...

    DONE.store(true, std::sync::atomic::Ordering::Release);
}

#[test]
fn graph_cycles() {
    std::thread::spawn(|| {
        let ex: &'static mut embassy_executor::Executor =
            Box::leak(Box::new(embassy_executor::Executor::new()));
        ex.run(|spawner| spawner.spawn(driver(spawner).unwrap()));
    });
    while !DONE.load(std::sync::atomic::Ordering::Acquire) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
```

Dev-dependencies for the harness:

```toml
[dev-dependencies]
embassy-executor = { version = "0.10", features = ["platform-std", "executor-thread"] }
embassy-time = { version = "0.5", features = ["mock-driver"] }
critical-section = { version = "1", features = ["std"] }
```

Turn on the supervisor's `log` feature in the test build too: without it,
bring-up, teardown and stale reports print nothing on a host, which is the
one place they are most useful.

`Box::leak` for the executor is the one deliberate static-promotion trick
the executor's API needs (`run` takes `&'static mut self`); it exists only
in the test binary.

## The mock clock

A frozen clock is fine on happy paths: every wait resolves by signal (acks,
slot fills, readiness), because the internal timeouts exist to convert a
*failure* into an error. Advance the clock only when a test wants to observe
one:

```rust
let clock = embassy_time::MockDriver::get();
clock.advance(embassy_time::Duration::from_millis(500));
```

Advance to observe a `ShutdownTimeout` (a node that never acks), a gate
fault (`ResourceMissing`, `ExecutorSlotEmpty`, `ReadyDepTimeout`), or
`is_stale` flipping after a `beat_timeout`. Cross-thread advance is sound.

## What to assert

Lifecycle behavior is all observable through the node statics, and the
guarantees are cross-thread:

```rust
// Bring-up order: read is_running() as the wave completes.
assert!(NET.is_running() && HTTP.is_running());

// Gate failures: an unprovided resource or an unasserted readiness comes
// back from start() as a NodeFault naming the node and the gate.
let fault = sup.start(&spawner).await.unwrap_err();
assert_eq!(fault.node.name(), "http"); // e.g. ReadyDepTimeout { dep: NET }

// Control: apply_control from the test, then:
assert!(OTA.is_disabled());
assert!(!OTA.is_running());

// Exit values:
sup.stop_node(&PROBE).await.unwrap();
let report = PROBE_EXIT.take();
```

A useful pattern for dataflow claims: with the mock clock, bring the graph
up, inject into a producer's signal, and assert the consumer's beat advanced
(`ticks_since_beat`) within a bounded interval. Only the test can build a
valid sample, which is why this lives in app tests rather than the library.

## Simulated sensors

Nothing stops a test graph from declaring nodes whose workers are
simulators: a fake IMU writing `IMU_DATA` on a virtual ticker, a fake
battery driving the failsafe path, a scripted mission feeding a navigator.
The device graph and the test graph share the workers worth sharing; the
test swaps the hardware edges for simulated ones and runs scenarios in
virtual time. Real firmware teams use this to fly whole missions in under a
second of wall time, lockstep, on every commit.

## Fault injection

Enable the `fault-inject` feature to make nodes fail without changing worker code:

```rust
STALLER.inject(Fault::Stall)?;                          // shell stops polling the worker
WEDGER.inject(Fault::Wedge)?;                           // node hides the stop and swallows the ack
CRASHER.inject(Fault::Crash)?;                          // worker future is dropped
HOGGER.inject(Fault::Hog(Duration::from_millis(400)))?; // executor busy-spins
STALLER.clear_fault();                                  // replay withheld events and wake
```

Expected effects:

- **Stall**: monitor reports `Stale`, then `Recovered` after clear; stops still ack.
- **Wedge**: `stop_node` returns `ShutdownTimeout` while the node stays running, then a late ack after clear. A worker that acks or exits while wedged drops its provided value in its own executor context.
- **Crash**: `has_exited()` becomes true, lent resources are restored, the `exit:` slot is empty, and `restart` respawns the worker.
- **Hog**: shows as one jump in mock time between two consecutive yields of the test task.

Stall, crash, and hog require the `task:` shell. Hand-written `spawn:` tasks return `InjectError::NoShell`. See the crate's `fault_inject` test for a full example.

## Next

[Diagram and lint tools](/guides/tools/) for keeping
the declaration itself honest in CI.
