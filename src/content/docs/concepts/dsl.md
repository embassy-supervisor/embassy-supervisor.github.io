---
title: Declaring the graph
description: "The supervisor_graph! language: nodes, pools and executor slots, every clause, and what the compiler checks."
---

<p class="eyebrow">Concepts</p>

# Declaring the graph

The declaration language has exactly three item kinds: `node`, `pool` and
`executor`. Each item is one logical line of comma-separated clauses, ending
in `;`. A useful graph is often just names, modes, deps and workers:

```rust
supervisor_graph! {
    node NET  = Terminate, task: net_task;
    node HTTP = Terminate, deps: [NET], task: http_worker;
}
```

Everything else is an optional clause on those lines. The full shape:

```text
supervisor_graph! {
    name: IDENT;                        // rename the emitted GRAPH static
    executor NAME;                      // a runtime-filled spawner slot
    default executor NAME;              // ..inherited by every task:/spawn:-fn item

    node NAME = Mode,                   // Terminate | Pause | OnDemand
        deps: [A, POOL, NET ready]      // start order; `ready` also waits
        , task: worker                  //   or `spawn: task_fn`
        , resources: [R: Type, ..]      //   owned values handed at spawn
        , provides: [R, ..]             //   slots this task fills
        , exit: Type                    //   capture the return value
        , state: Type = expr            //   per-activation boxed state
        , cancel                        //   shell owns shutdown, no node
        , reads: [crate::SIG, ..]       //   declared dataflow
        , writes: [crate::SIG beat]     //   entry markers: observed, beat, veto
        , discover                      //   bind tables derived from code
        , dataflow: [crate::setter]     //   adopt an accessor's tables
        , beat_timeout: MS, ready_on_write
        , enabled_if: crate::lora_on    //   fn() -> bool; false latches disabled
        , restart_budget: MAX in MS     //   restart() refuses MAX+1 in the window
        , executor: NAME
        , slot_timeout: MS, ack_timeout: MS, disabled;

    pool NAME = [Mode, ..],             // one mode per member, floor first
        deps: [..], task: worker,
        resources: [..],
        policy: DeferredShrink::new(..),
        min: N, max: M, slot_timeout: MS, ack_timeout: MS;
}
```

Regular reading rules:

- Position has meaning exactly twice: the optional `name:` header is the
  first item, and the mode sits right after `=` (a pool takes a bracketed
  mode list there, one mode per member, floor first).
- Everything else is keyword-dispatched and may appear in **any order**:
  top-level `node`, `pool`, `executor` and `observe` items mix freely, and
  node and pool clauses reorder freely (`deps:` is a clause like any other,
  and optional).
- Every clause is inline on its item. There are no block forms and no
  top-level `resources { }` section.
- Resource slot names are unique across the whole graph; only `shared`
  entries may repeat a name.
- `detached` is not a mode or a clause: a task makes itself detached at
  runtime with `node.set_detached(true)`.

## `task:` vs `spawn:`

Two ways to name the worker. **Prefer `task:`**: it names a plain `async fn`,
possibly generic, and the macro stamps the `#[embassy_executor::task]` shell
for you.

```rust
async fn sensor<D: Driver>(node: &'static TaskNode, dev: D) { /* ... */ }

supervisor_graph! {
    node BME = Terminate, task: sensor::<Bme280>(bme_dev());
    node SHT = Terminate, task: sensor(sht_dev());   // turbofish optional
}
```

`task:` also admits generic workers, which embassy task fns normally reject:
one worker, one node per concrete instantiation, each monomorphized into its
own shell (restarts: see
[Restarting `task:` nodes](/concepts/lifecycle/#restarting-task-nodes)).
Arguments in the partial call are evaluated **inside the shell at the start
of each activation**, on the node's own executor: good for building
resources where they run, wrong for values that must be snapshotted at spawn
time. An argument that might not exist yet at that point does not belong
here; that is what `resources:` is for.

`spawn:` names a hand-written `#[embassy_executor::task]` fn. It is the right
tool in four cases:

1. The fn already carries the attribute and you cannot strip it (another
   crate, other callers).
2. The same task is also spawned outside the graph, so you want the one
   existing task pool, not a second one.
3. You need a verbatim closure for custom spawn-time logic. Call
   `NODE.adopt(&token)` inside it or the node stays invisible to tracing;
   nothing will remind you.
4. An argument must be evaluated at spawn time on the supervisor's executor,
   for example a counter snapshot an interrupt-tier task would otherwise read
   late.

Omitting both makes the node **parked**: the application spawns it by hand
(typically a `Pause` task holding a peripheral) and the supervisor tracks it
without ever spawning it.

## `resources:`, `provides:`, `exit:`, `state:`

Each is a page or a section of one:

- [`resources:`](/concepts/resources/) hands owned
  values to workers through slots, with `consume`, `shared`, `divisible`
  and `local` kinds.
- [`provides:`](/concepts/resources/#provides-slots-that-die-with-their-producer)
  names the slots a node fills at runtime; they are cleared when it stops.
- [`exit:`](/concepts/resources/#exit-typed-exit-values)
  captures a worker's return value in a slot you can await.
- [`state:`](/concepts/memory/) boxes per-activation
  heap state that is freed when the task exits.

## Dependencies and markers

`deps:` names nodes or pools. A pool name resolves to its floor member, so
`deps: [WORKERS]` means "start once the pool's always-on member is up".
Markers refine what a dependency means:

- `ready` (feature `readiness`): the spawn additionally waits for the dep's
  task to call `set_ready()`.
- `ready bound` (feature `bound-deps`): additionally, if that provider later
  withdraws readiness, the dependent is stopped, and it comes back when the
  provider does.

The whole story, including budgets, is in
[Dependencies and gating](/concepts/dependencies/).

## `executor` slots

```rust
supervisor_graph! {
    executor HIGH;   // a spawner slot, filled at runtime

    node SAMPLER = Terminate, executor: HIGH, task: sampler_worker;
}
```

`executor NAME;` declares a spawner slot. Fill it at runtime with a `SendSpawner`
from an `InterruptExecutor` or another core's executor. Nodes marked with
`executor: NAME` spawn through it. Bring-up waits for the slot, so a late-booting
core becomes a rendezvous instead of a race.

`default executor NAME;` sets that slot as the default executor for every node
and pool that could have used `executor: NAME` themselves. `task:` workers and
`spawn:` functions inherit it; parked nodes and verbatim `spawn:` closures stay
on the supervisor's executor. An explicit `executor:` always overrides the
default. Only one default executor is allowed per graph, at the compose site, and
it cannot be `#[cfg]`-gated. Details in
[Executors and cores](/concepts/placement/).

## Pools

```rust
pool WORKERS = [Terminate, OnDemand, OnDemand], deps: [NET],
    task: http_worker,
    policy: embassy_supervisor::DeferredShrink::new(Duration::from_secs(4)),
    min: 1, max: 3;
```

The mode list declares the members, floor member first; it is the only
positional part. The clauses after it are order-free, like a node's: `deps:`,
optional `executor:`, the worker, `resources:`, `policy:`, `min:`, `max:`,
optional `slot_timeout:`, `ack_timeout:` and `cancel`. `min` and `max` accept
const expressions, validated so that min <= max <= member count. The emitted
constants `WORKERS_MIN` / `WORKERS_MAX` / `WORKERS_MEMBERS` exist for
const-context sizing, for example deriving a socket budget from the worker
budget. See [Elastic pools](/concepts/pools/).

## `enabled_if:` and `restart_budget:`

Two node-only clauses attach runtime policy to a node:

```rust
fn lora_on() -> bool { SETTINGS.lock(|s| s.lora) }

supervisor_graph! {
    node LORA = Terminate, task: lora_task, enabled_if: crate::lora_on;
    node NET  = Terminate, task: net_task, restart_budget: 3 in 60000;
}
```

`enabled_if:` (feature `enabled-if`) is `disabled` decided at runtime. It
takes any expression coercing to `fn() -> bool`; `start()` calls it once per
call for each node that is down, and `false` latches `disabled` exactly as
the marker would. `true` never clears a latch. See
[Settings-gated bring-up](/concepts/lifecycle/#settings-gated-bring-up).

`restart_budget: MAX in WINDOW_MS` (feature `restart-intensity`, which
implies `restart`) allows at most `MAX` explicit restarts of the node per
window; the next one inside the window fails with `FaultKind::RestartStorm`
before anything is stopped. See
[Restart intensity](/concepts/lifecycle/#restart-intensity).

## Feature gates

Constructs behind Cargo features (`ready`, `bound`, `reads:`/`writes:`,
`observed`, `veto`, `discover`, `dataflow:` entries, `local`, `divisible`,
`state:`, `beat_timeout:`, `beat_window:`, `enabled_if:`,
`restart_budget:`, `ready_on_write`) always **parse**. Whether your build
permits them is policy applied afterwards. Using one without its feature is
a compile error that names the feature. The exception is a construct gated
by a `#[cfg]` on its item, entry or clause. The macro then leaves the
construct out and emits the rejection as a `compile_error!` under the same
cfgs. A build where the cfg is false then compiles without the feature.

## `#[cfg(...)]`

Allowed on any node or pool, on individual deps, on individual resource
entries (gate the worker's matching parameter with the same attribute), and
on a node's value-level clauses: the timeouts, `beat_window:`,
`ready_on_write`, `disabled`, `discover`, `enabled_if:` and
`restart_budget:`. A node compiled out keeps its slot as `None` and is
skipped everywhere.

## What the compiler checks

Anything structurally wrong is an error with a span on the offending token:

- unknown dependency; a dependency repeated in one `deps:` list under the
  same `#[cfg]`s; duplicate node or pool name
- two items with one runtime name. Node names are lower-cased, and a pool
  member is named `<pool><index>`. So `Net` beside `NET`, or node `W0`
  beside pool `W`, would make a lookup by name find only the first
- unknown `executor:` name; `executor:` combined with a closure spawn
- `task:` and `spawn:` together; a closure under `task:`; `resources:` or
  `state:` without `task:`, on a node or a pool
- empty or duplicate resource names; contradictory kind markers; a `shared`
  slot redeclared with a different shape
- `local` without the `local-resources` feature; a `local` slot declared on
  two executors; a `local` slot provided by a node the graph does not spawn
  (a parked node or a `spawn:` closure). The macro cannot check that
  node's executor
- `divisible` mixed with another kind or given a type, or used without the
  `budget` feature
- `veto` on a `reads:` entry, without the `veto` feature, on a gate with too
  few slots, with more than 32 writers, or with one gate spelled two ways
- `serialized` without `shared`, or with holders spread over several
  executors
- `slot_timeout: 0` or `ack_timeout: 0`; `cancel` without `task:`; `cancel` with `Pause`
- a repeated clause, a second `exit:` or `provides:`; `enabled_if:` or
  `restart_budget:` without its feature; a `restart_budget:` with no `in`, a
  `MAX` outside 1..=65535, or a `WINDOW_MS` of 0
- literal pool bounds outside `min:` <= `max:` <= member count. Bounds
  given as const expressions fail a const assertion instead
- an empty pool; a `#[cfg]` on one pool member (gate the whole pool); a
  `pool` without the `pool` feature
- a fragment body that sets `name:`, an `observe` default or a
  `default executor`. All three are graph-wide, and the compose site pastes
  every fragment into one graph
- `ready_on_write` without the `liveness-monitor` feature, or without
  `beat_timeout:`. It also needs an `observed beat` write that is gated like
  it or not gated at all. The monitor sweep asserts the readiness by polling
  that write
- more than 256 slots (all graph indices are `u8`)
- an empty `resources: []`; a suffixed integer literal (`500u32`) in a
  clause; an attribute other than `#[cfg]` on a list entry; two list entries
  with no separator between them
- a `beat_timeout:` at or past `u32::MAX / 2` ticks
- a dependency cycle, caught by the const topological sort

The generated surface at the call site is: one `pub static` per node, the
pool array and its consts, one slot static per resource entry, one spawner
slot static per `executor`, and `GRAPH`. Nothing else.

## Next

[Writing supervised tasks](/concepts/tasks/) covers
the other half of the contract: what your workers do with their node.
