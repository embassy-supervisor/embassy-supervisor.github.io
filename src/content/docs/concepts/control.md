---
title: Runtime control
description: The control mailbox, dependency-aware cascades, and the restart verb.
---

<p class="eyebrow">Concepts</p>

# Runtime control

Feature `control` turns the graph from a boot-time construct into something
you can drive at runtime, from anywhere: a request handler, a button, a
shell, a test.

## The mailbox

Code that does not hold the supervisor talks to it through a decoupled,
**lossless** mailbox:

```rust
use embassy_supervisor::{request_control, try_request_control, ControlOp};

// Async: waits for mailbox capacity if the queue is full.
request_control(&OTA, ControlOp::Activate).await;

// Sync (an ISR, a callback): reports a full queue instead of waiting.
if try_request_control(&OTA, ControlOp::Activate).is_err() {
    // mailbox full: log, drop, or escalate
}
```

`ControlOp` is `Activate`, `Deactivate`, and with the `restart` feature
`Restart` (the enum is `#[non_exhaustive]`: further verbs may arrive without
a breaking change). The higher-level verbs (start, stop, pause, resume) fold
onto `Activate`/`Deactivate` according to the node's mode. The supervisor
side applies each command with `apply_control`, which is dependency- and
pool-aware. Mailbox depth is 4.

Acceptance into the mailbox is not execution success: `request_control`
waits for queue capacity, and `try_request_control` reports only a full
queue. Startup faults from an Activate command are returned by
`apply_control` and propagate through the supervisor driver to its caller.

The verbs that live directly on `Supervisor` when you do hold it:
`start_node`, `stop_node`, `resume_node` (single node, no cascade), and the
cascading pair below.

## Cascades

`activate` and `deactivate` expand through the graph:

- **`activate(&node, &spawner)`** pulls the node's transitive **dependencies** up in
  start order (skipping already-running ones). Activating a leaf rebuilds
  its whole supply chain.
- **`deactivate(&node)`** stops the node and its transitive **dependents**
  in reverse order. The target itself is marked `disabled`; its dependents
  get the **`collateral`** hold instead, so retiring a subtree does not
  brand every node in it as manually stopped.

The pair is symmetric over a subtree: the way back from `deactivate(NET)`
is `activate(NET)`. It clears the latch, then releases every collateral
dependent with no disabled node left in its transitive dependencies.
Released `Terminate` and `Pause` nodes restart in the same wave; released
`OnDemand` pool members are left to the elastic policy; activation requests
a demand re-check even when its startup wave returns an error. Overlapping
deactivations compose: a node under two deactivated ancestors comes back
on the second `activate`, and a node deactivated directly keeps its latch
through an ancestor's cycle. `start_node` overrides the hold by hand.

Both latches survive wake respawns and pool regrows; only `activate`
clears them. `activate` on a **pool member** expands to the whole pool:
respawn the floor, re-enable the on-demand members.

```mermaid
flowchart TD
    accDescr: Cascades expand in opposite directions
    WIFI["WIFI"]:::provider
    NET["NET<br/>deps: WIFI"]:::task
    UPLOAD["UPLOAD<br/>deps: NET"]:::pool

    WIFI --> NET --> UPLOAD
    UPLOAD -. "activate(UPLOAD)<br/>pulls WIFI, NET up" .-> WIFI
    WIFI -. "deactivate(WIFI)<br/>drops UPLOAD, NET first" .-> UPLOAD
```

Both cascades return `Result<(), NodeFault>`. `deactivate` commonly returns
a `ShutdownTimeout` naming a node that missed its ack.
`activate(&node, &spawner).await` completes the best-effort startup wave and
returns the first startup fault, or `Ok(())` if none occurs. Successful
starts are not rolled back, so an error can leave a partially running
graph. Already-running nodes are skipped, and selecting no work succeeds.
Handle the result with `.await?` or explicit error handling:

```rust
sup.activate(&UPLOAD, &spawner).await?;
```

The same activation result is returned by `apply_control` for
`ControlOp::Activate`, so the driver can return the fault rather than
silently accepting a failed activation.

## `restart`: one subtree cycles

Feature `restart` cycles a node **and its transitive dependents**: stop the
subtree, reset, then bring it back up through the full gate sequence
(resources re-gated, ready deps re-awaited). A node that went stale can be
cycled in place:

```rust
// health monitor, somewhere in the app:
loop {
    let event = embassy_supervisor::wait_health().await;
    if let HealthKind::Stale { .. } = event.kind {
        let _ = sup.restart(event.node, &spawner).await;
    }
}
```

`Pause` nodes in the subtree are resumed, never respawned (their parked task
keeps its state and its task-pool slot). The disabled latch sticks: a
manually stopped node is not revived by a restart above it.

With feature `restart-intensity`, a node's
`restart_budget: MAX in WINDOW_MS` clause bounds how often it can be the
target: the restart past `MAX` inside the window returns
`FaultKind::RestartStorm` before anything is stopped, and the node keeps
running as it was. Dependents cycled with the target do not count against
their own budgets ([Restart intensity](/concepts/lifecycle#restart-intensity)).

A direct `restart` call returns startup faults as well as shutdown
timeouts. The control Restart policy is different from Activate:
`apply_control` propagates Restart's shutdown timeout, and with
`restart-intensity` its `RestartStorm` refusal, so the `run` driver returns
them; it logs and absorbs Restart's startup faults.

## The split single-node verbs

Use `start_node` / `stop_node` when you mean exactly one node and no cascade:
pausing one service for a maintenance window, probing a respawn. `stop_node`
on a `Pause` node is the single-node pause: it acks and parks, and
`resume_node` thaws it in place, keeping held resources. `start_node`
waits up to the node's `slot_timeout` for the previous instance to finish,
as the bring-up waves do (see [Restarts across
tiers](/concepts/placement#restarts-across-tiers)).

## What skips everything

A **detached** node is outside all of this: never stopped, never restarted,
never pulled into a cascade, even when targeted directly. Its `deps:` ordered
its first spawn; after that the graph only remembers where it was declared.

## Next

[Health monitoring](/concepts/health/) is the natural
producer of control decisions.
