// Live graph view: React Flow over a dagre layout of the parsed model.
//
// The node set is STATIC per model (managed with useNodesState/onNodesChange
// so React Flow can record its dimension measurements — replacing the array
// per frame loses them and every node stays visibility:hidden). Per-frame
// runtime state flows through LiveContext instead, so only the node
// components re-render as snapshots arrive. Node cards carry the control
// verbs (activate / deactivate / restart) and fault injection; edges mirror
// the docs' runtime-view notation (solid deps, signal boxes between
// dataflow parties, dotted resources).
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import {
  ReactFlow,
  Background,
  BackgroundVariant,
  Controls,
  Handle,
  Position,
  useNodesState,
  type Edge,
  type Node,
  type NodeProps,
  type ReactFlowInstance,
} from '@xyflow/react';
import dagre from '@dagrejs/dagre';
import { tokenizeDsl } from './dslHighlight';
import '@xyflow/react/dist/style.css';
import type { GraphModel, NodeSnap, PoolSnap, ResourceSnap, SignalSnap, Snapshot } from '../../lib/playground/wasm';

interface Props {
  model: GraphModel | null;
  snapshot: Snapshot | null;
  stale: Set<string>;
  activeSignals: Set<string>;
  /** Plane grouping from the scenario: cluster label -> item names. */
  planes?: Record<string, string[]>;
  onControl: (idx: number, op: string) => void;
  onInject: (idx: number, kind: string) => void;
  onNodeCommand: (idx: number, op: string) => void;
  onResourceCommand: (name: string, cmd: 'provide' | 'clear') => void;
  /** The item's declaration text, extracted live from the editor. */
  declOf: (name: string) => string | null;
}

/** What the view filter shows: node cards only, plus signals, or everything. */
type ViewFilter = 'nodes' | 'signals' | 'all';

interface Live {
  snap: Map<string, NodeSnap>;
  pool: Map<string, PoolSnap>;
  sig: Map<string, SignalSnap>;
  res: Map<string, ResourceSnap>;
  stale: Set<string>;
  active: Set<string>;
  onControl: (idx: number, op: string) => void;
  onInject: (idx: number, kind: string) => void;
  onNodeCommand: (idx: number, op: string) => void;
  onResourceCommand: (name: string, cmd: 'provide' | 'clear') => void;
  declOf: (name: string) => string | null;
}

const LiveContext = createContext<Live>({
  snap: new Map(),
  pool: new Map(),
  sig: new Map(),
  res: new Map(),
  stale: new Set(),
  active: new Set(),
  onControl: () => {},
  onInject: () => {},
  onNodeCommand: () => {},
  onResourceCommand: () => {},
  declOf: () => null,
});

/** The item's DSL declaration, syntax-colored like the editor. */
function DslSnippet({ text }: { text: string }) {
  const parts: ReactNode[] = [];
  let pos = 0;
  for (const t of tokenizeDsl(text)) {
    if (t.from > pos) parts.push(text.slice(pos, t.from));
    parts.push(
      <span key={t.from} className={t.cls}>
        {text.slice(t.from, t.to)}
      </span>,
    );
    pos = t.to;
  }
  if (pos < text.length) parts.push(text.slice(pos));
  return <pre className="pg-decl-pop">{parts}</pre>;
}

/** Card title that toggles the declaration popover on click. */
function DeclName({ name, item }: { name: string; item?: string }) {
  const live = useContext(LiveContext);
  const [show, setShow] = useState(false);
  const decl = show ? live.declOf(item ?? name) : null;
  return (
    <>
      <button
        className="pg-node-name pg-node-name-btn"
        onClick={() => setShow((v) => !v)}
        title="show this item's declaration"
      >
        {name}
      </button>
      {show && decl && <DslSnippet text={decl} />}
    </>
  );
}

const SIZES = {
  sup: { width: 200, height: 92 },
  pool: { width: 220, height: 106 },
  sig: { width: 124, height: 46 },
  res: { width: 112, height: 38 },
};

const FIT = { padding: 0.08, maxZoom: 1.15 };

function ledClass(snap: NodeSnap | undefined, stale: boolean): string {
  if (!snap) return 'off';
  if (snap.disabled) return 'disabled';
  if (snap.collateral) return 'held';
  if (snap.bound_stopped) return 'bound';
  if (!snap.running) return snap.exited ? 'done' : 'off';
  if (stale) return 'stale';
  if (snap.detached) return 'detached';
  if (!snap.ready) return 'starting';
  return 'up';
}

function ledTitle(cls: string): string {
  return {
    up: 'running and ready',
    starting: 'running, not ready',
    stale: 'running, heartbeat stale',
    bound: 'stopped by a bound dependency',
    done: 'exited',
    off: 'down',
    disabled: 'disabled',
    held: 'held: stopped by a deactivated node; activate it to release',
    detached: 'running, detached: lifecycle ops skip it',
  }[cls] as string;
}

type SupData = { name: string; mode: string; executor: string | null };

function SupNode({ data }: NodeProps<Node<SupData>>) {
  const live = useContext(LiveContext);
  const snap = live.snap.get(data.name);
  const cls = ledClass(snap, live.stale.has(data.name));
  const running = snap !== undefined;
  const canStop = running && snap!.running;
  const [leaveTick, setLeaveTick] = useState(0);
  return (
    <div
      className={`pg-node pg-node-${cls}`}
      onMouseLeave={() => setLeaveTick((t) => t + 1)}
    >
      <Handle type="target" position={Position.Left} />
      <div className="pg-node-head">
        {/* Two columns: name over status on the left, chips stacked on the
            right — so the status line shares its row with a second chip. */}
        <div className="pg-node-title">
          <div className="pg-node-name-row">
            <span className={`pg-led pg-led-${cls}`} title={ledTitle(cls)} />
            <DeclName key={leaveTick} name={data.name} />
          </div>
          <div className="pg-node-meta">
            {data.executor && <span className="pg-exec" title="named executor">{data.executor}</span>}
            {/* A stopped node's last status ("session open") reads as live;
                show it only while the task actually runs. */}
            <span className="pg-node-status">{snap?.running ? (snap.status ?? '') : 'not running'}</span>
            {running && snap!.epoch > 1 && <span className="pg-epoch" title="restart epoch">×{snap!.epoch}</span>}
          </div>
        </div>
        <span className="pg-node-chips">
          <span className={`chip chip-${data.mode}`}>{data.mode}</span>
          {snap?.grant != null && (
            <span className="chip chip-grant" title="budget share: granted / wanted units">
              {snap.grant}/{snap.want}
            </span>
          )}
          {snap?.detached && (
            <span className="chip chip-detached" title="self-managed: teardown and respawn skip it">
              detached
            </span>
          )}
        </span>
      </div>
      {running && (
        <div className="pg-node-actions">
          <button
            onClick={() => live.onControl(snap!.idx, canStop ? 'deactivate' : 'activate')}
            title={canStop ? 'deactivate (stop and disable)' : 'activate (enable and start)'}
          >
            {canStop ? '⏻ stop' : '⏻ start'}
          </button>
          <button onClick={() => live.onControl(snap!.idx, 'restart')} title="restart (rest-for-one cascade)">
            ↻
          </button>
          <select
            className="pg-fault-menu"
            value=""
            onChange={(e) => {
              if (e.target.value) live.onInject(snap!.idx, e.target.value);
            }}
            title="inject a fault"
          >
            <option value="">⚡</option>
            <option value="stall">stall heartbeat</option>
            <option value="wedge">wedge (no shutdown ack)</option>
            <option value="exit">crash (abrupt exit)</option>
            <option value="clear">clear fault</option>
          </select>
        </div>
      )}
      <Handle type="source" position={Position.Right} />
    </div>
  );
}

type PoolData = { name: string; min: number; max: number; members: string[] };

function PoolNode({ data }: NodeProps<Node<PoolData>>) {
  const live = useContext(LiveContext);
  const poolSnap = live.pool.get(data.name);
  const memberSnaps = data.members.map((m) => live.snap.get(m));
  // Live bounds from the snapshot when running; declared bounds before.
  const min = poolSnap?.min ?? data.min;
  const max = poolSnap?.max ?? data.max;
  const running = poolSnap?.running ?? 0;
  const busy = poolSnap?.busy ?? 0;
  const [leaveTick, setLeaveTick] = useState(0);
  const [openMember, setOpenMember] = useState<number | null>(null);
  const member = openMember !== null ? memberSnaps[openMember] : undefined;
  return (
    <div
      className="pg-node pg-pool"
      onMouseLeave={() => {
        setLeaveTick((t) => t + 1);
        setOpenMember(null);
      }}
    >
      <Handle type="target" position={Position.Left} />
      <div className="pg-node-head">
        <div className="pg-node-title">
          <div className="pg-node-name-row">
            <span className="pg-pool-mark" title="elastic pool">
              ⬡
            </span>
            <DeclName key={leaveTick} name={data.name} />
          </div>
          <div className="pg-node-meta">
            <span className="pg-node-status">
              {running} up · {busy} busy
            </span>
          </div>
        </div>
        <span className="pg-node-chips">
          <span className="chip chip-flag">pool</span>
        </span>
      </div>
      <div className="pg-pool-members">
        {memberSnaps.map((m, i) => {
          const cls = ledClass(m, false);
          return (
            <button
              key={i}
              className={`pg-member pg-led-${cls} ${m?.running && m.busy ? 'busy' : ''} ${openMember === i ? 'open' : ''}`}
              disabled={!m}
              onClick={() => setOpenMember((o) => (o === i ? null : i))}
              title={`${data.members[i]}: ${ledTitle(cls)}${m?.running && m.busy ? ', busy' : ''}${m ? ' — click for member controls' : ''}`}
            />
          );
        })}
      </div>
      {member && (
        <div className="pg-member-actions">
          <span className="pg-member-name">{data.members[openMember!]}</span>
          <button
            onClick={() => live.onNodeCommand(member.idx, member.running ? 'stop' : 'start')}
            title={member.running ? 'stop_node this member' : 'start_node this member'}
          >
            {member.running ? '⏻ stop' : '⏻ start'}
          </button>
          <select
            className="pg-fault-menu"
            value=""
            onChange={(e) => {
              if (e.target.value) live.onInject(member.idx, e.target.value);
            }}
            title="inject a fault into this member"
          >
            <option value="">⚡</option>
            <option value="stall">stall heartbeat</option>
            <option value="wedge">wedge (no shutdown ack)</option>
            <option value="exit">crash (abrupt exit)</option>
            <option value="clear">clear fault</option>
          </select>
        </div>
      )}
      <div
        className="pg-pool-meter"
        title={`running ${running} (busy ${busy}) of min ${min} / max ${max}${
          running > busy ? ` · ${running - busy} idle: DeferredShrink keeps one idle spare and shrinks only past it` : ''
        }`}
      >
        <span className="pg-pool-range">
          min {min} · max {max}
        </span>
      </div>
      <Handle type="source" position={Position.Right} />
    </div>
  );
}

type SigData = { name: string; observed: boolean; beat: boolean; veto: boolean };

function SigNode({ data }: NodeProps<Node<SigData>>) {
  const live = useContext(LiveContext);
  const s: SignalSnap | undefined = live.sig.get(data.name);
  const active = live.active.has(data.name);
  const tail = data.name.split('::').pop() ?? data.name;
  const kind = s?.kind ?? 'plain';
  return (
    <div className={`pg-sig pg-sig-${kind} ${active ? 'active' : ''}`}>
      <Handle type="target" position={Position.Left} />
      <div className="pg-sig-name" title={data.name}>
        {tail}
        {kind !== 'plain' && <span className="pg-sig-kind">{kind}</span>}
        {data.beat && <span className="pg-sig-flag" title="observed writes beat the writer">beat</span>}
        {data.veto && kind !== 'veto' && (
          <span className="pg-sig-flag pg-sig-flag-veto" title="a veto gate: any writer's bit forces the safe state">
            veto
          </span>
        )}
      </div>
      <div className="pg-sig-stats">
        <span title="writes">w {s?.writes ?? 0}</span>
        <span title="reads">r {s?.reads ?? 0}</span>
        {s?.leases != null && (
          <span className={`pg-leases ${s.drained ? 'drained' : ''}`} title="live leases">
            ⛓ {s.leases}
            {s.drained ? ' drained' : ''}
          </span>
        )}
        {s?.openers != null && (
          <span className="pg-openers" title="live Open guards: the producer can retire once this reaches 0">
            ⊙ {s.openers}
          </span>
        )}
        {s?.asserted != null && (
          <span className={`pg-veto ${s.asserted ? 'asserted' : ''}`} title="contributor bits up">
            {s.asserted ? '⛔' : '○'} {s.contributors}
          </span>
        )}
      </div>
      <Handle type="source" position={Position.Right} />
    </div>
  );
}

type ResData = { name: string; kind: string };

function ResNode({ data }: NodeProps<Node<ResData>>) {
  const live = useContext(LiveContext);
  const snap = live.res.get(data.name);
  const filled = snap?.filled ?? false;
  const kind = snap?.kind ?? data.kind;
  const title = snap?.held_by
    ? `lent to ${snap.held_by}`
    : snap?.capacity != null
      ? `budget: ${snap.granted} of ${snap.capacity} units granted to ${snap.claimants} claimant${snap.claimants === 1 ? '' : 's'}`
      : filled
        ? 'slot provided'
        : 'slot empty';
  return (
    <div className={`pg-res ${filled ? 'filled' : ''}`} title={title}>
      <Handle type="target" position={Position.Left} />
      <span className="pg-res-dot" />
      {data.name}
      <span className={`pg-res-kind pg-res-kind-${kind}`}>{kind}</span>
      {snap && !filled && !snap.held_by && (
        <button
          className="pg-res-provide"
          onClick={() => live.onResourceCommand(data.name, 'provide')}
          title="re-provide this slot by hand"
        >
          +
        </button>
      )}
      {snap?.held_by && <span className="pg-res-holder">→ {snap.held_by}</span>}
      {snap?.capacity != null && (
        <span className="pg-res-holder">
          {snap.granted}/{snap.capacity}
        </span>
      )}
      <Handle type="source" position={Position.Right} />
    </div>
  );
}

type PlaneData = { label: string };

function PlaneNode({ data }: NodeProps<Node<PlaneData>>) {
  return <div className="pg-plane-label">{data.label}</div>;
}

const nodeTypes = { sup: SupNode, pool: PoolNode, sig: SigNode, res: ResNode, plane: PlaneNode };

interface Layout {
  cards: { id: string; kind: 'sup' | 'pool' | 'sig' | 'res'; plane?: string }[];
  planeBoxes: { id: string; label: string; x: number; y: number; width: number; height: number }[];
  edges: Edge[];
  positions: Map<string, { x: number; y: number }>;
}

function computeLayout(model: GraphModel, planes: Record<string, string[]>, view: ViewFilter): Layout {
  const compound = Object.keys(planes).length > 0;
  const g = new dagre.graphlib.Graph({ compound });
  // tight-tree + small separations keep the layout compact so fitView does
  // not have to zoom the cards down to illegibility. Compound graphs need
  // the default ranker: tight-tree mislays clusters.
  g.setGraph({
    rankdir: 'LR',
    nodesep: 14,
    ranksep: 32,
    edgesep: 8,
    marginx: 8,
    marginy: 8,
    ...(compound ? {} : { ranker: 'tight-tree' }),
  });
  g.setDefaultEdgeLabel(() => ({}));

  const memberPool = new Map<string, string>();
  for (const p of model.pools) for (const m of p.members) memberPool.set(m, p.name);
  const cardOf = (name: string) => memberPool.get(name) ?? name;

  // Plane membership: scenario labels name nodes or pools.
  const planeOf = new Map<string, string>();
  for (const [label, items] of Object.entries(planes)) {
    for (const item of items) planeOf.set(cardOf(item), label);
  }

  const cards: Layout['cards'] = [];
  for (const n of model.nodes) {
    if (!n.pool) cards.push({ id: n.name, kind: 'sup', plane: planeOf.get(n.name) });
  }
  for (const p of model.pools) cards.push({ id: p.name, kind: 'pool', plane: planeOf.get(p.name) });
  if (view !== 'nodes') {
    for (const s of model.signals) cards.push({ id: `sig:${s.name}`, kind: 'sig' });
  }
  if (view === 'all') {
    const resNames = new Set<string>();
    for (const n of model.nodes) {
      for (const r of n.provides) resNames.add(r);
      for (const r of n.resources) resNames.add(r.name);
    }
    for (const r of resNames) cards.push({ id: `res:${r}`, kind: 'res' });
  }

  const planeIds = new Map<string, string>();
  if (compound) {
    for (const label of Object.keys(planes)) {
      const id = `plane:${label}`;
      planeIds.set(label, id);
      g.setNode(id, {});
    }
  }
  for (const c of cards) {
    g.setNode(c.id, { ...SIZES[c.kind] });
    if (c.plane && planeIds.has(c.plane)) g.setParent(c.id, planeIds.get(c.plane)!);
  }

  const present = new Set(cards.map((c) => c.id));
  const edges: Edge[] = [];
  const seen = new Set<string>();
  const addEdge = (e: Edge) => {
    if (!present.has(e.source) || !present.has(e.target)) return;
    if (!seen.has(e.id)) {
      seen.add(e.id);
      edges.push(e);
      g.setEdge(e.source, e.target);
    }
  };

  for (const n of model.nodes) {
    const to = cardOf(n.name);
    for (const d of n.deps) {
      const from = cardOf(d.name);
      const label = d.bound ? 'ready bound' : d.ready ? 'ready' : undefined;
      addEdge({
        id: `dep:${from}->${to}`,
        source: from,
        target: to,
        label,
        className: d.bound ? 'pg-edge-bound' : 'pg-edge-dep',
        markerEnd: 'arrowclosed' as never,
      });
    }
    for (const w of n.writes) {
      addEdge({
        id: `w:${to}->${w.name}`,
        source: to,
        target: `sig:${w.name}`,
        className: 'pg-edge-sig',
      });
    }
    for (const r of n.reads) {
      addEdge({
        id: `r:${r.name}->${to}`,
        source: `sig:${r.name}`,
        target: to,
        className: 'pg-edge-sig',
      });
    }
    for (const pr of n.provides) {
      addEdge({ id: `p:${to}->${pr}`, source: to, target: `res:${pr}`, className: 'pg-edge-res' });
    }
    for (const rs of n.resources) {
      addEdge({ id: `c:${rs.name}->${to}`, source: `res:${rs.name}`, target: to, className: 'pg-edge-res' });
    }
  }
  // When the filter hides signals, keep the dataflow visible as direct
  // writer -> reader edges so the graph still reads as a system.
  if (view === 'nodes') {
    for (const sig of model.signals) {
      for (const w of sig.writers) {
        for (const r of sig.readers) {
          const from = cardOf(w);
          const to = cardOf(r);
          if (from === to) continue;
          addEdge({
            id: `df:${from}->${to}`,
            source: from,
            target: to,
            className: 'pg-edge-sig',
          });
        }
      }
    }
  }

  dagre.layout(g);
  const positions = new Map<string, { x: number; y: number }>();
  for (const c of cards) {
    const pos = g.node(c.id);
    const size = SIZES[c.kind];
    let { x, y } = { x: pos.x - size.width / 2, y: pos.y - size.height / 2 };
    // React Flow child positions are relative to their parent's origin.
    if (c.plane && planeIds.has(c.plane)) {
      const cl = g.node(planeIds.get(c.plane)!);
      x -= cl.x - cl.width / 2;
      y -= cl.y - cl.height / 2;
    }
    positions.set(c.id, { x, y });
  }
  const planeBoxes: Layout['planeBoxes'] = [];
  for (const [label, id] of planeIds) {
    const cl = g.node(id);
    if (!cl || !Number.isFinite(cl.x)) continue;
    planeBoxes.push({
      id,
      label,
      x: cl.x - cl.width / 2,
      y: cl.y - cl.height / 2,
      width: cl.width,
      height: cl.height,
    });
  }
  return { cards, planeBoxes, edges, positions };
}

function resKindOf(model: GraphModel, name: string): string {
  for (const n of model.nodes) {
    for (const r of n.resources) {
      if (r.name === name) return r.divisible ? 'divisible' : r.consume ? 'consume' : r.shared ? 'shared' : 'lend';
    }
  }
  return 'lend';
}

function buildNodes(model: GraphModel, layout: Layout): Node[] {
  // Parents must precede their children in React Flow's node array.
  const groups: Node[] = layout.planeBoxes.map((b) => ({
    id: b.id,
    type: 'plane',
    position: { x: b.x, y: b.y },
    style: { width: b.width, height: b.height },
    className: 'pg-plane',
    data: { label: b.label },
    selectable: false,
    draggable: false,
  }));
  const cards = layout.cards.map((c) => {
    const position = layout.positions.get(c.id)!;
    const parent = c.plane ? { parentId: `plane:${c.plane}` } : {};
    if (c.kind === 'sup') {
      const m = model.nodes.find((n) => n.name === c.id)!;
      return {
        id: c.id,
        type: 'sup',
        position,
        ...parent,
        data: { name: m.name, mode: m.mode, executor: m.executor } satisfies SupData,
      };
    }
    if (c.kind === 'pool') {
      const p = model.pools.find((x) => x.name === c.id)!;
      return {
        id: c.id,
        type: 'pool',
        position,
        ...parent,
        data: { name: p.name, min: p.min, max: p.max, members: p.members } satisfies PoolData,
      };
    }
    if (c.kind === 'sig') {
      const name = c.id.slice(4);
      const sm = model.signals.find((s) => s.name === name)!;
      return {
        id: c.id,
        type: 'sig',
        position,
        data: { name, observed: sm.observed, beat: sm.beat, veto: sm.veto } satisfies SigData,
      };
    }
    const name = c.id.slice(4);
    return { id: c.id, type: 'res', position, data: { name, kind: resKindOf(model, name) } satisfies ResData };
  });
  return [...groups, ...cards];
}

export default function GraphPane({
  model,
  snapshot,
  stale,
  activeSignals,
  planes,
  onControl,
  onInject,
  onNodeCommand,
  onResourceCommand,
  declOf,
}: Props) {
  const [view, setView] = useState<ViewFilter>('all');
  const layout = useMemo(
    () => (model ? computeLayout(model, planes ?? {}, view) : null),
    [model, planes, view],
  );

  const [nodes, setNodes, onNodesChange] = useNodesState<Node>([]);
  const needsFit = useRef(true);
  useEffect(() => {
    needsFit.current = true;
    setNodes(model && layout ? buildNodes(model, layout) : []);
  }, [model, layout, setNodes]);

  // Fit once the fresh node set has real dimensions: the measurement event
  // is the only ordering guarantee, timers race it under throttling.
  const handleNodesChange = useCallback(
    (changes: Parameters<typeof onNodesChange>[0]) => {
      onNodesChange(changes);
      if (needsFit.current && changes.some((c) => c.type === 'dimensions')) {
        // Fit only once every card is measured: plane nodes come pre-sized,
        // so the first dimensions event fires before the rest exist and a
        // premature fit computes NaN bounds for a frame.
        const all = rfRef.current?.getNodes() ?? [];
        if (
          all.length > 0 &&
          all.every((n) => n.type === 'plane' || (n.measured?.width ?? 0) > 0)
        ) {
          needsFit.current = false;
          setTimeout(() => rfRef.current?.fitView(FIT), 0);
        }
      }
    },
    [onNodesChange],
  );

  const rfRef = useRef<ReactFlowInstance | null>(null);
  const hostRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const t = setTimeout(() => rfRef.current?.fitView(FIT), 60);
    return () => clearTimeout(t);
  }, [layout]);

  // Refit when the pane itself resizes (editor toggled, window resized).
  // Depends on `layout`: the host div only exists once a model parsed, so a
  // mount-only effect would observe nothing and the toggle would leave the
  // content fitted for the old pane width.
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    let t: ReturnType<typeof setTimeout>;
    let first = true;
    const ro = new ResizeObserver(() => {
      // The initial observation fires on attach; only real resizes refit.
      if (first) {
        first = false;
        return;
      }
      clearTimeout(t);
      t = setTimeout(() => rfRef.current?.fitView(FIT), 120);
    });
    ro.observe(host);
    return () => {
      clearTimeout(t);
      ro.disconnect();
    };
  }, [layout]);

  const edges = useMemo(() => {
    if (!layout) return [] as Edge[];
    return layout.edges.map((e) => {
      const isSig = e.className === 'pg-edge-sig';
      const sigName = isSig ? (e.id.startsWith('w:') ? e.target.slice(4) : e.source.slice(4)) : null;
      return { ...e, animated: sigName !== null && activeSignals.has(sigName) };
    });
  }, [layout, activeSignals]);

  const live = useMemo<Live>(
    () => ({
      snap: new Map(snapshot?.nodes.map((n) => [n.name, n]) ?? []),
      pool: new Map(snapshot?.pools.map((p) => [p.name, p]) ?? []),
      sig: new Map(snapshot?.signals.map((s) => [s.name, s]) ?? []),
      res: new Map(snapshot?.resources.map((r) => [r.name, r]) ?? []),
      stale,
      active: activeSignals,
      onControl,
      onInject,
      onNodeCommand,
      onResourceCommand,
      declOf,
    }),
    [snapshot, stale, activeSignals, onControl, onInject, onNodeCommand, onResourceCommand, declOf],
  );

  if (!model) {
    return <div className="pg-graph-empty">Fix the declaration to see the graph.</div>;
  }

  return (
    <div className="pg-flow-host" ref={hostRef}>
      <div className="pg-view-filter" role="group" aria-label="Graph detail level">
        <button className={view === 'nodes' ? 'on' : ''} onClick={() => setView('nodes')} title="node cards only">
          nodes
        </button>
        <button className={view === 'signals' ? 'on' : ''} onClick={() => setView('signals')} title="nodes and signals">
          +signals
        </button>
        <button className={view === 'all' ? 'on' : ''} onClick={() => setView('all')} title="nodes, signals and resources">
          +resources
        </button>
      </div>
      <LiveContext.Provider value={live}>
        <ReactFlow
          nodes={nodes}
          edges={edges}
          onNodesChange={handleNodesChange}
          onInit={(inst) => {
            rfRef.current = inst;
          }}
          nodeTypes={nodeTypes}
          minZoom={0.08}
          maxZoom={1.6}
          nodesConnectable={false}
          deleteKeyCode={null}
          proOptions={{ hideAttribution: true }}
        >
          <Background variant={BackgroundVariant.Dots} gap={22} size={1.5} className="pg-flow-bg" />
          <Controls showInteractive={false} />
        </ReactFlow>
      </LiveContext.Provider>
    </div>
  );
}
