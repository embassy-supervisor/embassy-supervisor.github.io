---
title: Lifecycle and modes
description: What each supervisor operation does to a node, by mode, how the stop and start waves work, and the events and policies around them.
---

<p class="eyebrow">Concepts</p>

# Lifecycle and modes

A node's mode decides what each lifecycle transition does to it. Three modes
and three cross-cutting flags cover everything.

| mode | at boot | on teardown | on wake re-bring-up |
|---|---|---|---|
| `Terminate` | spawned | exits and acks | **respawned** with a fresh body |
| `Pause` | spawned (or app-spawned) | acks, then parks on `wait_resume()` | **resumed in place**, keeps held resources |
| `OnDemand` | not started | stopped if running | left down; the pool policy regrows it |

```mermaid
stateDiagram-v2
    accDescr: The states a node moves through, and the transition that causes each
    direction LR
    [*] --> Running : start (dep order)
    Running --> Stopped : stop / teardown<br/>Terminate: exits
    Stopped --> Running : respawn_terminate<br/>activate
    Running --> Parked : stop / teardown<br/>Pause: acks then parks
    Parked --> Running : resume_pausable<br/>resume_node (in place)
    Running --> Exited : body returns<br/>mark_exited
    Exited --> Running : Activate<br/>(control respawn)
```

Three flags cut across the modes:

- **`disabled`** (the `disabled;` clause, an `enabled_if:` predicate that
  returned false at `start()`, or a control `Deactivate` on the node
  itself) is the "someone said stop" latch. Every bring-up path honors it,
  so a manual stop survives a wake respawn or an elastic regrow, until an
  `Activate` clears it.
- **`collateral`** (`TaskNode::is_collateral()`) marks a node stopped only
  as a *dependent* of a deactivated node. It blocks bring-up exactly like
  `disabled`, but `activate` on the ancestor releases it once no disabled
  node remains among its transitive dependencies. `start_node` overrides
  the hold.
- **`detached`** (`set_detached(true)`) is full hands-off. The supervisor
  starts a detached node once, then never drives it again: teardown,
  cascades, stop and respawn all skip it. Its `deps:` still order its first
  spawn.

## The operation matrix

The canonical behavior per operation. "Cascade" rows expand through the
graph: `activate` expands dependencies upward, `deactivate` expands
dependents downward.

| operation | Terminate | Pause | OnDemand | disabled | detached |
|---|---|---|---|---|---|
| `start` | spawn in dep order; idempotent | spawn cold, or **resume** an instance parked by an earlier teardown | skipped | skipped | first start spawns it; re-entry skips |
| `teardown` | stop + ack | stop + ack, parks | stop if running | nothing to do | skipped |
| `deactivate` | seed: disabled + stopped; transitive dependents: `collateral` + stopped, dependents first | disabled (or `collateral`) + stopped, parks | disabled + stopped, the whole pool when a member is the target; `collateral` as a dependent | re-disabled (idempotent) | skipped, even when targeted |
| `activate` | enabled + started after its transitive deps; a `collateral` dependent with no disabled dep left is released and restarted in the same wave | enabled + resumed in place | enabled/released only, policy regrows | clears the latch | skipped |
| `resume_node` | no-op (wrong mode) | reset + resumed in place | no-op | skipped | no-op |
| `restart` | cycle node + transitive dependents, re-gated on the way up | resumed, never respawned | left down | skipped | skipped |
| `respawn_terminate` | reset + respawn in dep order | untouched | left down | skipped | skipped |
| `resume_pausable` | untouched | reset + resumed in place | untouched | skipped | left parked |

Worth knowing: the pair is symmetric over a subtree. `deactivate(NET)`
stops NET's dependents under the `collateral` hold, and `activate(NET)`
brings the chain back: it clears the latch and releases every held
dependent with no disabled node left in its dependencies. Released
`Terminate` and `Pause` nodes restart in the same wave; released
`OnDemand` pool members are left to the elastic policy. Overlapping
deactivations compose: a node under two deactivated ancestors comes back
on the second `activate`, and a node deactivated directly keeps its latch
through an ancestor's cycle. `start_node` overrides the hold by hand.

## Stops are a wave, not a loop

Teardown does not walk the order array one node at a time. It computes, at
each moment, every node whose stopping dependents have all acked, and signals
exactly those. The consequences are worth internalizing:

- A dependency **keeps serving** while its dependents stop, because it is not
  even signalled until they are gone. A dependent may flush one last buffer
  over a link or drive a final ioctl through a runner it depends on, inside
  its own shutdown.
- Nodes with no ordering relation to each other stop **concurrently**. Write
  stop paths against that contract: a node may be told to stop while an
  unrelated service is still running, so it frees what it owns as soon as it
  acks and must not assume unordered services still work.

A missed ack is an error, never a hang: every stop path awaits the ack with
a 2 s timeout and returns `ShutdownTimeout` naming the node. The default is
per-node overridable with `ack_timeout:`, for cleanup that legitimately
takes longer; each node's window runs from the moment it is signalled.

A stop that times out still releases the node's divisible shares: a wedged
holder cannot strand its claim. A `Pause` park is the exception, since the
task is coming back.
`teardown` aborts at the first timeout so a still-live dependent never has
its dependencies stopped under it; `teardown_continue` is the best-effort
variant for the "reset next anyway" path.

## Starts are a wave too

`start`, `activate` and `restart`'s up half spawn every node whose
in-pass dependencies are up and whose gates test satisfied, on each round,
parking between rounds on a gate-event signal (a slot was filled, an executor
slot was set, readiness was asserted). Independent slow bring-ups overlap
instead of queueing, and spawn ordering stays strict: a dependent never
spawns before its in-pass deps. In a `start()` wave a node's `slot_timeout`
covers all its gates together, from the moment its dependencies resolve; the
single-node verbs (`start_node`, and through them the cascades) budget per
gate instead.

## Restarting `task:` nodes

A restart (`restart`, `respawn_terminate`, `activate`, `start_node`, pool
growth) runs a `task:` node's worker again from scratch: a `Terminate` node
comes back with fresh body state, its extras evaluated and its resources
taken again. Nothing from the previous run carries over: its exit, ack and
status are cleared before the new run starts.

Restarts work the same whichever executor or core the node runs on and
wherever the supervisor runs, including a supervisor on an interrupt
executor restarting a thread-mode node. Hand-written `spawn:` tasks restart
just as reliably as long as they acknowledge their stop as their last act;
see [Restarts across tiers](/concepts/placement#restarts-across-tiers).

For declarations, a `state:` type must be `Send`.

## Lifecycle events

Features `lifecycle-events` and `lifecycle-sink` report every transition a
supervisor drives: the waves of `start`, `teardown`, `activate`,
`deactivate`, `restart` and `respawn_terminate`, the single-node verbs, pool
scaling and bound-dep cascades. Each transition is one
`LifecycleEvent { node, kind }`:

| `LifecycleKind` | when |
|---|---|
| `Started` | a fresh instance was started (an app-spawned node: marked running) |
| `Resumed` | a parked `Pause` instance was resumed in place |
| `StopRequested` | the node was signalled to stop |
| `Stopped` | it acked without its body having returned: a parked `Pause` instance, a hand-written `ack_dropped()` |
| `Exited` | it acked by its body returning (`mark_exited()`, which every `task:` shell calls) |
| `Faulted(FaultKind)` | a lifecycle step failed: gate timeout, spawn failure, missed ack, refused restart |

`LifecycleKind` is `#[non_exhaustive]`, so match it with a `_` arm;
`kind.as_str()` gives a stable lower-case name (`"stop_requested"`) for logs
and JSON.

A worker that acknowledges its stop itself and then returns (the
`run_cancellable_acked` idiom) can be reported as either `Stopped` or
`Exited`. A worker that returns on its own, without being asked to stop,
produces no event; `has_exited()` is how to read it.

**The queue** (`lifecycle-events`): one consumer awaits the events.

```rust
loop {
    let ev = embassy_supervisor::wait_lifecycle().await;
    info!("{} {}", ev.node.name(), ev.kind.as_str());
}
```

The queue holds 8 events and never makes the supervisor wait for you: while
it is full, newer events are dropped and counted
(`lifecycle_events_dropped()`), so a consumer that starts after a large
bring-up sees its first events, not its last. `try_wait_lifecycle()` checks
without awaiting.

**The sink** (`lifecycle-sink`): the supervisor calls an app function
instead.

```rust
fn record(ev: embassy_supervisor::LifecycleEvent) {
    let now = embassy_time::Instant::now(); // the transition's own time
    info!("{} {}", ev.node.name(), ev.kind.as_str());
    // push (ev.node, ev.kind, now) into a ring
}

embassy_supervisor::set_lifecycle_sink(record); // before the first wave
```

Your function is called at the moment of each transition, in order, so
nothing is dropped and a timestamp taken in it is exact. It runs inside the
supervisor, so keep it short and non-blocking: a log line or a write into a
ring buffer, never an await or a long lock. Install it before starting the
graph; earlier events are not delivered.

Choosing between them: the queue suits a consumer that wants to await or do
real work per event, at the price of drops under bursts and timestamps taken
whenever it gets to an event. The sink suits recording and logging: lossless,
exact, and cheaper in RAM ([measured
costs](/concepts/memory#static-costs-measured)), but your code runs inside
the supervisor. Both features can be on;
then every event reaches both. To learn which node a reset interrupted, use
the [bring-up breadcrumb](/concepts/health#bring-up-breadcrumb).

## Settings-gated bring-up

Feature `enabled-if` decides `disabled` at runtime:

```rust
fn lora_on() -> bool { SETTINGS.lock(|s| s.lora) }

supervisor_graph! {
    node LORA = Terminate, task: lora_task, enabled_if: crate::lora_on;
}
```

`Supervisor::start` calls the predicate (any expression coercing to
`fn() -> bool`) once per call, before the wave, for each node that is down.
`false` latches `disabled` exactly as the `disabled` clause would:
dashboards see it, and `activate` and `start_node` still override it.
`true` never clears a latch, so a manual `deactivate` survives a later
`start()`. Running and detached nodes are not evaluated, and neither
`respawn_terminate` nor the control verbs re-evaluate the predicate: when a
setting changes at runtime, waking the node is the app's call (`activate`).

## Restart intensity

Feature `restart-intensity` (implies `restart`) bounds how often one node
may be restarted, after OTP's restart intensity:

```rust
supervisor_graph! {
    node NET = Terminate, task: net_task, restart_budget: 3 in 60000;
}
```

At most `MAX` explicit `Supervisor::restart` calls targeting the node
(directly or through `ControlOp::Restart`) per `WINDOW_MS` window. The next
one inside the window returns `FaultKind::RestartStorm` before anything is
stopped: the node keeps running as it was. Only the verb's target counts;
dependents cycled with it, and `activate`, do not. The window opens at the
first counted restart, and a restart after it closes opens a fresh window.
Windows cap at `u32::MAX / 2` ticks (about 35 min at a 1 MHz tick); `MAX`
and `WINDOW_MS` must both be at least 1. `apply_control` propagates the
fault rather than absorbing it, so the `run` driver returns it (see
[Runtime control](/concepts/control#restart-one-subtree-cycles)).

## Defaults, in one place

Shutdown-ack timeout **2 s** (per node overridable with `ack_timeout:`; the
window starts when the node is signalled). Pre-spawn gate wait **100 ms**
per node (override with `slot_timeout:`). Control mailbox depth **4**. Trace
tracks up to **4 executors**; graphs register onto a linked chain, any
number. Up to **256 nodes** per graph, indices are `u8`.

## Next

- [Runtime control](/concepts/control/) for driving
  these transitions from anywhere.
- [Dependencies and gating](/concepts/dependencies/)
  for what holds a spawn back.
