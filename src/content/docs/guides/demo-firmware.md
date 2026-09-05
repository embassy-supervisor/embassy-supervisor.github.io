---
title: The reference firmware
description: A complete flashable RP2350 application that exercises every part of the supervisor.
---

<p class="eyebrow">Guides</p>

# The reference firmware

The [`firmware/` crate](https://github.com/cedrivard/embassy-supervisor/tree/main/firmware)
in the repository is a complete, flash-it-today application on an RP2350,
built to put every part of the supervisor through its paces. When the docs
say "this is the shape", this firmware is usually the working instance.

## What it demonstrates

- **USB networking** (TCP/IP over the USB cable, no extra hardware) and an
  **HTTP control and observability plane**: drive runtime control
  (start/stop/pause/resume) and watch the graph respond live.
- An **elastic pool of keep-alive workers** that grows under concurrent
  connections and shrinks after a cooldown.
- **Multi-executor and multi-core placement**: the supervisor itself on an
  interrupt tier above everything it polices, a heartbeat on a second
  interrupt tier, the rest of the graph in thread mode through one
  `default executor` line, and a control-started compute load on the second
  core.
- **Fault injection** from the dashboard: stall, wedge, crash or hog any
  node and watch the monitor, the ack window and the trace react.
- **OTA firmware update** with a safe A/B swap and automatic rollback,
  orchestrated as a lifecycle transition.
- **Tracing**: per-task and per-executor stats over the same HTTP plane.

## The graph

The firmware's graph is composed from module-owned fragments (the network
module declares its slice, the HTTP module its own), assembled by one
`compose_graph!` site. Four executors: the supervisor runs on its own
`InterruptExecutor` (P1), the heartbeat on a second one (P2), core 1 runs the
bench, and everything else lands in thread mode through
`default executor THREAD;`. A node's placement is one field in the
declaration, or nothing at all.

| Node | Mode | Role |
|---|---|---|
| `net` | Terminate | brings up the USB network stack, provides its handle |
| `http` | pool 1..2 | the elastic HTTP worker pool |
| `watchdog` | Terminate | detached daemon: feeds the hardware watchdog unconditionally, logs liveness events and trace stalls |
| `heartbeat` | Pause | LED heartbeat on the interrupt tier, parks across sleep |
| `ota` | Terminate | disabled at boot, control-started for updates |
| `ota_confirm` | Terminate | run-once confirmation after an update |
| `bench` | Terminate | compute load on core 1, control-started |

Gate budgets (`slot_timeout:` / `beat_timeout:`) are declared per node: the
OTA path waits on the network with a raised budget, the heartbeat carries a
15 s beat budget, and the whole set is visible in the graph source, not in
scattered constants.

## Build and run

Requires a debug probe and [probe-rs](https://probe.rs); defmt logs stream
over the probe and the USB port is the network link.

```console
# build both crates (release matches the OTA heap budget and image size)
cargo build --release -p bootloader
cargo build --release -p firmware

# wipe flash so state starts blank, flash the bootloader once
probe-rs erase --chip RP235x
probe-rs download --chip RP235x target/thumbv8m.main-none-eabihf/release/bootloader

# flash and run the firmware
cargo run --release -p firmware
```

On the host side, bring up the USB network interface (the device enumerates
as a USB-CDC-NCM link) and the control plane answers on HTTP.

## Exercise it

- **Pool:** hold several concurrent connections open and watch the pool grow
  in the task view; drop them and watch it shrink after the cooldown.
- **Control:** start and stop `bench` (or `ota`) from the dashboard or
  `curl`, and watch the dependency cascade do the right thing.
- **OTA:** POST a compressed image; the device acks, drains, swaps and comes
  back, rolling over if the new image fails to confirm.
- **Faults:** open the ⚡ menu on any row. Stall the heartbeat and the monitor
  flags it stale. Wedge and pause a task and the supervisor reports a timeout
  while the dashboard stays up. Crash `bench` to restart it, or hog `http0`
  and watch the dashboard freeze for three seconds while the LED keeps
  blinking.

## Reading the observability data

Every response from `/api/tasks` carries the trace snapshot:

- **System / heap**: arena size and bytes free. Healthy is a steady baseline
  across load.
- **Per executor**: `idle_ticks`, `exec_ticks`, `polls`, `passes`. Over a
  window `dt`: idle share, in-poll share, and by subtraction the executor
  overhead and unsupervised share.
- **Per task**: the lifecycle flags, `epoch`, `exec_ticks`, `polls`,
  `max_poll_ticks`, `status` and `deps` (`status` is the node's one-line
  `report_status` self-description). The `max_poll_ticks` watermark names a
  task that hogged its executor; `polls`-per-pass is the wake-storm tell.

The firmware README walks a load test with `wrk` and interprets a real
report line by line.

## Portability

The supervisor crate is HAL-agnostic and reused verbatim on any embassy
target. Porting the firmware to another MCU means swapping `embassy-rp` for
the matching HAL crate and rewriting the pin and peripheral setup; the graph,
the workers' supervisor-facing halves, the control plane and the OTA
sequence carry over unchanged.

## Next

The [feature reference](/reference/features/) lists
exactly which features the firmware turns on and what each buys.
