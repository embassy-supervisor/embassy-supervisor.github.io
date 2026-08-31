// The application log console: virtual-time-stamped records from the
// supervisor's `log` backend, tail-following unless the reader scrolls up.
import { useEffect, useRef } from 'react';
import type { LogEntry } from '../../lib/playground/wasm';

interface Props {
  logs: LogEntry[];
  running: boolean;
}

export default function LogPane({ logs, running }: Props) {
  const hostRef = useRef<HTMLDivElement>(null);
  const followRef = useRef(true);

  useEffect(() => {
    const el = hostRef.current;
    if (el && followRef.current) el.scrollTop = el.scrollHeight;
  }, [logs]);

  return (
    <div className="pg-log-wrap">
      <div className="pg-pane-title">
        <span>Logs</span>
        <span className="pg-log-count">{logs.length ? `${logs.length} records` : running ? 'waiting…' : 'not running'}</span>
      </div>
      <div
        className="pg-log-host"
        ref={hostRef}
        onScroll={() => {
          const el = hostRef.current;
          if (el) followRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 8;
        }}
      >
        {logs.map((l, i) => (
          <div key={i} className={`pg-log-line pg-log-${l.level.toLowerCase()}`}>
            <span className="pg-log-ts">{(l.ts_us / 1_000_000).toFixed(3)}</span>
            <span className="pg-log-level">{l.level}</span>
            <span className="pg-log-msg">{l.msg}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
