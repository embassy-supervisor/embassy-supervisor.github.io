---
title: The model
description: TaskNode, Graph and Supervisor, the three statics every supervised firmware is built from.
---

<p class="eyebrow">Concepts</p>

# The model

Everything the supervisor does happens through three kinds of statics. The
macro emits the first two; you construct the third.

<div class="graph-slot" style="--gw: 1123.64; --gh: 218">

```mermaid
flowchart LR
    accDescr: NodeCfg, TaskHandle and TaskNode form the Graph the Supervisor drives
    CFG["NodeCfg<br/>name · mode · gates<br/>flash-resident constants"]:::signal
    HANDLE["TaskHandle<br/>running · ready · beats<br/>atomics in .bss"]:::task
    NODE["TaskNode<br/>two pointers, in flash"]:::task
    GRAPH["GRAPH<br/>nodes + topo order + pools"]:::provider
    SUP["Supervisor<br/>start · stop · control"]:::pool

    CFG -. paired by .-> NODE
    HANDLE -. paired by .-> NODE
    NODE --> GRAPH
    GRAPH --> SUP
```

</div>

## TaskNode

One per managed task, three statics so RAM stays small. The **`NodeCfg`** is
the immutable half: name, mode, spawn glue, gate list, budgets, coupling
tables and the `disabled` boot marker, a flash constant. Its fields are
private. `const` builders set them, and you read them through the node.
Timeouts are stored as `u32` milliseconds, and the heartbeat budget as `u32`
ticks. So the struct has no 8-byte field and aligns to 4. The
**`TaskHandle`** is the live half: running, shutdown requested, ready,
heartbeat, exit record, packed into atomic words and all zero at rest, so it
lives in `.bss`. A stop's ack is announced on one shared stop event. So the
handle needs no ack signal of its own. The **`TaskNode`** pairs the two and
is itself two pointers in flash. The macro emits all three per node. A
hand-built node is
`static CFG: NodeCfg = NodeCfg::new("n", Mode::Terminate, None)`,
`static H: TaskHandle = TaskHandle::new()` and
`static N: TaskNode = TaskNode::new(&CFG, &H)`, with
`NodeCfg::with_disabled_at_boot(true)` for a node that starts disabled. The
split keeps per-node RAM to the atomics even with tracing on.

Application code reads the node through methods: `name()`, `mode()`,
`slot_timeout()`, `is_running()`, `is_ready()`, `is_stale(..)` and friends.
Task code also *writes* it: `beat()`, `set_ready()`, the shutdown
combinators. The full task-side protocol is on
[Writing supervised tasks](/concepts/tasks/).

## Graph

`supervisor_graph!` emits `pub static GRAPH`. It bundles:

- `nodes`: a fixed array of node references. A node compiled out with
  `#[cfg]` keeps its slot as `None` and is skipped at runtime.
- `topo`: the dependency indices per node and the start order, computed at
  compile time. A graph with no `deps:` anywhere uses a zero-sized `Flat`
  topology instead. The topology is typed by the graph's structural
  **shape**: whether it declares ready deps, executor slots, resources,
  `Pause` or `OnDemand` nodes, heartbeats, `observed` signals, `bound`
  edges or pools. Every lifecycle branch serving a structure the graph
  lacks is compiled out rather than branched over at runtime, so a lean
  graph ships lean code.
- `pools` (feature `pool`) and `graph_ref` (feature `graph-ref`).

The fields are public for reading. `Graph` is `#[non_exhaustive]`, so a
hand-built graph is `Graph::new(&NODES, topo)`, followed by
`.with_pools(..)` and `.with_graph_ref(..)` where those features are on.

You can walk it directly: `GRAPH.iter_nodes()`, `GRAPH.deps_of(i)`,
`GRAPH.dependents_of(i, ..)`, `GRAPH.order()`. Status endpoints and monitors
are usually ten lines over those.

## Supervisor

`Supervisor::new(&GRAPH)` is `const` and infallible (the graph was validated
at compile time). It works over a borrowed graph with no hidden state of its
own, in three tiers of verbs:

| tier | verbs | notes |
|---|---|---|
| whole graph | `start`, `run`, `run_with`, `teardown`, `teardown_continue`, `respawn_terminate`, `resume_pausable` | `run` = best-effort `start` + drive pools/control; returns only on a missed shutdown ack or a refused restart. `run_with` takes a `RunPolicy` that picks which faults it returns |
| single node | `start_node`, `stop_node`, `resume_node` | no cascade |
| subsystem | `activate`, `deactivate`, `apply_control`, `restart` | dependency cascades |

Every fallible verb returns the same error shape.

## One error type, with provenance

```rust
pub struct NodeFault {
    pub node: &'static TaskNode,
    pub kind: FaultKind,
}
```

`FaultKind` names what went wrong: `ExecutorSlotEmpty`, `ResourceMissing`,
`ReadyDepTimeout { dep }`, `Spawn(SpawnError)`, `ShutdownTimeout`, and with
`restart-intensity` `RestartStorm`. Its
`Display` names the node and the cause, so `panic!("supervisor: {fault}")`
produces an actionable line. All guarantees are cross-thread with proper
atomics: a host test's main thread reads them as safely as another task.

`Aborted` is the cancellation result of the cancellable combinators,
`Resumed` the pause-cycle result of `run_pausable`, and `ControlQueueFull`
is what the synchronous control call returns when the mailbox is full.
Those four types cover the whole crate's error surface; each implements
`Display` and `core::error::Error`.

## What the supervisor never does

It never runs task code: tasks are spawned onto whatever executor the
declaration routes them to. It never allocates. It never owns a HAL. And it
never decides policy for you: what to do about a stale task, a failed
bring-up or a missed shutdown ack is returned or reported as data, and the
application escalates.

## Next

[Declaring the graph](/concepts/dsl/) covers every
clause of the declaration language these statics come from.
