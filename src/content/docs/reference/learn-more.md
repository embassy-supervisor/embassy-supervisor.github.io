---
title: Learn more
description: The Rust, async and embassy resources this site leans on, plus where the project lives.
---

<p class="eyebrow">Reference</p>

# Learn more

These docs stay focused on the supervision layer and link out for
foundations. The pages below are the ones worth bookmarking.

## Rust and async

- [The Rust book](https://doc.rust-lang.org/book/) - the language, from
  ownership onward.
- [Asynchronous Programming in Rust](https://rust-lang.github.io/async-book/)
  - the futures model: state machines, wakers, executors.
- [The Embedded Rust Book](https://docs.rust-embedded.org/book/) - no_std,
  HALs, cross-compiling, and the `embedded-hal` trait family.
- [The Embedded Rust Discovery Book](https://docs.rust-embedded.org/discovery/)
  - hands-on, if you learn best from hardware.

## Embassy

- [The embassy book](https://embassy.dev/book/) - tasks, executors, time,
  sync primitives, and HAL usage.
- [embassy executor docs](https://docs.rs/embassy-executor) - what
  `#[embassy_executor::task]` really does, task pools, `Spawner`, and the
  interrupt-executor tiers this supervisor places tasks across.
- [embassy time](https://docs.rs/embassy-time) - timers, tickers, and the
  mock driver used in desktop tests.
- [embassy sync](https://docs.rs/embassy-sync) - `Signal`, `Watch`,
  `Channel`, `Mutex`: the primitives the dataflow layer declares over.
- [The embassy FAQ](https://embassy.dev/book/faq.html) - async patterns,
  pitfalls, and why things are the way they are.

## Ecosystem

- [defmt](https://defmt.ferrous-systems.com/) - efficient logging for
  embedded, the usual log backend on target.
- [probe-rs](https://probe.rs) - flashing and debugging, including the
  RP2350 the reference firmware runs on.
- [embedded-hal](https://docs.rs/embedded-hal) - the driver trait
  abstractions your workers consume.
- [bytemuck](https://docs.rs/bytemuck) - `Zeroable`, behind the
  `state: zeroed Type` clause.

## This project

- [GitHub repository](https://github.com/cedrivard/embassy-supervisor) -
  source, the reference firmware, the bootloader and the host tools.
- [0.4 to 0.5 migration table](https://github.com/cedrivard/embassy-supervisor/blob/main/supervisor/README.md#migration)
  - what changed between the releases, row by row.
- [embassy-supervisor on crates.io](https://crates.io/crates/embassy-supervisor)
  and [on docs.rs](https://docs.rs/embassy-supervisor) - the API, item by
  item.
- [Video walkthrough](https://youtu.be/rlLaaMKMPWo) - the architecture in
  one sitting: the declaration, the guarantees, executor tiers, the
  lifecycle matrix.
- [embassy-supervisor-observe](https://crates.io/crates/embassy-supervisor-observe)
  - the facade a signal library implements in one line.
