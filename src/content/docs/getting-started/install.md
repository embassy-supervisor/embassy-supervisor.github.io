---
title: Installation
description: Add embassy-supervisor to a firmware, pick the right feature set, and match embassy versions.
---

<p class="eyebrow">Start</p>

# Installation

## Requirements

- **Rust 1.88 or newer** (edition 2024). Check with `rustc --version`.
- A firmware already using **embassy**, or a host binary using
  embassy-executor's std support. The supervisor tracks these crate versions:
  `embassy-executor` 0.10, `embassy-sync` 0.8, `embassy-time` 0.5. Because
  embassy is pre-1.0, your application must resolve to compatible minor
  versions. If you use a git or patched embassy, make sure the supervisor
  resolves to the same revision through your `[patch]` section.
- No allocator needed. No `unsafe` in the library
  (`#![forbid(unsafe_code)]`); two optional features can emit small,
  documented `unsafe` helpers into *your* crate, and both are opt-in.

## Add the crate

```console
cargo add embassy-supervisor
```

The `macros` feature is on by default. It provides the `supervisor_graph!`
declaration macro, re-exported from the crate root. Everything else the
supervisor can do is opt-in:

```toml [Cargo.toml]
[dependencies]
embassy-supervisor = "0.8"

[features]
# an example application feature that turns on supervisor capabilities
default = ["supervised"]
supervised = [
    "embassy-supervisor/readiness",
    "embassy-supervisor/liveness-monitor",
    "embassy-supervisor/control",
    "embassy-supervisor/pool",
]
```

Pick features by what your graph actually uses. Both `control` and `pool` add
code to the supervisor's driver loop that runs whether or not a graph uses
it, so a graph with no runtime control and no pools should not carry either.
A wrong choice is loud: declaring a `pool` without the feature, or calling a
control verb without it, produces an error that names the feature.

The full list with defaults is in the
[feature reference](/reference/features/).

## Logging backends

The supervisor logs lifecycle events through an optional backend:

- `defmt` for on-target logging (the usual choice with embassy and RTT),
- `log` for hosted or std consumers.

With neither enabled the log calls compile to nothing. If you want stale
reports and bring-up lines visible anywhere, enable one.

## Release profile

For smaller, better optimized firmware, set this profile in the workspace
root's `Cargo.toml`:

```toml [Cargo.toml]
[profile.release]
debug = 2
lto = "fat"
opt-level = "s"
codegen-units = 1
```

- `lto = "fat"` runs link-time optimization across the whole dependency
  tree, letting the compiler inline and eliminate code across crate
  boundaries.
- `codegen-units = 1` compiles each crate as one unit instead of parallel
  chunks, enabling more thorough optimization at the cost of compile time.
- `opt-level = "s"` optimizes for size rather than speed, the usual
  embedded budget.
- `debug = 2` keeps full debug symbols in the ELF for probe-rs and GDB. It
  does not change codegen, and debug sections are not flashed to the device.

## Verify the toolchain

```console
rustup target add thumbv7em-none-eabihf   # or your MCU's target
cargo build
```

The library itself is HAL-free, so it builds for any target embassy builds
for, including `x86_64-unknown-linux-gnu` for host tests.

## Next

[Your first graph](/getting-started/first-graph/)
takes the crate from installed to running.
