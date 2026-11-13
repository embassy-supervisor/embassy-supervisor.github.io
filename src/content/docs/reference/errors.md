---
title: Errors and limits
description: The fault and error types, what each failure names, the defaults, and the hard limits.
---

<p class="eyebrow">Reference</p>

# Errors and limits

The crate's error surface is four types, and every failure is **returned,
never panicked** inside the library.

## `NodeFault`

```rust
pub struct NodeFault {
    pub node: &'static TaskNode,
    pub kind: FaultKind,
}
```

`Display` names the node and the cause, so `{fault}` is a complete
escalation message. Four distinct bring-up causes arrive as one typed fault,
and each prints something you can act on:

```text
att-estimator: ready-dep imu-reader did not assert within 2000ms
```

`NodeFault` derives `Debug`, so `.unwrap()` and `.expect()` work. It also
implements `core::error::Error`. For a refused spawn, `source()` returns the
`SpawnError`. `FaultKind`'s `Debug` prints variant names only, such as
`Spawn(Busy)` or `ReadyDepTimeout { dep: "imu-reader" }`. `defmt::Format`
arrives with the `defmt` feature.

| `FaultKind` | what it names | returned by |
|---|---|---|
| `ExecutorSlotEmpty` | an `executor:` slot still empty at the deadline | `start`, `start_node`, `respawn_terminate`, `activate`, `apply_control` (`Activate`), direct `restart`. `run()` and pool growth only log it |
| `ResourceMissing` | a `resources:` slot, or an unprovided `divisible` budget, unfilled at the deadline | same |
| `ReadyDepTimeout { dep }` | a `ready` dep that never asserted | same |
| `Spawn(SpawnError)` | the spawn was refused: a full task pool, a refused `state:` allocation (`heap-state`), or the previous instance of the node still running when its `slot_timeout` ran out (a `spawn:` task that keeps awaiting after its ack) | same |
| `ShutdownTimeout` | a node that missed the shutdown-ack deadline (its divisible shares are released either way) | `stop_node`, `teardown`, `deactivate`, `restart`, `apply_control`, `run()`, `run_pools` and `pool_pass` (a wedged shrink) |
| `RestartStorm` (feature `restart-intensity`) | a `restart` refused because the node already used its `restart_budget:` inside the current window; nothing was stopped and the node keeps running as it was | `restart`, `apply_control` (`Restart` returns it), `run()` |

`run()` absorbs the four bring-up kinds: `ExecutorSlotEmpty`,
`ResourceMissing`, `ReadyDepTimeout` and `Spawn`. It logs each one, reports
it as a `Faulted` lifecycle event, and keeps driving. It returns only
`ShutdownTimeout` and, with `restart-intensity`, `RestartStorm`. To return
other faults, use `run_with` (see [Escalation](#escalation)).

## The other three

- `Aborted`: the cancellation result of `run_cancellable` /
  `run_cancellable_acked`, handed to the worker's body.
- `Resumed`: the pause-cycle result of `run_pausable`, handed to the
  worker's body. Not a failure: the park is already over, and the next
  loop iteration is the fresh cycle.
- `ControlQueueFull`: what `try_request_control` returns when the mailbox is
  full. The async `request_control` instead waits for capacity; neither
  drops a command.

All four implement `Display` and `core::error::Error`.

`SpawnError` is re-used from embassy-executor and only appears inside
`FaultKind::Spawn`.

## Defaults

| knob | default | override |
|---|---|---|
| shutdown-ack timeout (window starts when the node is signalled) | 2 s | `ack_timeout:` per node |
| pre-spawn gate wait (executor slot, resources, ready deps, a previous instance finishing): one shared budget per node in a `start()` wave, per gate in the single-node verbs | 100 ms | `slot_timeout:` |
| control mailbox depth | 4 | fixed |
| trace registries | 4 executors; graphs register onto a linked chain, any number | fixed |
| fresh-spawn grace | a spawn counts as a beat | none needed |

## Hard limits

- **256 nodes per graph** (pool members included): all graph indices are
  `u8`, keeping the dep table and order arrays byte-sized on
  flash-constrained targets.
- **Pool bounds**: `min <= max <= member count`, values fitting `u8`. The
  member count itself stays a literal (it determines how much is emitted).
- **One compose site per binary**; **one set of trace hook symbols per
  binary** (the unnamed graph carries them).
- **Restart budgets** (feature `restart-intensity`): `MAX` in 1..=65535,
  `WINDOW_MS` at least 1.
- **Heartbeat budgets** (feature `liveness-monitor`): a `beat_timeout:` at or
  past `u32::MAX / 2` ticks (about 35.8 min at a 1 MHz tick) is refused at
  compile time.
- **Veto gates** (feature `veto`): at most 32 writers per gate, one spelling
  per gate; the target must be a `VetoGate` with a slot per writer.
- **Budgets** (feature `budget`): one slot per declaring node and pool
  member, inside the 256-slot cap.

## Escalation

`start()` is fail-fast. When the graph is the product, its bring-up fault
is often escalated hard, with a `panic!` into a hardware-watchdog reset. If
the graph cannot come up, nothing should run.

`run()` does not return bring-up faults. It logs each one and reports it as
a `Faulted` lifecycle event. The node stays down and enabled, and `run()`
keeps driving. An app that wants the hard policy has three options:

- call `run_with` with a policy that returns them (see below),
- apply it in a lifecycle consumer, or
- call `start()` first, escalate its result, then call `run()`.

What `run()` does return is the more interesting case:

- A `ShutdownTimeout` names a wedged task. Retry, log and continue
  degraded, or reset, as the domain demands.
- A `RestartStorm` names a crash loop the restart policy refused to feed.
  The node is still running as it was. Deactivating it, degrading or
  resetting is again the application's call.

Re-entering `run()` after a return is safe. It retries every enabled node
that is down. The library's contract ends at reporting the fault with
provenance.

### Choosing what the driver returns

`run_with(&spawner, &mut policy)` is `run()` with a `RunPolicy` of your
choice. The trait has one method:
`returns(&mut self, fault: &NodeFault, origin: Origin) -> bool`.

- `ReturnOnHalt` is what `run()` uses: a missed ack or a refused restart.
- `FailFast` returns every fault, bring-up faults included.
- Your own policy can look at the kind, the node and the `Origin`:
  `Boot`, `Activate`, `Deactivate`, `Restart`, `Bind`, `PoolGrow` or
  `PoolShrink`. `FaultKind::is_bring_up()` tells faults that leave a node
  down from faults about a running node.

The policy is called once for every fault the driver meets, in order. A
fault it does not return is still logged and reported as `Faulted`.

- A wave (the boot wave, an `Activate`, the way back up of a `Restart`)
  runs to the end. `run_with` then returns the first fault the policy took.
  The later faults of that wave still reach the policy.
- A missed ack, a refused restart or a failed pool growth ends its step,
  whatever the policy says.

A policy is a value, so it can count faults or keep them for the caller to
read after `run_with` returns:

```rust
struct ThreeStrikes {
    bring_up_faults: u8,
}

impl RunPolicy for ThreeStrikes {
    fn returns(&mut self, fault: &NodeFault, _origin: Origin) -> bool {
        if !fault.kind.is_bring_up() {
            return true;
        }
        self.bring_up_faults += 1;
        self.bring_up_faults >= 3
    }
}

let mut policy = ThreeStrikes { bring_up_faults: 0 };
loop {
    let fault = sup.run_with(&spawner, &mut policy).await;
    escalate(fault).await;
}
```

The policy runs inside the driver's waves, so it only observes. To act on a
fault, send a command with `try_request_control`. The driver applies it
after the wave.
