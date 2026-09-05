// Wasm lifecycle for the playground. The .wasm binary is fetched and
// compiled once; every run gets a genuinely fresh instance (fresh linear
// memory and statics — the supervisor's statics are never reset in place).
//
// wasm-bindgen's generated init() memoizes: once a glue module is
// initialized, calling init() again returns the same instance. So each run
// dynamically imports a NEW copy of the glue (the ES module cache is keyed
// by URL; the ?run= counter defeats it) and initializes that copy with the
// cached compiled module.

const glueUrl = new URL(
  '../../generated/playground-wasm/embassy_supervisor_playground.js',
  import.meta.url,
);
const wasmUrl = new URL(
  '../../generated/playground-wasm/embassy_supervisor_playground_bg.wasm',
  import.meta.url,
);

/** The glue module's surface (see playground/src/api.rs). */
export interface Wasm {
  default(opts: { module_or_path: WebAssembly.Module }): Promise<unknown>;
  parse_dsl(src: string): ParseOutcome;
  start_run(src: string, behaviorsJson: string): ParseOutcome;
  tick(advanceUs: number): void;
  drain_events(): Snapshot;
  control(nodeIdx: number, op: string): void;
  node_command(nodeIdx: number, op: string): void;
  power(cmd: string): void;
  resource_command(name: string, cmd: string): void;
  set_input(node: string, value: number): void;
  inject(nodeIdx: number, kind: string): void;
  signal_command(signalIdx: number, cmd: string): void;
}

let compiled: Promise<WebAssembly.Module> | null = null;
let runCounter = 0;

function compiledModule(): Promise<WebAssembly.Module> {
  compiled ??= (async () => {
    const resp = await fetch(wasmUrl);
    return WebAssembly.compileStreaming
      ? WebAssembly.compileStreaming(Promise.resolve(resp))
      : WebAssembly.compile(await resp.arrayBuffer());
  })();
  return compiled;
}

/** Instantiate a fresh wasm instance and return its exports. */
export async function freshInstance(): Promise<Wasm> {
  const mod = (await import(/* @vite-ignore */ `${glueUrl.href}?run=${runCounter++}`)) as Wasm;
  await mod.default({ module_or_path: await compiledModule() });
  return mod;
}

export interface ParseError {
  line: number;
  msg: string;
}
export interface Badge {
  item: string;
  clause: string;
  note: string;
}
export interface DepModel {
  name: string;
  ready: boolean;
  bound: boolean;
}
export interface NodeModel {
  name: string;
  mode: string;
  deps: DepModel[];
  task: string | null;
  resources: {
    name: string;
    local: boolean;
    consume: boolean;
    shared: boolean;
    divisible: boolean;
    serialized: boolean;
  }[];
  provides: string[];
  disabled: boolean;
  /** Where the task spawns: an explicit `executor:` or the graph's `default executor`. */
  executor: string | null;
  /** The executor was inherited from `default executor`, not written on the node. */
  executor_defaulted: boolean;
  beat_timeout_ms: number | null;
  reads: { name: string; observed: boolean; beat: boolean; veto: boolean }[];
  writes: { name: string; observed: boolean; beat: boolean; veto: boolean }[];
  pool: string | null;
}
export interface PoolModel {
  name: string;
  members: string[];
  min: number;
  max: number;
  cooldown_ms: number;
}
export interface SignalModel {
  name: string;
  writers: string[];
  readers: string[];
  observed: boolean;
  beat: boolean;
  /** Some writer carries `veto`: the signal runs as a VetoGate. */
  veto: boolean;
  /** The veto-carrying writers, in declaration order (= contributor bit order). */
  veto_writers: string[];
}
export interface GraphModel {
  name: string | null;
  nodes: NodeModel[];
  pools: PoolModel[];
  executors: string[];
  signals: SignalModel[];
  order: number[];
}
export interface ParseOutcome {
  ok: boolean;
  errors: ParseError[];
  lints: string[];
  badges: Badge[];
  model: GraphModel | null;
}

export interface NodeSnap {
  idx: number;
  name: string;
  mode: string;
  running: boolean;
  busy: boolean;
  disabled: boolean;
  collateral: boolean;
  ready: boolean;
  bound_stopped: boolean;
  exited: boolean;
  detached: boolean;
  epoch: number;
  status: string | null;
  ticks_since_beat: number;
  /** Share of the node's first divisible resource: granted / wanted (null without one). */
  grant: number | null;
  want: number | null;
  /** Liveness-policed (beat_timeout declared): only then is beat age meaningful. */
  policed: boolean;
  /** Executor name (null = the root executor): written on the node, or inherited from `default executor`. */
  executor: string | null;
  executor_defaulted: boolean;
  /** Trace executor id this node last polled on (0 = never polled). */
  exec_id: number;
  /** Genuine poll count from the trace recorders. */
  polls: number;
  /** Wall-clock poll durations — browser time, not MCU microseconds. */
  last_poll_us: number;
  max_poll_us: number;
  exec_us: number;
  /** The injected fault the node carries (`stall`, `wedge`), or null. */
  fault: string | null;
}
export interface SignalSnap {
  name: string;
  kind: 'plain' | 'backed' | 'leased' | 'veto';
  writes: number;
  reads: number;
  value: number;
  /** Staged backlog, when a queue behavior maintains one on this signal. */
  depth: number | null;
  leases: number | null;
  drained: boolean | null;
  /** Live Open guards on a backed signal. */
  openers: number | null;
  /** A veto gate's state and the number of contributor bits up. */
  asserted: boolean | null;
  contributors: number | null;
}
export interface PoolSnap {
  name: string;
  members: string[];
  running: number;
  busy: number;
  min: number;
  max: number;
}
export interface LogEntry {
  ts_us: number;
  level: string;
  target: string;
  msg: string;
}
export interface ResourceSnap {
  name: string;
  filled: boolean;
  kind: 'lend' | 'consume' | 'shared' | 'divisible';
  /** The node holding a lent value, while it is out. */
  held_by: string | null;
  /** A budget's provided capacity, the units granted, and the holders stating a want. */
  capacity: number | null;
  granted: number | null;
  claimants: number | null;
}
export interface ExecutorSnap {
  id: number;
  name: string;
  polls: number;
  passes: number;
  /** Wall-clock exec time (browser time). */
  exec_us: number;
  /** The node currently mid-poll on this executor, if any. */
  current: string | null;
}
export interface HealthSnap {
  node: string;
  kind: string;
  /** What the app-owned escalation policy did about it. */
  action: string;
}
export interface Snapshot {
  now_us: number;
  watchdog_bite: boolean;
  logs: LogEntry[];
  nodes: NodeSnap[];
  signals: SignalSnap[];
  pools: PoolSnap[];
  resources: ResourceSnap[];
  executors: ExecutorSnap[];
  health: HealthSnap[];
  faults: string[];
}
