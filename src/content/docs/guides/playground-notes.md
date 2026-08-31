---
title: 'How the playground works'
description: 'What is real in the playground, what is simulated, and which DSL clauses it executes.'
---

The [playground](/playground) is not a mock-up. Your graph is parsed by the same
`embassy-supervisor-syntax` crate the proc-macro uses and run by the real
`embassy-supervisor` runtime: the crates.io release, compiled to WebAssembly
on embassy-executor's wasm platform. Bring-up order, ready gating, pools, the
control queue, the liveness monitor, gated reads and leases are the same code
that runs on an MCU.

Three things differ from firmware.

## Virtual time

The clock is embassy-time's mock driver: time advances only when the page
advances it, which is what the pause / 1× / 10× / step controls drive. Runs
are deterministic and scrubbable, and timings model an MCU's *ordering*, not
its microseconds. Nothing here measures poll latency; the
[tracing guide](/concepts/trace/) covers that on hardware.

## Simulated task bodies

A static page cannot compile your Rust, so `task:` paths run on a generic
interpreter instead, the approach SlintPad and the Typst web app take. Each
worker's behavior (periodic producer, pipeline stage, pool server, bounded
queue, budget allocator, session holder, control loop, …) comes from the
scenario's bindings, or is inferred from the node's shape when you add new
nodes. The workers drive the real task-side APIs: `beat()`, `set_ready()`,
`mark_busy()`, `provide()`, `open()`, `lease()`, `run_pausable()`,
`set_detached()`, `report_status()`.

A `queue` behavior carries an explicit overflow policy (`reject`,
`backpressure`, or `drop_oldest`) because the right answer depends on the
other side: back-pressure what can slow down, drop what is driven by a clock
you cannot pause. Stale reports are answered by the scenario's escalation map
(`report`, `clear_ready`, `restart`, `deactivate`, `activate:OTHER`): the
liveness monitor only reports, the application decides.

A "crash" is an abrupt worker exit. A real `panic!` would take the whole wasm
instance with it: the one place the browser is less forgiving than a
supervised MCU.

Two flag details for reading the panes. Busy and ready clear on a node's
*next* activation, not on stop, so the worker clears both itself on every stop
path: a bound-stopped session no longer reads busy, and stopping a node
withdraws its readiness, which is what lets a `ready bound` subtree follow a
`stop_node` down. And `DeferredShrink` shrinks only when at least **two**
members are idle, so a `min: 0` pool settles at one warm member: hysteresis,
not a leak.

## What executes

The interpreter rebuilds the graph at runtime from the supervisor's public
constructors, so most of the DSL executes for real:

| Executes for real | |
| --- | --- |
| modes | `Terminate`, `Pause`, `OnDemand` |
| ordering | `deps:`, including `ready` and `ready bound` markers |
| executors | named `executor` clauses get real (wasm) executor instances |
| pools | member modes, `min:`/`max:` (integer literals), `DeferredShrink` cooldowns |
| resources | `resources:` gating, `provides:`, consume markers |
| timeouts | `slot_timeout:`, `ack_timeout:`, `beat_timeout:`, `beat_window:` |
| liveness | beats, `observed` / `beat` markers, `ready_on_write`, stale reports |
| data-deps | gated `open()` demand-start, `Leased` leases / drain / reopen |
| control | activate, deactivate, restart cascades, `disabled` at boot, the `collateral` hold released by activate |
| single-node verbs | `start_node` / `stop_node` / `resume_node` (the graph cards and device buttons drive them) |
| whole-graph verbs | `teardown`, `resume_pausable`, `respawn_terminate`: the power coordinator's sleep/wake cycle |
| Pause | parks for real: ack, wait, resume in place, keeping what it took |
| detached | `set_detached` (the power coordinator and self-test behaviors), shown as a chip and LED state |
| resource kinds | lend (taken and restored, the holder shown), `consume` (empty after exit; a respawn fails closed until re-provided), `shared` (never taken) |
| pool deps | `deps: [POOL]` resolves to the floor member, matching the crate |
| trace | the recorders run: genuine poll and pass counts, the current task per executor |
| composition | `supervisor_fragment!` + `compose_graph!` |

Clauses parsed but not executed are badged in the editor rather than rejected:
`exit:` and `state:` (their storage is macro-generated), `discover` and
`dataflow:` adoption lists (shown and linted, not run), `#[cfg(...)]` (treated
as enabled), and non-literal pool expressions. A parked node (no `task:`) is
spawned by the app only when a scenario binds a `power_coordinator` behavior
to it; otherwise it is tracked but idle.

## The task panel

The Tasks panel is a `top` for the graph: one row per node, running or not,
with its executor, declared core, `report_status()` text, busy flag, poll
counts and durations, exec share, beat age, epoch and flags, plus a
per-executor strip naming the task currently mid-poll.

Two honesty notes. **Counts are real, durations are browser time.** The mock
clock never advances during a poll, so the crate's own `exec_ticks` would read
zero here; the playground stamps `performance.now()` around each poll instead,
and the last/max poll columns show wasm wall time in your browser, never MCU
microseconds. The [tracing guide](/concepts/trace/) covers measuring on
hardware. **The core column is declared, not measured**: wasm is
single-threaded, so it shows the placement the scenario declares
(`trace::set_core_id_fn` is the on-hardware mechanism), and every executor
polls on the one main thread. A `hog` marker means a single poll ran past
50 ms of browser time: a task starving its executor, which is a different
failure from a stale heartbeat (a task not making progress).

## Tips

- A `deps: [X ready]` edge waits at the gate for the default 100 ms
  `slot_timeout`: give consumers of slow providers an explicit
  `slot_timeout:`, or the supervisor will (correctly) fault the bring-up.
- The wedge fault is honest: the node stops acking shutdown, so the next stop
  or restart surfaces a real `ShutdownTimeout` fault.
- Every line in the logs pane is the supervisor's own `log` backend,
  timestamped in virtual seconds.
