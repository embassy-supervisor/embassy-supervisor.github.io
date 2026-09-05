---
title: 'How the playground works'
description: 'What is real in the playground, what is simulated, and how to read the task panel.'
---

The [playground](/playground) runs around 95% of the real code that
runs on an MCU. Your graph is parsed by the same
`embassy-supervisor-syntax` crate, then executed by the
same `embassy-supervisor` runtime that ships to crates.io, compiled to
WebAssembly on embassy-executor's `wasm` platform.

Bring-up order, ready
gating, executor scheduling, pools, the control queue, the liveness monitor,
gated reads, leases, and producer retirement are the exact code paths a
firmware binary uses.

## Supervisor aspects that run unchanged

The following design concepts all execute in the browser with the same
semantics as on hardware:

- Task modes: `Terminate`, `Pause`, `OnDemand`.
- Dependency ordering: `deps:`, including `ready` and `ready bound`
  markers.
- Named executors: `executor` clauses create real wasm executors; the
  default executor is inherited. Cards with inherited executors show a
  dashed chip.
- Pools: member modes, `min:`/`max:` sizes, and `DeferredShrink`
  cooldowns.
- Resources: `resources:` gating, `provides:`, and the `consume` and
  `divisible` markers.
- Timeouts: `slot_timeout:`, `ack_timeout:`, `beat_timeout:`, and
  `beat_window:`.
- Liveness: beats, `observed`/`beat` markers, `ready_on_write`, and stale
  reports.
- Data dependencies: gated `open()` demand-start with counted `Open`
  guards, producer `retire`, and `Leased` leases, drain, and reopen.
- Control plane: activate, deactivate, restart cascades, `disabled` at
  boot, and the `collateral` hold released by activate.
- Single-node verbs: `start_node`, `stop_node`, `resume_node`.
- Whole-graph verbs: `teardown`, `resume_pausable`,
  `respawn_terminate` (the power coordinator's sleep/wake cycle).
- `Pause` behavior: real park, ack, wait, and resume in place, keeping
  what it took.
- `detached` nodes set through `set_detached`.
- Resource kinds: lend (taken and restored), `consume` (empty after exit;
  a respawn fails closed until re-provided), `shared` (never taken), and
  `divisible` (a real `Budget` per name, with holders claiming through a
  `Claimant` and the allocator dividing via `FairShare` /
  `ShrinkFastGrowSlow`; the supervisor releases a stopped holder's share
  on its shutdown ack).
- Veto gates: `writes: [X veto]` runs `X` as a `VetoGate`, with one
  contributor bit per writer, `node.veto()` handling, and the reader's
  `wait_asserted` / `wait_released`; a stopped writer's bit stays up.
- Pool dependencies: `deps: [POOL]` resolves to the floor member, matching
  the crate.
- Trace recorders: genuine poll and pass counts, plus the current task
  per executor.
- Composition: `supervisor_fragment!` and `compose_graph!`.
- Fault injection: the ⚡ menu calls real fault verbs. Stall skips polls,
  crash drops the worker, and wedge swallows the stop ack. Hog is not
  offered because wasm's single thread would freeze alongside it.

## What a task is in the playground

In the playground, a task is a generic task that emulates a hardware
component's behavior. A static page cannot compile your Rust, so `task:`
paths do not run your real functions. Instead, each worker's behavior is
provided by a generic interpreter, the same approach SlintPad and the
Typst web app take. The behavior comes from the scenario's bindings, or
is inferred from the node's shape when you add new nodes.

The generic task drives the real task-side APIs, including `beat()`,
`set_ready()`, `provide()`, `open()`, `lease()`, `retire()`, `veto()`,
and `report_status()`. The runtime underneath is identical to firmware;
only the body is a stand-in.

Because the worker body is generic, time is driven generically too. The
playground uses embassy-time's mock driver: the clock advances only when
the page advances it, through the pause, 1×, 10×, and step controls. It
advances in slices no coarser than 25 ms, whatever the multiplier, so a
100 ms timer and a 500 ms timer never come due on the same poll.
Fast-forward compresses wall-clock time, not the ratios between the
rates you declared. Runs are deterministic and scrubbable, and timings
model an MCU's *ordering*, not its microseconds. Nothing here measures
poll latency; the [tracing guide](/concepts/trace/) covers that on
hardware.

A `queue` behavior illustrates the approach. It carries an explicit
overflow policy (`reject`, `backpressure`, or `drop_oldest`) and drains
one item per running consumer per tick, so the service rate is set by
whoever takes from it. The interpreter fills in these semantics so the
real supervisor code above it sees the same lifecycle it would on a
chip.

## Interpreting the task panel

The Tasks panel is a `top` for the graph. It shows one row per node,
running or not, with the executor it lives on, its declared core,
`report_status()` text, the busy flag, poll counts and durations, exec
share, beat age, epoch and flags, and a per-executor strip naming the
task currently mid-poll.

### Reading the columns

- **Executor and core.** The executor name is real. The core column is
  the placement the scenario declares, not a measured value. Wasm is
  single-threaded, so every executor polls on the one browser thread;
  `trace::set_core_id_fn` is the on-hardware mechanism. Treat the column
  as a scheduling hint the supervisor would honor on a real chip.

- **Poll counts.** These are real counts from the supervisor's trace
  recorder: how many times the node was polled and how many of those
  polls did useful work.

- **Durations.** The mock clock stands still during a poll, so the panel
  measures wasm wall time with `performance.now()` instead. Last and max
  are browser milliseconds, not MCU microseconds.

- **Beat age.** How long since the node last called `beat()`. This is
  measured in virtual seconds, because beats are driven by the mock
  clock.

- **Exec share.** The share of executor time the node has consumed
  recently.

- **Flags.** Epoch, busy, stale, and other state bits the supervisor
  exposes. Busy and ready clear on the node's next activation, not on
  stop.

- **Hog marker.** A single poll that ran past 50 ms of browser time. It
  means the task is starving its executor, which is a different failure
  from a stale heartbeat (a task that is not making progress).

The [tracing guide](/concepts/trace/) covers measuring real poll latency
and durations on hardware.

## Tips

- A `deps: [X ready]` edge waits at the gate for the default 100 ms
  `slot_timeout`: give consumers of slow providers an explicit
  `slot_timeout:`, or the supervisor will (correctly) fault the bring-up.
- Wedge swallows the shutdown ack, so the next stop or restart surfaces a
  real `ShutdownTimeout`. `clear fault` delivers the swallowed ack and
  lets a restart succeed. Stall survives a stop, so restart without clear
  leaves the node stalled, shown by the `⚡stall` chip.
- Every line in the logs pane is the supervisor's own `log` backend,
  timestamped in virtual seconds.
