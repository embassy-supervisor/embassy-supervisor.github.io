---
title: Glossary
description: The vocabulary of embassy-supervisor in one page.
---

<p class="eyebrow">Reference</p>

# Glossary

<dl>

<dt>node</dt>
<dd>A declared task with a name, a mode and optional clauses. The macro
emits one <code>pub static NAME: TaskNode</code> per node, usable anywhere
in the application.</dd>

<dt>pool</dt>
<dd>A declared set of single-instance workers scaled between
<code>min</code> and <code>max</code> by a policy. Members share one worker
shell; take-kind resources become per-member slot arrays.</dd>

<dt>floor</dt>
<dd>The pool's member 0: always-on when <code>min >= 1</code>, the member a
<code>deps: [POOL]</code> edge resolves to.</dd>

<dt>mode</dt>
<dd><code>Terminate</code> (spawned at boot, respawned after a wake),
<code>Pause</code> (parks holding its resources), or
<code>OnDemand</code> (started by a pool policy or control).</dd>

<dt>executor slot</dt>
<dd>A runtime-filled <code>SendSpawner</code> static declared with
<code>executor NAME;</code> and targeted with <code>executor: NAME</code>.
The two uses: an interrupt-priority tier on the same core, or a second
core's own executor.</dd>

<dt>resource / slot</dt>
<dd>A value one party builds and one worker owns while it runs, handed over
through a <code>ResourceSlot</code> at spawn. Kinds: lend (default),
<code>consume</code>, <code>shared</code>, <code>divisible</code> (feature
<code>budget</code>), with <code>local</code> composing onto the value
kinds for <code>!Send</code> values.</dd>

<dt>gate</dt>
<dd>Something a spawn waits on, bounded by the node's
<code>slot_timeout</code>: an executor slot, a resource slot, or a
<code>ready</code> dep. Failing closed produces a fault naming the gate.</dd>

<dt>signal / coupling</dt>
<dd>A <code>'static</code> two nodes both touch (a <code>Watch</code>,
<code>Channel</code>, mutex, atomic), declared in
<code>reads:</code>/<code>writes:</code> or derived by
<code>#[dataflow]</code>. Couplings may be cyclic and never feed the
topological sort.</dd>

<dt>budget / claimant</dt>
<dd>Feature <code>budget</code>: a <code>divisible</code> resource declares
one <code>Budget&lt;K&gt;</code> with a slot per holder. Each holder
receives a <code>Claimant</code>: it states a <code>want</code> and
receives a <code>grant</code>, and the supervisor releases the share when
the holder stops. An allocator <code>provide</code>s the capacity and
re-divides it under a <code>BudgetPolicy</code>.</dd>

<dt>veto gate / contributor</dt>
<dd>Feature <code>veto</code>: a <code>VetoGate&lt;N&gt;</code> stays
asserted while any contributor holds its bit and releases only when all
do. Each <code>veto</code> writer owns one slot, so a writer that stops
with its bit up leaves the gate asserted.</dd>

<dt>Open guard</dt>
<dd>What <code>node.open(&SIG).await</code> returns for a gated signal: a
counted <code>Deref</code> handle. The count is the reader's hold on the
producer, and its slow leak to zero is what <code>retire</code> waits
for.</dd>

<dt>retire</dt>
<dd>A producer-side verb: resolve once a <code>Backed</code> signal's
openers have been gone a whole cooldown, then withdraw readiness and
request the node's own <code>Deactivate</code>. The next
<code>open</code> starts it again.</dd>

<dt>serialized</dt>
<dd>A <code>shared</code> slot marker that makes "every holder runs on one
executor" a compile-time rule: priority ceiling by construction for a
serialized link, at no runtime cost.</dd>

<dt>beat / heartbeat</dt>
<dd>A task's sign of life. Raised by <code>beat()</code>, a
<code>beat_put</code>/<code>beat_writer</code> access, or polled from an
<code>observed beat</code> signal entry. Consumed by
<code>is_stale()</code> and the liveness sweep.</dd>

<dt>readiness</dt>
<dd>A task-asserted "I am actually serving" latch that gates dependents
(<code>deps: [X ready]</code>). Status, not control, unless the edge is
<code>bound</code>.</dd>

<dt>epoch</dt>
<dd>A per-node activation generation counter, for a running consumer to
notice a provider restarted underneath it.</dd>

<dt>parked</dt>
<dd>Either a node declared with no worker (the app spawns it by hand), or a
<code>Pause</code> instance stopped and waiting on
<code>wait_resume()</code>, keeping its held resources.</dd>

<dt>disabled</dt>
<dd>The control latch: not started at boot (<code>disabled;</code>) or
deactivated directly (<code>Deactivate</code> marks its target). Survives
wake respawns and pool regrows until an <code>Activate</code> clears it.</dd>

<dt>collateral</dt>
<dd>Shown as "held" in the playground: stopped only as a dependent of a
deactivated node. Blocks bring-up like <code>disabled</code>, but
<code>Activate</code> on the ancestor releases it once no disabled node
remains among its dependencies.</dd>

<dt>detached</dt>
<dd>A task that called <code>set_detached(true)</code>: self-managed from
then on. Every lifecycle verb skips it, including teardown and cascades.</dd>

<dt>wave</dt>
<dd>How starts and stops propagate: signal every node whose pass conditions
hold (deps up + gates satisfied) or whose stopping dependents have all
acked, and repeat. Independent branches move concurrently; providers keep
serving through their dependents' cleanup.</dd>

<dt>ack</dt>
<dd>The task-side half of a stop: <code>ack_dropped()</code> (or the
combinator doing it). Missed after its window (2 s default, <code>ack_timeout:</code> per node):
a <code>ShutdownTimeout</code> fault naming the node.</dd>

<dt>fragment</dt>
<dd>A module- or crate-owned slice of a graph, declared with
<code>supervisor_fragment!</code> and relayed verbatim into one
<code>compose_graph!</code> site per binary.</dd>

<dt>graph</dt>
<dd>The macro-emitted bundle: node slots, dependency table, compile-time
order, pools. One <code>pub static GRAPH</code> (or a named variant) drives
one <code>Supervisor</code>.</dd>

</dl>
