// The virtual device rail: the scenario's physical inputs (sensor sliders,
// link switches, load dials, momentary buttons, lease rollover, gauges)
// plus the resource-slot and fault readouts.
import { useEffect, useRef, useState } from 'react';
import type { Snapshot } from '../../lib/playground/wasm';
import type { ButtonAction, DeviceSpec } from './scenarios';

interface Props {
  devices: DeviceSpec[];
  snapshot: Snapshot | null;
  faults: string[];
  running: boolean;
  onInput: (target: string, value: number) => void;
  onSignalCommand: (signal: string, cmd: 'drain' | 'reopen') => void;
  onAction: (action: ButtonAction) => void;
  onResourceCommand: (name: string, cmd: 'provide' | 'clear') => void;
}

function Slider({ d, running, onInput }: { d: DeviceSpec; running: boolean; onInput: Props['onInput'] }) {
  const min = d.min ?? 0;
  const max = d.max ?? 1;
  const [v, setV] = useState(d.initial ?? (min + max) / 2);
  return (
    <label className="pg-device">
      <span className="pg-device-label">
        {d.label}
        <output>
          {v.toFixed(max - min > 4 ? 0 : 2)}
          {d.unit ?? ''}
        </output>
      </span>
      <input
        type="range"
        min={min}
        max={max}
        step={(max - min) / 100}
        value={v}
        disabled={!running}
        onChange={(e) => {
          const nv = Number(e.target.value);
          setV(nv);
          onInput(d.target, nv);
        }}
      />
      {d.hint && <small>{d.hint}</small>}
    </label>
  );
}

/** A stepped rotary dial: discrete detents over the range, coarse by design
 * (a device count, a client count), where the slider is continuous. */
function Dial({ d, running, onInput }: { d: DeviceSpec; running: boolean; onInput: Props['onInput'] }) {
  const min = d.min ?? 0;
  const max = d.max ?? 1;
  // A dial over a small integral range is a count (devices, clients): its
  // detents are whole units and its readout an integer — 3 clients, never
  // 3.00 or a 0.375 step between them.
  const integral = Number.isInteger(min) && Number.isInteger(max) && max - min <= 8;
  const step = integral ? 1 : (max - min) / 8;
  const [v, setV] = useState(d.initial ?? min);
  // Clicks landing inside one React batch all see the same rendered `v`;
  // stepping from the last value written keeps three fast clicks three
  // detents, and keeps the simulation in step with the readout.
  const last = useRef(v);
  const set = (nv: number) => {
    const clamped = Math.min(max, Math.max(min, nv));
    last.current = clamped;
    setV(clamped);
    onInput(d.target, clamped);
  };
  const step_ = (dir: 1 | -1) => set(last.current + dir * step);
  const frac = (v - min) / (max - min || 1);
  const angle = -120 + frac * 240;
  return (
    <div className="pg-device pg-dial-device">
      <span className="pg-device-label">
        {d.label}
        <output>
          {integral || max - min > 4 ? Math.round(v) : v.toFixed(2)}
          {d.unit ?? ''}
        </output>
      </span>
      <div className="pg-dial-row">
        <button className="pg-dial-step" disabled={!running || v <= min} onClick={() => step_(-1)} aria-label={`${d.label} down`}>
          −
        </button>
        <div
          className="pg-dial"
          role="slider"
          aria-valuemin={min}
          aria-valuemax={max}
          aria-valuenow={v}
          aria-label={d.label}
          tabIndex={0}
          onKeyDown={(e) => {
            if (e.key === 'ArrowUp' || e.key === 'ArrowRight') step_(1);
            if (e.key === 'ArrowDown' || e.key === 'ArrowLeft') step_(-1);
          }}
          onWheel={(e) => {
            if (!running) return;
            step_(e.deltaY < 0 ? 1 : -1);
          }}
        >
          <span className="pg-dial-needle" style={{ transform: `rotate(${angle}deg)` }} />
        </div>
        <button className="pg-dial-step" disabled={!running || v >= max} onClick={() => step_(1)} aria-label={`${d.label} up`}>
          +
        </button>
      </div>
      {d.hint && <small>{d.hint}</small>}
    </div>
  );
}

function Switch({ d, running, onInput }: { d: DeviceSpec; running: boolean; onInput: Props['onInput'] }) {
  const [on, setOn] = useState((d.initial ?? 1) >= 0.5);
  const last = useRef(on);
  return (
    <div className="pg-device">
      <span className="pg-device-label">{d.label}</span>
      <button
        role="switch"
        aria-checked={on}
        className={`pg-switch ${on ? 'on' : ''}`}
        disabled={!running}
        onClick={() => {
          const next = !last.current;
          last.current = next;
          setOn(next);
          onInput(d.target, next ? 1 : 0);
        }}
      >
        <span className="pg-switch-knob" />
        <span className="pg-switch-text">{on ? (d.onLabel ?? 'up') : (d.offLabel ?? 'down')}</span>
      </button>
      {d.hint && <small>{d.hint}</small>}
    </div>
  );
}

/** Momentary action: fires once per press. A `pulse` action raises the input
 * for one beat, then returns it to rest. */
function ActionButton({
  d,
  running,
  onAction,
  onInput,
}: {
  d: DeviceSpec;
  running: boolean;
  onAction: Props['onAction'];
  onInput: Props['onInput'];
}) {
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    [],
  );
  const fire = () => {
    const a = d.action;
    if (!a) return;
    if (a.type === 'pulse') {
      onInput(a.node, a.value);
      timer.current = setTimeout(() => onInput(a.node, 0), 400);
    } else {
      onAction(a);
    }
  };
  return (
    <div className="pg-device">
      <span className="pg-device-label">{d.label}</span>
      <button className="pg-button" disabled={!running} onClick={fire}>
        {d.label}
      </button>
      {d.hint && <small>{d.hint}</small>}
    </div>
  );
}

/** Read-only readout bound to a signal's value or queue depth, a node's
 *  budget grant, or a budget's granted total / capacity. */
function Gauge({ d, snapshot }: { d: DeviceSpec; snapshot: Snapshot | null }) {
  const source = d.source ?? 'value';
  let raw = 0;
  if (source === 'grant') {
    raw = snapshot?.nodes.find((n) => n.name === d.target)?.grant ?? 0;
  } else if (source === 'granted' || source === 'capacity') {
    const res = snapshot?.resources.find((r) => r.name === d.target);
    raw = (source === 'granted' ? res?.granted : res?.capacity) ?? 0;
  } else {
    const sig = snapshot?.signals.find((s) => s.name === d.target);
    raw = source === 'depth' ? (sig?.depth ?? 0) : (sig?.value ?? 0);
  }
  const integer = source !== 'value';
  const min = d.min ?? 0;
  const max = d.max ?? (source === 'depth' ? 16 : 1);
  const frac = Math.min(1, Math.max(0, (raw - min) / (max - min || 1)));
  const shown = integer || max - min > 4 ? Math.round(raw) : raw.toFixed(2);
  return (
    <div className="pg-device pg-gauge-device">
      <span className="pg-device-label">
        {d.label}
        <output>
          {shown}
          {d.unit ?? ''}
        </output>
      </span>
      <div
        className="pg-gauge"
        role="meter"
        aria-valuemin={min}
        aria-valuemax={max}
        aria-valuenow={typeof shown === 'string' ? raw : shown}
        aria-label={d.label}
      >
        <span className="pg-gauge-fill" style={{ width: `${frac * 100}%` }} data-hot={frac > 0.8 || undefined} />
      </div>
      {d.hint && <small>{d.hint}</small>}
    </div>
  );
}

function Lease({
  d,
  snapshot,
  running,
  onSignalCommand,
}: {
  d: DeviceSpec;
  snapshot: Snapshot | null;
  running: boolean;
  onSignalCommand: Props['onSignalCommand'];
}) {
  const sig = snapshot?.signals.find((s) => s.name === d.target);
  const drained = sig?.drained ?? false;
  return (
    <div className="pg-device">
      <span className="pg-device-label">
        {d.label}
        {sig && <output>⛓ {sig.leases ?? 0}</output>}
      </span>
      <div className="pg-lease-buttons">
        <button disabled={!running || drained} onClick={() => onSignalCommand(d.target, 'drain')}>
          drain
        </button>
        <button disabled={!running || !drained} onClick={() => onSignalCommand(d.target, 'reopen')}>
          reopen
        </button>
      </div>
      {d.hint && <small>{d.hint}</small>}
    </div>
  );
}

export default function DevicePane({
  devices,
  snapshot,
  faults,
  running,
  onInput,
  onSignalCommand,
  onAction,
  onResourceCommand,
}: Props) {
  return (
    <div className="pg-device-wrap">
      <div className="pg-pane-title">
        <span>Virtual device</span>
      </div>
      <div className="pg-device-grid">
        {devices.length === 0 && (
          <p className="pg-device-none">This scenario has no physical inputs; drive it from the graph.</p>
        )}
        {devices.map((d) => {
          switch (d.kind) {
            case 'switch':
              return <Switch key={d.target + d.kind + d.label} d={d} running={running} onInput={onInput} />;
            case 'dial':
              return <Dial key={d.target + d.kind + d.label} d={d} running={running} onInput={onInput} />;
            case 'button':
              return (
                <ActionButton key={d.target + d.kind + d.label} d={d} running={running} onAction={onAction} onInput={onInput} />
              );
            case 'gauge':
              return <Gauge key={d.target + d.kind + d.label} d={d} snapshot={snapshot} />;
            case 'lease':
              return (
                <Lease key={d.target + d.kind + d.label} d={d} snapshot={snapshot} running={running} onSignalCommand={onSignalCommand} />
              );
            default:
              return <Slider key={d.target + d.kind + d.label} d={d} running={running} onInput={onInput} />;
          }
        })}

        {snapshot && snapshot.resources.length > 0 && (
          <div className="pg-readout">
            <span className="pg-device-label">Resource slots</span>
            <ul>
              {snapshot.resources.map((r) => (
                <li key={r.name}>
                  <span className={`pg-res-dot ${r.filled ? 'filled' : ''}`} /> {r.name}
                  <span className="pg-res-kind">{r.kind}</span>
                  <em>
                    {r.held_by
                      ? `lent to ${r.held_by}`
                      : r.capacity != null && r.filled
                        ? `${r.granted} of ${r.capacity} granted · ${r.claimants} claiming`
                        : r.filled
                          ? 'provided'
                          : 'empty'}
                  </em>
                  {!r.filled && !r.held_by && (
                    <button
                      className="pg-res-provide"
                      disabled={!running}
                      onClick={() => onResourceCommand(r.name, 'provide')}
                      title="re-provide this slot by hand (the rebuild step a consume teardown demands)"
                    >
                      provide
                    </button>
                  )}
                </li>
              ))}
            </ul>
          </div>
        )}

        {faults.length > 0 && (
          <div className="pg-readout pg-faults">
            <span className="pg-device-label">Faults</span>
            <ul>
              {faults.map((f, i) => (
                <li key={i}>{f}</li>
              ))}
            </ul>
          </div>
        )}
      </div>
    </div>
  );
}
