// The playground island: editor | live graph | device rail, over one wasm
// instance running the real supervisor on a virtual clock.
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  freshInstance,
  type ParseOutcome,
  type Snapshot,
  type LogEntry,
  type Wasm,
} from '../../lib/playground/wasm';
import { scenarios, type ButtonAction, type Scenario } from './scenarios';
import { extractDecl } from './dslHighlight';
import EditorPane from './EditorPane';
import GraphPane from './GraphPane';
import DevicePane from './DevicePane';
import LogPane from './LogPane';
import TaskPane from './TaskPane';

type Phase = 'loading' | 'idle' | 'running' | 'faulted' | 'error';

const MAX_LOGS = 400;

export default function PlaygroundApp() {
  const wasmRef = useRef<Wasm | null>(null);
  const rafRef = useRef<number>(0);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const lastFrameRef = useRef<number>(0);
  const speedRef = useRef<number>(1);
  const prevWritesRef = useRef<Map<string, number>>(new Map());

  const [phase, setPhase] = useState<Phase>('loading');
  const [scenario, setScenario] = useState<Scenario>(scenarios[0]);
  const [dsl, setDsl] = useState<string>(scenarios[0].dsl);
  const [parse, setParse] = useState<ParseOutcome | null>(null);
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [faults, setFaults] = useState<string[]>([]);
  const [stale, setStale] = useState<Set<string>>(new Set());
  const [speed, setSpeed] = useState<number>(1);
  const [error, setError] = useState<string | null>(null);
  const [activeSignals, setActiveSignals] = useState<Set<string>>(new Set());
  const [editorHidden, setEditorHidden] = useState(false);
  const [rebootCount, setRebootCount] = useState(0);
  const [rebooted, setRebooted] = useState(false);
  // Bumped on every run and reset: the device widgets hold their own
  // positions, and a re-run seeds the simulation from each device's
  // `initial`, so they must remount or the first click would write the
  // value the simulation already holds.
  const [runGen, setRunGen] = useState(0);

  speedRef.current = speed;

  // Boot: compile + instantiate once, parse the initial scenario.
  useEffect(() => {
    let cancelled = false;
    freshInstance()
      .then((w) => {
        if (cancelled) return;
        wasmRef.current = w;
        setPhase('idle');
        setParse(w.parse_dsl(scenarios[0].dsl) as ParseOutcome);
      })
      .catch((e) => {
        setError(String(e));
        setPhase('error');
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Debounced re-parse on edit (parse_dsl is pure; safe while running).
  useEffect(() => {
    const w = wasmRef.current;
    if (!w) return;
    const t = setTimeout(() => {
      try {
        setParse(w.parse_dsl(dsl) as ParseOutcome);
      } catch (e) {
        setError(String(e));
      }
    }, 300);
    return () => clearTimeout(t);
  }, [dsl, phase]);

  const stopLoop = useCallback(() => {
    cancelAnimationFrame(rafRef.current);
    rafRef.current = 0;
    if (intervalRef.current !== null) {
      clearInterval(intervalRef.current);
      intervalRef.current = null;
    }
  }, []);

  const drainFrame = useCallback(() => {
    const w = wasmRef.current;
    if (!w) return;
    const snap = w.drain_events() as Snapshot;
    // Signal activity: a write-counter delta this frame animates the edge.
    const active = new Set<string>();
    const prev = prevWritesRef.current;
    for (const s of snap.signals) {
      if ((prev.get(s.name) ?? 0) !== s.writes) active.add(s.name);
      prev.set(s.name, s.writes);
    }
    setActiveSignals((old) => {
      if (old.size === active.size && [...active].every((n) => old.has(n))) return old;
      return active;
    });
    if (snap.logs.length) {
      setLogs((old) => {
        const next = old.concat(snap.logs);
        return next.length > MAX_LOGS ? next.slice(next.length - MAX_LOGS) : next;
      });
    }
    if (snap.faults.length) {
      setFaults((old) => old.concat(snap.faults));
      setPhase('faulted');
    }
    if (snap.watchdog_bite) {
      setRebootCount((n) => n + 1);
    }
    if (snap.health.length) {
      setStale((old) => {
        const next = new Set(old);
        for (const h of snap.health) {
          if (h.kind.startsWith('stale')) next.add(h.node);
          else next.delete(h.node);
        }
        return next;
      });
    }
    snapshotRef.current = snap;
    setSnapshot(snap);
  }, []);

  const advance = useCallback(
    (t: number) => {
      const w = wasmRef.current;
      if (!w) return;
      const dt = Math.min(t - (lastFrameRef.current || t), 100);
      lastFrameRef.current = t;
      if (speedRef.current > 0 && dt > 0) {
        w.tick(dt * 1000 * speedRef.current);
      }
      drainFrame();
    },
    [drainFrame],
  );

  const loop = useCallback(
    (t: number) => {
      advance(t);
      rafRef.current = requestAnimationFrame(loop);
    },
    [advance],
  );

  const startLoop = useCallback(() => {
    lastFrameRef.current = 0;
    rafRef.current = requestAnimationFrame(loop);
    // rAF is suspended in occluded/background windows; this keeps virtual
    // time moving (coarsely) there instead of silently freezing the run.
    intervalRef.current = setInterval(() => {
      const now = performance.now();
      if (now - lastFrameRef.current > 220) advance(now);
    }, 250);
  }, [loop, advance]);

  const reset = useCallback(async () => {
    stopLoop();
    setRunGen((g) => g + 1);
    setPhase('loading');
    setSnapshot(null);
    setLogs([]);
    setFaults([]);
    setStale(new Set());
    prevWritesRef.current = new Map();
    try {
      wasmRef.current = await freshInstance();
      setPhase('idle');
      setParse(wasmRef.current.parse_dsl(dsl) as ParseOutcome);
    } catch (e) {
      setError(String(e));
      setPhase('error');
    }
  }, [dsl, stopLoop]);

  const run = useCallback(async () => {
    stopLoop();
    setRebooted(false);
    setRunGen((g) => g + 1);
    // Statics are never reset in place: every run gets a fresh instance.
    setSnapshot(null);
    setLogs([]);
    setFaults([]);
    setStale(new Set());
    prevWritesRef.current = new Map();
    try {
      const w = await freshInstance();
      wasmRef.current = w;
      const outcome = w.start_run(dsl, JSON.stringify(scenario.behaviors)) as ParseOutcome;
      setParse(outcome);
      if (!outcome.ok) {
        setPhase('idle');
        return;
      }
      // Seed device initial values.
      for (const d of scenario.devices) {
        if (d.kind !== 'lease' && d.initial !== undefined) {
          try {
            w.set_input(d.target, d.initial);
          } catch {
            // A device may target a node the user edited away; ignore.
          }
        }
      }
      setPhase('running');
      startLoop();
    } catch (e) {
      setError(String(e));
      setPhase('error');
    }
  }, [dsl, scenario, startLoop, stopLoop]);

  useEffect(() => stopLoop, [stopLoop]);

  // A hardware watchdog bite reboots the MCU: restart the whole instance,
  // as the real chip would, and say so at the top of the fresh boot log.
  const runRef = useRef(run);
  runRef.current = run;
  useEffect(() => {
    if (rebootCount === 0) return;
    let cancelled = false;
    void (async () => {
      await runRef.current();
      if (cancelled) return;
      setRebooted(true);
      setLogs((l) => [
        { ts_us: 0, level: 'ERROR', target: 'hw', msg: 'hardware watchdog bit: MCU reset' },
        ...l,
      ]);
    })();
    return () => {
      cancelled = true;
    };
  }, [rebootCount]);

  const pickScenario = useCallback(
    (id: string) => {
      const s = scenarios.find((x) => x.id === id);
      if (!s) return;
      setScenario(s);
      setDsl(s.dsl);
      void reset();
    },
    [reset],
  );

  const onControl = useCallback((idx: number, op: string) => {
    try {
      wasmRef.current?.control(idx, op);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const onInject = useCallback((idx: number, kind: string) => {
    try {
      wasmRef.current?.inject(idx, kind);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const onNodeCommand = useCallback((idx: number, op: string) => {
    try {
      wasmRef.current?.node_command(idx, op);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const onResourceCommand = useCallback((name: string, cmd: 'provide' | 'clear') => {
    try {
      wasmRef.current?.resource_command(name, cmd);
    } catch {
      // Resource edited out of the running graph; harmless.
    }
  }, []);

  const snapshotRef = useRef<Snapshot | null>(null);
  const onAction = useCallback((a: ButtonAction) => {
    const w = wasmRef.current;
    if (!w) return;
    try {
      if (a.type === 'power') w.power(a.cmd);
      else if (a.type === 'resource') w.resource_command(a.resource, a.cmd);
      else if (a.type === 'node') {
        const idx = snapshotRef.current?.nodes.find((n) => n.name === a.node)?.idx ?? -1;
        if (idx >= 0) w.node_command(idx, a.op);
      }
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const onInput = useCallback((target: string, value: number) => {
    try {
      wasmRef.current?.set_input(target, value);
    } catch {
      // Target edited out of the running graph; harmless.
    }
  }, []);

  const onSignalCommand = useCallback(
    (signalName: string, cmd: 'drain' | 'reopen') => {
      const idx = snapshot?.signals.findIndex((s) => s.name === signalName) ?? -1;
      if (idx >= 0) wasmRef.current?.signal_command(idx, cmd);
    },
    [snapshot],
  );

  const step = useCallback(() => {
    const w = wasmRef.current;
    if (!w || phase === 'idle') return;
    w.tick(100_000);
    drainFrame();
  }, [phase, drainFrame]);

  // Vertical resize of the editor/graph row: direct style writes during the
  // drag (no re-render churn); the graph's own ResizeObserver refits it.
  const mainRef = useRef<HTMLDivElement>(null);
  const setMainHeight = useCallback((h: number) => {
    const main = mainRef.current;
    if (!main) return;
    const clamped = Math.min(Math.max(h, 240), 1600);
    main.style.height = `${clamped}px`;
    main.style.minHeight = '0';
    main.style.flex = 'none';
  }, []);
  const resetMainHeight = useCallback(() => {
    const main = mainRef.current;
    if (!main) return;
    main.style.height = '';
    main.style.minHeight = '';
    main.style.flex = '';
  }, []);
  const onResizeStart = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      const main = mainRef.current;
      if (!main) return;
      e.preventDefault();
      const handle = e.currentTarget;
      try {
        handle.setPointerCapture(e.pointerId);
      } catch {
        // A cancelled or synthetic pointer cannot be captured; the drag
        // still works while the cursor stays over the handle.
      }
      const startY = e.clientY;
      const startH = main.getBoundingClientRect().height;
      const move = (ev: PointerEvent) => setMainHeight(startH + ev.clientY - startY);
      const up = (ev: PointerEvent) => {
        try {
          handle.releasePointerCapture(ev.pointerId);
        } catch {
          // Never captured; nothing to release.
        }
        handle.removeEventListener('pointermove', move);
        handle.removeEventListener('pointerup', up);
        handle.removeEventListener('pointercancel', up);
      };
      handle.addEventListener('pointermove', move);
      handle.addEventListener('pointerup', up);
      handle.addEventListener('pointercancel', up);
    },
    [setMainHeight],
  );
  const onResizeKey = useCallback(
    (e: React.KeyboardEvent<HTMLDivElement>) => {
      if (e.key !== 'ArrowUp' && e.key !== 'ArrowDown') return;
      const main = mainRef.current;
      if (!main) return;
      e.preventDefault();
      const delta = e.key === 'ArrowUp' ? -24 : 24;
      setMainHeight(main.getBoundingClientRect().height + delta);
    },
    [setMainHeight],
  );

  // Same resize affordance for the logs/tasks row.
  const bottomRef = useRef<HTMLDivElement>(null);
  const setBottomHeight = (h: number) => {
    const el = bottomRef.current;
    if (!el) return;
    el.style.height = `${Math.min(Math.max(h, 160), 1200)}px`;
  };
  const resetBottomHeight = () => {
    const el = bottomRef.current;
    if (el) el.style.height = '';
  };
  const onBottomResizeStart = (e: React.PointerEvent<HTMLDivElement>) => {
    const el = bottomRef.current;
    if (!el) return;
    e.preventDefault();
    const handle = e.currentTarget;
    try {
      handle.setPointerCapture(e.pointerId);
    } catch {
      // A cancelled or synthetic pointer cannot be captured.
    }
    const startY = e.clientY;
    const startH = el.getBoundingClientRect().height;
    const move = (ev: PointerEvent) => setBottomHeight(startH + ev.clientY - startY);
    const up = (ev: PointerEvent) => {
      try {
        handle.releasePointerCapture(ev.pointerId);
      } catch {
        // Never captured; nothing to release.
      }
      handle.removeEventListener('pointermove', move);
      handle.removeEventListener('pointerup', up);
      handle.removeEventListener('pointercancel', up);
    };
    handle.addEventListener('pointermove', move);
    handle.addEventListener('pointerup', up);
    handle.addEventListener('pointercancel', up);
  };
  const onBottomResizeKey = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== 'ArrowUp' && e.key !== 'ArrowDown') return;
    const el = bottomRef.current;
    if (!el) return;
    e.preventDefault();
    setBottomHeight(el.getBoundingClientRect().height + (e.key === 'ArrowUp' ? -24 : 24));
  };

  const declOf = useCallback((name: string) => extractDecl(dsl, name), [dsl]);

  const clockMs = snapshot ? Math.round(snapshot.now_us / 1000) : 0;
  const running = phase === 'running' || phase === 'faulted';

  const runLabel = useMemo(() => {
    if (phase === 'loading') return 'Loading…';
    if (!running) return 'Run';
    return 'Restart';
  }, [phase, running]);

  if (phase === 'error') {
    return (
      <div className="pg-error" role="alert">
        <strong>The playground failed to load.</strong>
        <p>{error}</p>
        <p>A browser with WebAssembly support is required.</p>
      </div>
    );
  }

  return (
    <div className="pg-app">
      <header className="pg-toolbar">
        <label className="pg-scenario">
          <span className="pg-toolbar-label">Scenario</span>
          <select value={scenario.id} onChange={(e) => pickScenario(e.target.value)}>
            <optgroup label="Systems">
              {scenarios
                .filter((s) => s.group === 'systems')
                .map((s) => (
                  <option key={s.id} value={s.id}>
                    {s.title}
                  </option>
                ))}
            </optgroup>
            <optgroup label="Mechanisms">
              {scenarios
                .filter((s) => s.group === 'mechanisms')
                .map((s) => (
                  <option key={s.id} value={s.id}>
                    {s.title}
                  </option>
                ))}
            </optgroup>
          </select>
        </label>
        <button
          className="pg-run"
          onClick={() => void run()}
          disabled={phase === 'loading' || (parse !== null && !parse.ok)}
          title={parse && !parse.ok ? 'Fix the parse errors first' : 'Build and start the graph'}
        >
          {runLabel}
        </button>
        <button className="pg-ghost" onClick={() => void reset()} disabled={!running}>
          Reset
        </button>
        <button
          className="pg-ghost pg-editor-toggle"
          onClick={() => setEditorHidden((h) => !h)}
          aria-pressed={editorHidden}
          title={editorHidden ? 'Show the code editor' : 'Hide the code editor (full-width graph)'}
        >
          {editorHidden ? '◨ Show code' : '◧ Hide code'}
        </button>
        <div className="pg-time" role="group" aria-label="Virtual time">
          <button className={speed === 0 ? 'on' : ''} onClick={() => setSpeed(0)} title="Pause virtual time">
            ⏸
          </button>
          <button className={speed === 1 ? 'on' : ''} onClick={() => setSpeed(1)} title="Real-time">
            1×
          </button>
          <button className={speed === 10 ? 'on' : ''} onClick={() => setSpeed(10)} title="Fast-forward">
            10×
          </button>
          <button onClick={step} disabled={!running || speed !== 0} title="Step 100 ms">
            +100ms
          </button>
          <span className="pg-clock" title="Virtual clock (mock time driver)">
            t = {(clockMs / 1000).toFixed(2)}s
          </span>
        </div>
        {phase === 'faulted' && <span className="pg-fault-banner">supervisor faulted — Restart to run again</span>}
        {rebooted && phase !== 'faulted' && (
          <span className="pg-fault-banner" title="the watchdog feeder stopped; the hardware watchdog reset the system">
            ↻ hardware watchdog rebooted the MCU
          </span>
        )}
      </header>

      <p className="pg-blurb">
        {scenario.blurb}
        {scenario.mechanisms && (
          <span className="pg-mechanisms"> Mechanisms shown: {scenario.mechanisms}.</span>
        )}
      </p>

      <div className={`pg-main ${editorHidden ? 'pg-main-full' : ''}`} ref={mainRef}>
        {/* Kept mounted while hidden so the editor's undo history survives. */}
        <section className="pg-pane pg-editor" aria-label="Graph declaration" hidden={editorHidden}>
          <EditorPane value={dsl} onChange={setDsl} parse={parse} />
        </section>
        <section className="pg-pane pg-graph" aria-label="Live graph">
          <GraphPane
            model={parse?.model ?? null}
            snapshot={running ? snapshot : null}
            stale={stale}
            activeSignals={activeSignals}
            planes={scenario.planes}
            onControl={onControl}
            onInject={onInject}
            onNodeCommand={onNodeCommand}
            onResourceCommand={onResourceCommand}
            declOf={declOf}
          />
        </section>
      </div>

      <div
        className="pg-resizer"
        role="separator"
        aria-orientation="horizontal"
        aria-label="Resize the graph area"
        tabIndex={0}
        title="Drag to resize the graph; double-click to reset"
        onPointerDown={onResizeStart}
        onKeyDown={onResizeKey}
        onDoubleClick={resetMainHeight}
      />

      <section className="pg-pane pg-devices" aria-label="Virtual device">
        <DevicePane
          key={runGen}
          devices={scenario.devices}
          snapshot={running ? snapshot : null}
          faults={faults}
          running={running}
          onInput={onInput}
          onSignalCommand={onSignalCommand}
          onAction={onAction}
          onResourceCommand={onResourceCommand}
        />
      </section>

      <div
        className="pg-resizer"
        role="separator"
        aria-orientation="horizontal"
        aria-label="Resize the logs and tasks row"
        tabIndex={0}
        title="Drag to resize the logs and tasks; double-click to reset"
        onPointerDown={onBottomResizeStart}
        onKeyDown={onBottomResizeKey}
        onDoubleClick={resetBottomHeight}
      />

      <div className="pg-bottom" ref={bottomRef}>
        <section className="pg-pane pg-logs" aria-label="Application logs">
          <LogPane logs={logs} running={running} />
        </section>
        <section className="pg-pane pg-tasks" aria-label="Task table">
          <TaskPane snapshot={running ? snapshot : null} stale={stale} cores={scenario.cores ?? {}} />
        </section>
      </div>
    </div>
  );
}
