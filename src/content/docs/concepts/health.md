---
title: Health monitoring
description: Heartbeats, the stale sweep and the watchdog feed gate, readiness, epochs, the status line, the bring-up breadcrumb, and why escalation stays yours.
---

<p class="eyebrow">Concepts</p>

# Health monitoring

The supervisor reports; the application decides. Every health input is a
cheap atomic load, and nothing here ever acts on its own.

## Liveness: the heartbeat

With the `liveness` feature, a task's node carries a heartbeat flag.

- `node.beat()` raises it. A fresh spawn counts as a beat, so a node is never
  instantly stale.
- `ticks_since_beat()` converts the flag using a clock read the caller makes
  anyway; `is_stale(max_age)` answers "running but stalled".
- The write-side verbs `beat_put` / `beat_writer` (feature `dataflow`) fold
  the beat into the access itself (they live in the `dataflow` verb set and
  additionally need `liveness`), and an `observed beat` entry can drive the
  heartbeat by polling, with nothing asked of the task.

Anyone who can see the static can beat it: an ISR, a driver callback, a task
that does not know the supervisor exists. That makes the node a
free-standing health handle, not just a task-side API.

## The sweep

`liveness-monitor` polls so you do not have to. Declare a budget on the nodes
whose bodies beat:

```rust
node ESTIMATOR = Terminate, task: estimator, beat_timeout: 1000, beat_window: 3;
```

The sweep checks each budgeted node; `beat_window` consecutive strikes emit a
`Stale` event **once**, and the next beat after a stall emits `Recovered`.
Consume events from anywhere:

```rust
loop {
    let event = embassy_supervisor::wait_health().await;
    match event.kind {
        HealthKind::Stale { ticks } => {
            // your escalation: log, degrade, restart, deactivate...
        }
        HealthKind::Recovered => {}
    }
}
```

**Report-only by design.** Where a subsystem can be cycled safely, feeding
the event to `Supervisor::restart` or `clear_ready()` across a `bound` edge
is reasonable. Where it cannot be, a flight control loop, a motor
commutation task, anything holding physical state, restarting is the wrong
reflex and degrading to a safe mode is the right one. The supervisor cannot
tell those apart, so it does not try.

The graph itself is walkable, no feature needed: `index_of`, `dependents_of`,
`iter_nodes`, and per-node `slot_timeout()` / `ready_deps()` let an app walk
from a node to its place in the topology, which is what a status page or a
recovery policy is built from.

## Watchdog feed gate

A hardware watchdog is the backstop for the hang nothing else reports,
including a hang of the app's own health consumer. `GRAPH.all_fresh()`
(feature `liveness-monitor`) is the sweep's policing as one synchronous
`bool`: `true` unless a running, non-detached node with a `beat_timeout:`
has gone longer than that budget without a beat. Feed the watchdog only
while it holds, and a wedged node resets the chip instead of being kept
alive by an unconditional feeder:

```rust
loop {
    if GRAPH.all_fresh() {
        wd.feed(Duration::from_secs(8));
    }
    Timer::after_secs(2).await;
}
```

`GRAPH.stale_nodes()` yields the culprits, for the log line before the
reset. A node without `beat_timeout:` never appears, and neither does a
stopped, paused, exited or detached one. Both are atomics plus one clock
read per policed node, so they fit in a feeder on any tier. Like the sweep,
they grant a pending `beat()` as they look; unlike it, they have no
`beat_window:` hysteresis, so size the hardware timeout to cover the jitter
the window would absorb. A feeder on a higher-priority executor than the
tier it watches would otherwise mask that tier's hangs (see
[Executors and cores](/concepts/placement/)).

## Readiness

The other half of "healthy": `set_ready()` is a task asserting *I am actually
serving*, and `deps: [X ready]` is a dependent whose spawn waits for it. The
provider side is three calls:

- `set_ready()` once serving;
- `clear_ready()` on a lost link: status, not control. Running dependents
  keep running; future spawns and pool growth defer. Pair with a control
  `Deactivate` for a cascade, or mark the edge `bound` to opt that one
  dependent into a stop-and-restart coupling.
- the pre-spawn reset clears it, so a respawned provider re-asserts.

`wait_ready()` exists for app code with the single-pre-fill-waiter caveat
shared by all latching gates: fan N waiters out through an app-owned `Watch`.

## Epochs

`epochs` adds a generation counter per node: `node.epoch()` and
`wait_epoch_change(seen)`. It answers a question no dep edge can: a *running*
consumer noticing that a provider was restarted underneath it and its cached
negotiation is stale. Pure status; the reaction is yours.

## A status line per node

`node-status` adds `report_status("receiving image")` and `node.status()`:
one descriptive line, cleared on each activation so a fresh instance does not
wear the previous one's last words. Never an event, never acted on; exactly
the right hook for a dashboard or a shell command.

## Bring-up breadcrumb

A reset in the middle of bring-up leaves nothing to say which node hung it.
With the `breadcrumb` feature each node records the last phase a supervisor
drove it through: `node.crumb()` returns a `CrumbPhase` (`Idle`,
`Spawning`, `Started`, `StopSignalled`, `Stopped`, `Faulted`, `Resumed`).
`crumb_word()` gives the last transition of the whole program as one `u32`.
Stash the word somewhere that survives the reset (no-init RAM or a watchdog
scratch register) and decode it at the next boot:

```rust
// in the watchdog feeder or the panic handler: one store per pass
pac::WATCHDOG.scratch(6).write_value(embassy_supervisor::crumb_word());

// at the next boot
if let Some((phase, slot)) = embassy_supervisor::decode_crumb(prev) {
    let name = GRAPH.nodes.get(slot as usize).copied().flatten().map_or("?", |n| n.name());
    warn!("previous run ended at {} {}", name, phase.as_str());
}
```

A bring-up stuck waiting on a node leaves `Spawning`; a stop that was never
acknowledged leaves `StopSignalled`. `decode_crumb` returns `None` when
there was no transition yet or the word is not a crumb. With several graphs
in one program the word cannot say which graph the node belongs to; read
each node's `crumb()` then. Cost: 1 B per node and 4 B overall.

## The app-owned monitor

Rolling your own over the raw surface is a few lines, and stays valid in
every feature combination:

```rust
for (i, node) in GRAPH.iter_nodes() {
    let down = !node.is_running();
    let stalled = node.is_stale(embassy_time::Duration::from_secs(1));
    let unready = !node.is_ready();
    // GRAPH.deps_of(i) / GRAPH.dependents_of(i, ..) for topology context
}
```

A hardware watchdog completes the story: the supervisor catches tasks that
hang; the watchdog catches the crashes it cannot, and the
[feed gate](#watchdog-feed-gate) ties the two together. The
[pattern gallery](/guides/patterns/) shows a supervised watchdog node that
pets itself only while the control loop is fresh.

## Next

[Tracing and profiling](/concepts/trace/) adds
"where did the CPU go" to "who is alive".
