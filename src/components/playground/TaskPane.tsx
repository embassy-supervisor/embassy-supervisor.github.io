// A `top` for the graph: one row per node, running or not, fed by the trace
// recorders (genuine poll/pass counts), the wall-clock hooks (browser-time
// durations), and the task-side status APIs (report_status, mark_busy).
//
// Durations here are wall time measured in the browser — wasm polled on the
// page's main thread — never MCU microseconds; the docs' tracing guide covers
// measuring on hardware. "core" is the scenario's *declared* placement:
// wasm is single-threaded, and `trace::set_core_id_fn` is the on-hardware
// mechanism.
import { useMemo, useState } from 'react';
import type { NodeSnap, Snapshot } from '../../lib/playground/wasm';

interface Props {
  snapshot: Snapshot | null;
  stale: Set<string>;
  /** Declared core per executor name (root = 0 unless declared). */
  cores: Record<string, number>;
}

/** A poll this long (browser time) is a task hogging its executor — a
 * different failure than a missed heartbeat, which is a task not making
 * progress. */
const HOG_POLL_US = 50_000;

type SortKey = 'name' | 'executor' | 'status' | 'polls' | 'last' | 'max' | 'share' | 'beat' | 'epoch';

// Same vocabulary as the graph pane's LED, so a node reads alike in both.
function ledClass(n: NodeSnap, stale: boolean): string {
  if (n.disabled) return 'disabled';
  if (n.collateral) return 'held';
  if (n.bound_stopped) return 'bound';
  if (!n.running) return n.exited ? 'done' : 'off';
  if (stale) return 'stale';
  if (n.detached) return 'detached';
  if (!n.ready) return 'starting';
  return 'up';
}

function fmtUs(us: number): string {
  if (us === 0) return '—';
  if (us < 1000) return `${us}µs`;
  return `${(us / 1000).toFixed(1)}ms`;
}

export default function TaskPane({ snapshot, stale, cores }: Props) {
  const [sortKey, setSortKey] = useState<SortKey>('name');
  const [sortDesc, setSortDesc] = useState(false);
  const [execFilter, setExecFilter] = useState<string>('');

  const execTotals = useMemo(() => {
    const m = new Map<number, number>();
    for (const e of snapshot?.executors ?? []) m.set(e.id, e.exec_us);
    return m;
  }, [snapshot]);

  const rows = useMemo(() => {
    if (!snapshot) return [];
    const list = snapshot.nodes.filter((n) => !execFilter || (n.executor ?? 'root') === execFilter);
    const share = (n: NodeSnap) => {
      const total = execTotals.get(n.exec_id) ?? 0;
      return total > 0 ? n.exec_us / total : 0;
    };
    const key: (n: NodeSnap) => number | string = {
      name: (n: NodeSnap) => n.name,
      executor: (n: NodeSnap) => n.executor ?? 'root',
      status: (n: NodeSnap) => n.status ?? '',
      polls: (n: NodeSnap) => n.polls,
      last: (n: NodeSnap) => n.last_poll_us,
      max: (n: NodeSnap) => n.max_poll_us,
      share: share,
      beat: (n: NodeSnap) => n.ticks_since_beat,
      epoch: (n: NodeSnap) => n.epoch,
    }[sortKey];
    return [...list].sort((a, b) => {
      const ka = key(a);
      const kb = key(b);
      const c = typeof ka === 'string' ? ka.localeCompare(kb as string) : (ka as number) - (kb as number);
      return sortDesc ? -c : c;
    });
  }, [snapshot, sortKey, sortDesc, execFilter, execTotals]);

  if (!snapshot) {
    return (
      <div className="pg-task-wrap">
        <div className="pg-pane-title">
          <span>Tasks</span>
        </div>
        <p className="pg-device-none">Run the graph to see the task table.</p>
      </div>
    );
  }

  const executorNames = [...new Set(snapshot.nodes.map((n) => n.executor ?? 'root'))];
  const clickSort = (k: SortKey) => {
    if (k === sortKey) setSortDesc((d) => !d);
    else {
      setSortKey(k);
      setSortDesc(k !== 'name' && k !== 'executor' && k !== 'status');
    }
  };
  const th = (k: SortKey, label: string, title?: string) => (
    <th
      role="button"
      tabIndex={0}
      aria-sort={sortKey === k ? (sortDesc ? 'descending' : 'ascending') : undefined}
      onClick={() => clickSort(k)}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') clickSort(k);
      }}
      title={title}
    >
      {label}
      {sortKey === k ? (sortDesc ? ' ↓' : ' ↑') : ''}
    </th>
  );

  return (
    <div className="pg-task-wrap">
      <div className="pg-pane-title">
        <span>Tasks</span>
        {executorNames.length > 1 && (
          <select
            className="pg-task-filter"
            value={execFilter}
            onChange={(e) => setExecFilter(e.target.value)}
            aria-label="Filter by executor"
          >
            <option value="">all executors</option>
            {executorNames.map((n) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
          </select>
        )}
      </div>
      {snapshot.executors.length > 0 && (
        <div className="pg-exec-strip">
          {snapshot.executors.map((e) => (
            <span key={e.id} className="pg-exec-row" title="poll passes / task executions / exec wall time (browser)">
              <strong>{e.name}</strong> core {cores[e.name] ?? 0} · {e.passes} passes · {e.polls} polls ·{' '}
              {fmtUs(e.exec_us)}
              {e.current && <em className="pg-exec-current"> ▶ {e.current}</em>}
            </span>
          ))}
        </div>
      )}
      <div className="pg-task-scroll">
        <table className="pg-task-table">
          <thead>
            <tr>
              {th('name', 'task')}
              {th('executor', 'exec')}
              <th title="declared placement: wasm is single-threaded, so this is scenario metadata, not a measured core">
                core
              </th>
              {th('status', 'status', 'report_status()')}
              <th title="mark_busy() / mark_idle()">busy</th>
              {th('polls', 'polls', 'poll count from the trace recorders')}
              {th('last', 'last poll', 'browser wall time, not MCU time')}
              {th('max', 'max poll', 'browser wall time; a hog marker past 50ms')}
              {th('share', 'share', "this node's exec time as a share of its executor's")}
              {th('beat', 'beat', 'ms since the last beat (virtual) — only for nodes with a beat_timeout')}
              {th('epoch', 'epoch', 'bumps on every activation; a restart is visible here')}
              <th>flags</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((n) => {
              const cls = ledClass(n, stale.has(n.name));
              const total = execTotals.get(n.exec_id) ?? 0;
              const share = total > 0 ? n.exec_us / total : 0;
              const hog = n.max_poll_us > HOG_POLL_US;
              // Status, busy and ready describe the running task; a stopped
              // node's last words ("session open") would misread as live.
              const flags = [
                n.running && n.ready && 'ready',
                n.disabled && 'disabled',
                n.collateral && 'held',
                n.bound_stopped && 'bound',
                n.detached && 'detached',
                n.exited && 'exited',
              ].filter(Boolean) as string[];
              return (
                <tr key={n.name} className={`pg-task-${cls}`}>
                  <td className="pg-task-name">
                    <span className={`pg-led pg-led-${cls}`} />
                    {n.name}
                    <span className={`chip chip-${n.mode}`}>{n.mode}</span>
                  </td>
                  <td>{n.executor ?? 'root'}</td>
                  <td>{cores[n.executor ?? 'root'] ?? 0}</td>
                  <td className="pg-task-status">{n.running ? (n.status ?? '') : ''}</td>
                  <td>{n.running && n.busy ? '●' : ''}</td>
                  <td className="num">{n.polls}</td>
                  <td className="num">{fmtUs(n.last_poll_us)}</td>
                  <td className="num">
                    {fmtUs(n.max_poll_us)}
                    {hog && (
                      <span className="pg-hog" title="one poll ran past 50 ms of browser time: this task is starving its executor">
                        hog
                      </span>
                    )}
                  </td>
                  <td className="num">{total > 0 ? `${Math.round(share * 100)}%` : '—'}</td>
                  <td className="num">
                    {n.running && n.policed ? Math.round(n.ticks_since_beat / 1000) : '—'}
                  </td>
                  <td className="num">{n.epoch}</td>
                  <td className="pg-task-flags">{flags.join(' ')}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}
