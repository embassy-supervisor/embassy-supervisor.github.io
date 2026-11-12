---
title: Diagram and lint tools
description: supervisor-mermaid draws your graph and supervisor-lint checks its dataflow, straight from the source.
---

<p class="eyebrow">Guides</p>

# Diagram and lint tools

The `embassy-supervisor-tools` crate ships two host tools that read your graph
declaration straight out of the Rust source: the same parser, the same
scanner the `#[dataflow]` attribute uses, so the output is the build's view
by construction.

```console
cargo install embassy-supervisor-tools
```

As a library with `default-features = false`, the crate works from source
text alone. It parses, models, renders and lints. Its default `cli` feature
adds the two binaries, input gathering, output writing and the JSON model.
It also adds their `flate2` and `serde_json` dependencies.

## supervisor-mermaid

Run it from the crate whose graph you want drawn:

```console
$ cd firmware && supervisor-mermaid
```

No arguments means "this crate". Point it at things explicitly when the
graph spans files or crates (a compose site plus its fragments, or the files
holding the `#[dataflow]` fns a `discover` node binds). `--deps` adds the
workspace packages the current one reaches through `path` dependencies. It
reads them with `cargo metadata --no-deps`. From a virtual manifest's
directory, it adds every member:

```console
$ supervisor-mermaid src/main.rs src/tasks.rs
$ supervisor-mermaid --deps src/main.rs
```

Inputs follow these rules:

- A directory is walked recursively for `*.rs`. The walk skips hidden
  directories, symlinks, and a `target` directory that carries Cargo's
  `CACHEDIR.TAG`.
- An unreadable directory is reported.
- A file that does not parse is reported, not skipped. `--check` and
  `supervisor-lint` fail on it.
- An option takes its value as the next argument, or inline as
  `--opt=value`. A next argument that looks like an option is an error, so
  write a value that starts with `-` inline. A lone `-` (stdin) is the
  exception.

### The three diagrams

**Bring-up** (the default) answers *what starts after what*. A `deps:` edge
joins two boxes and its weight says how much is enforced: plain is spawn
order, `ready` awaits `set_ready()`, thick `ready bound` propagates
readiness. Resource slots are drawn too: an unfilled one fails the spawn, so
it gates bring-up as much as a dep does.

**Runtime** (`--runtime`) answers *what the running system looks like*:
every signal and resource, no bring-up edges. Coupling always routes through
a signal box, so it can never be mistaken for a dep; the two relations are
not the same one. `--runtime-deps` restores every `deps:` edge as dotted
spawn context beside the solid lifetime coupling.

**Lifecycles** (`--states`) answers *what happens to one node over its
life*: a state diagram with only the transitions the declaration implies.
With `--signals`, one composite per node, carrying the concrete gates: the
slots its spawn takes, the readiness it waits on, the slots a stop clears.

A `#[cfg]`-gated item draws like any other. Its predicate appears in its
label or on its edge. This covers a node, a dep, a signal entry, a
`discover` clause, a `dataflow:` entry and a `#[dataflow]` call site.
`--hide-cfg` omits those markers. A `#[cfg]` on the `supervisor_graph!`
invocation itself puts its predicate in the diagram title. Fragments of one
name may be declared under different `#[cfg]` sets. The tool splices them
all, and gates each item by its fragment's predicates.

### Getting the output somewhere useful

- `--live-url`: a mermaid.live share link, nothing to install.
- `--html FILE`: one self-rendering page. It loads pinned releases of
  mermaid and `@mermaid-js/layout-elk` from a CDN. It loads ELK only for a
  diagram that asks for it.
- `--render FILE`: svg/png/pdf through `mmdc`.
- `--update FILE.md`: rewrites the managed block between
  `<!-- supervisor-mermaid:start/end -->` markers in a markdown file, and
  touches nothing else.
  - An unchanged file is left alone.
  - A write goes through a temporary file and a rename. It keeps the
    file's CRLF line endings.
  - Markers inside fenced code blocks are ignored.
  - A second start marker after the managed block is an error.

  With `--watch`, the file follows the source as you edit. The inputs are
  gathered again every cycle, so a new file or `mod` is picked up. This is
  how a README keeps a current picture of its graph.
- `--json`: the graph model, for anything that is not a diagram. It holds
  nodes, deps, resources, signals and scanned accesses. Signals carry their
  `veto` marker, and resources their `divisible` and `serialized` kinds.
  Each node and pool carries its `fragment`, and each graph its `cfg`
  array. The top level holds `"schema": 1`.
- `--links 'vscode://file/{file}:{line}:1'`: each node box links to its
  declaration. `{file}` is its absolute path and `{line}` its source line.
  Pair it with `--html` and open the page in an external browser.

`enabled_if:` and `restart_budget:` show up as node facts in the diagrams
(`if radio_on`, `restarts 3/60000ms`) and in the JSON model (`enabled_if` as
the predicate's text, `restart_budget` as `{max, window_ms}`).

Layout help on bigger graphs: `--layout elk`, `--max-fanout 6` to collapse a
widely-read signal into one aggregate box, `--legend` for a key,
`--executors` to box nodes by the executor they spawn through,
`--fragments` to box each fragment's items.

<div class="graph-slot" style="--gw: 986.83; --gh: 76">

```mermaid
flowchart LR
    accDescr: The tool pipeline from source to diagram
    SRC["Rust sources<br/>graph + #[dataflow] fns"]:::task
    PARSE["shared parser<br/>one grammar"]:::provider
    MODEL["graph model<br/>nodes · deps · signals"]:::resource
    OUT["mermaid · html · json<br/>live-url · markdown update"]:::pool

    SRC --> PARSE --> MODEL --> OUT
```

</div>

## supervisor-lint

The same model, asked what its dataflow is missing: a signal some node reads
that nothing writes (`orphan-reads`), a signal some node writes that nothing
reads (`dead-writes`). The static shape of diagnostics a running supervisor
logs, at build time instead of on a serial console.

```console
$ cd firmware && supervisor-lint
$ supervisor-lint --only dead-writes --allow RATE_PID_TERMS src/
```

Findings exit 1. An input file that does not parse also exits 1. A failed
run (a bad option, an unreadable input, no graph) exits 2. This is a CI
gate, not a report. A one-sided
signal is often a real, accepted absence (an input this target has no
producer for, a telemetry tap nothing consumes yet), so `--allow` names the
accepted ones in the invocation, where it gets reviewed like code. An
`--allow` entry that suppresses nothing is itself reported. This happens
whenever `--only` keeps a signal category.

`public-gate` is the third finding: a `Backed`, `Leased` or `VetoGate`
static that is not private. Gates guard the access path, not the data; the
module boundary is what makes bypass deliberate (see
[Gated reads and leases](/concepts/data-deps/)).

`observed`/`beat` entries are exempt from dead-write findings by design:
their consumer is the supervisor.

## `--check`: keeping graph and code in lockstep

`supervisor-mermaid --check` verifies and stops: it fails when the graph
names a `#[dataflow]` fn no scanned file defines, or a scanned fn no node
binds. Both are how a signal comes to look one-sided when it is not. The CI
shape:

```console
supervisor-mermaid --check --deps src/ && supervisor-lint --deps src/
```

Together the two tools pin the two halves: the diagram cannot drift from
the declaration, and the dataflow cannot grow silent holes.

## Next

[The reference firmware](/guides/demo-firmware/)
runs these tools on a real graph and shows the artifacts.
