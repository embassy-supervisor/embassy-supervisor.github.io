---
title: Feature flags
description: Every Cargo feature of embassy-supervisor, what it adds, and the ones that opt into unsafe.
---

<p class="eyebrow">Reference</p>

# Feature flags

Only `macros` is on by default. Everything the supervisor can *do* is
opt-in, including `control` and `pool`: both add code to the driver loop
that runs every iteration whether or not a graph uses it. `restart` and
`bound-deps` enable `control` on their own, so you rarely name it directly.

Using a gated construct without its feature is a compile error naming the
feature, never a silent behavior change.

| feature | default | adds |
|---|:---:|---|
| `macros` | ✓ | the `supervisor_graph!` declaration macro |
| `control` | | runtime control plane: `ControlOp`, `request_control`, `try_request_control`, `apply_control`; `Deactivate` holds dependents under the `collateral` flag until `Activate` releases them |
| `pool` | | elastic pools: `ElasticPool`, `run_pools`, `GRAPH.pools` |
| `local-resources` | | the `local` resource kind. ⚠ opts into the macro emitting a documented `unsafe impl Sync` (single-core contract) |
| `budget` | | the `divisible` resource kind: a graph-sized `Budget<K>`, a `Claimant` per holder, allocator-side `rebalance` under a `BudgetPolicy` (`FairShare`, `ShrinkFastGrowSlow`), and a stopped holder's share released by the supervisor (missed ack included; a `Pause` park keeps it) |
| `readiness` | | `set_ready` / `wait_ready` / `clear_ready` and the `ready` dep marker |
| `liveness` | | per-node heartbeat: `beat()`, `ticks_since_beat()`, `is_stale()` |
| `liveness-monitor` | | the sweep: `beat_timeout:` / `beat_window:`, `wait_health()`, `HealthEvent`. Report-only. Implies `liveness` |
| `epochs` | | per-node activation generation: `epoch()`, `wait_epoch_change(seen)` |
| `coupling` | | declared dataflow: `reads:` / `writes:` and the signal-indexed queries; `Stamped<T>` for read-side write-freshness checks |
| `coupling-observe` | `coupling` | the `observed` marker and its accessor; with `liveness-monitor`, `beat` drives the heartbeat and `ready_on_write` by polling |
| `dataflow` | `coupling` | the node as the access path: `#[dataflow]`, `discover`, `dataflow: [..]`, verbs of your own; `beat_put` / `beat_writer` (need `liveness` too) |
| `graph-ref` | | the graph as one addressable `'static` (`GraphRef`); the handle `data-deps` and `trace` need |
| `veto` | `dataflow` | the `veto` write marker: one contributor slot of a `VetoGate<N>` per writer, numbered and capacity-checked by the macro; `node.veto(&SIG)` moves only that writer's bit, and a stopped writer's bit stays asserted |
| `data-deps` | `graph-ref` + `dataflow` | gated reads (`Backed`, the counted `Open` guard, `retire`, `producer_of`) and leases (`Leased`, `lease`, `drain`) |
| `node-status` | | `report_status()` / `status()`: a one-line self-description per node |
| `restart` | | `Supervisor::restart`: cycle a node and its transitive dependents, re-gated. Implies `control` |
| `bound-deps` | | the `bound` dep marker: `clear_ready()` stops a bound dependent. ⚠ the one feature that changes a documented contract. Implies `readiness` + `control` |
| `heap-state` | | `state: Type = expr` / `state: zeroed Type`. ⚠ emits a ~6-line fallible-boxing `unsafe` helper into your crate; needs a `#[global_allocator]` |
| `defmt` | | route the supervisor's logs through defmt |
| `log` | | route them through the `log` facade (hosted/std). With neither, log calls are no-ops |
| `trace` | | trace recorders: per-node CPU time, polls, max-poll watermark; executor stats; stall detection |
| `trace-hooks` | | also define the `_embassy_trace_*` hook symbols at the graph site. Implies `trace` |
| `metadata-names` | | node names in task metadata for external tooling (rtos-trace/SystemView); no hook symbols |
| `trace-names` | | `trace` + `metadata-names` |
| `trace-nested` | | preemption-exact accounting. Implies `trace` |
| `trace-self` | | the supervisor's own driver task as a hidden auto-adopted node. Implies `trace` |

A reasonable "most of the model" set for a connected device:

```toml
embassy-supervisor = { version = "0.8", features = [
    "readiness", "liveness-monitor", "control", "pool",
    "coupling-observe", "dataflow", "defmt",
] }
```

## Related crates

- [`embassy-supervisor-observe`](https://crates.io/crates/embassy-supervisor-observe):
  the leaf facade a signal library implements (`Observable`, `Counted`)
  without depending on the supervisor.
- [`embassy-supervisor-tools`](https://crates.io/crates/embassy-supervisor-tools):
  the `supervisor-mermaid` and `supervisor-lint` host tools.
- [`embassy-supervisor-macros`](https://crates.io/crates/embassy-supervisor-macros):
  the proc macros, pinned by exact version automatically.
