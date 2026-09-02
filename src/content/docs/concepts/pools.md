---
title: Elastic pools
description: Worker pools that grow under load and shrink after a cooldown, within a declared member budget.
---

<p class="eyebrow">Concepts</p>

# Elastic pools

A `pool` item declares a fixed set of single-instance members and lets a
scaling policy move the running count between `min` and `max`:

```rust
supervisor_graph! {
    node BROKER = Terminate, task: broker_worker;
    pool WORKERS = [Terminate, OnDemand, OnDemand, OnDemand], deps: [BROKER],
        task: http_worker,
        policy: embassy_supervisor::DeferredShrink::new(
            embassy_time::Duration::from_secs(4)),
        min: 1, max: 4;
}
```

Four member slots. `min: 1` is the always-on floor; growth to `max: 4`
happens under load. The mode list reads: member 0 is `Terminate` (started at
boot, the floor a `deps: [WORKERS]` edge resolves to), the rest are
`OnDemand`, started by the policy and stopped by it.

```mermaid
flowchart LR
    accDescr: A pool grows under load and shrinks after a cooldown
    F["floor member 0<br/>Terminate · always on"]:::task
    M1["member 1<br/>OnDemand"]:::pool
    M2["member 2<br/>OnDemand"]:::pool
    M3["member 3<br/>OnDemand"]:::pool

    F -. "all busy · below max<br/>grow now" .-> M1
    M1 -. "still busy" .-> M2
    M2 -. "still busy" .-> M3
    M3 -. "idle surplus 4 s<br/>shrink" .-> M2
```

## How scaling decides

Workers report load with `mark_busy()` and `mark_idle()`. A real transition
fires the scale signal itself; no manual `request_scale()` needed. The
supervisor's `run_pools` future wakes on each scale request (it never polls),
asks each pool's `ScalingPolicy` for a decision, and starts or stops one
member accordingly.

The built-in `DeferredShrink` policy grows immediately when saturated (no
idle member, below `max`) and shrinks only after an idle surplus has
persisted for the configured cooldown, with one idle spare as the stable
dead-band so a single spare never flaps. Your own policy is one sync,
allocation-free fn: implement `ScalingPolicy`.

Two safety rails: a member is never grown while one of its declared
dependencies is down (or, with `readiness`, while a `ready`-marked dep is
unready), and a shrink is a stop like any other, with the full shutdown
handshake. A wedged member surfaces as the ordinary ack timeout.

## Per-member resources

Take-kind resource entries (the default lend, and `consume`) become
**per-member slot arrays**: `pub static RES: [ResourceSlot<T>; K]`. Member
`i` takes and restores element `i` exclusively, so members never contend, the
floor comes up with only floor-many elements provided, and a lent value
survives a shrink and regrow on the same index. The per-connection-worker
shape falls out: provide one connection handle per element as they open.

`shared` entries (including `shared local`) stay one fan-out slot for the
whole pool. Only take-kind `local` is rejected on pools. A worker derives
its own index with `WORKERS_POOL.member_index(node)` to reach per-member app
state without per-member spawn arguments.

A `divisible` entry on a pool is one budget shared with the rest of the
graph, and each member holds its own claimant slot in it. A shrink is a
stop like any other, so a shrunken member's share is released for the
remaining holders; a regrown member claims again on its next run.

## Budgeting from the declaration

The emitted constants exist for const-context sizing, so a related capacity
derives from the DSL instead of a duplicated number:

```rust
// One socket per concurrently-running worker, plus one for DNS:
pub const SOCKET_BUDGET: usize = WORKERS_MAX + 1;
let resources = embassy_net::StackResources::<SOCKET_BUDGET>::new();
```

A `const` cannot read the member static array, hence the named constants.

## `cancel` pools

A pool takes the `cancel` flag like a node, applying to the one shell
all members share. That is what lets the policy retire a worker that would
never have acked a stop: the member's future is dropped in place and its
per-member resource restores to its own slot index, ready for the regrow.
The load signal moves outside the worker: whatever hands it work calls
`WORKERS[i].mark_busy()` / `mark_idle()`, since the worker holds no node.

## On another core

A whole pool can carry `executor: CORE1`: an elastic pool on the second
core, scaled by the first core's supervisor. See
[Executors and cores](/concepts/placement/).

## Next

[Runtime control](/concepts/control/) covers driving
pools (and everything else) from anywhere in the application.
