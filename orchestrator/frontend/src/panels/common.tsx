import { createMemo, createSignal, For, Show } from 'solid-js';
import {
  AckResult,
  Calibration,
  Role,
  ServoCal,
  calibrations,
  deviceConnected,
  saveCalibration,
  updateCalibration,
} from '../maintenance';

/** Inline OK/ERR/timeout chip for the last result of a tracked action. */
export function AckChip(props: { ack: AckResult | null }) {
  return (
    <Show when={props.ack}>
      {(a) => (
        <span class={`ack-chip ack-${a().result}`}>
          {a().result === 'ok' && (a() as { line: string }).line}
          {a().result === 'err' && `ERR: ${(a() as { reason: string }).reason}`}
          {a().result === 'timeout' && 'timeout'}
          {a().result === 'no_device' && 'no device'}
        </span>
      )}
    </Show>
  );
}

/** Connection badge for a device role. */
export function DeviceBadge(props: { role: Role }) {
  const up = createMemo(() => deviceConnected(props.role));
  return (
    <span class={`device-badge ${up() ? 'up' : 'down'}`}>
      <span class="dot" /> {props.role} {up() ? 'connected' : 'not connected'}
    </span>
  );
}

/** Small hook returning [ack, run]: run(promise) stores the resolved ack. */
export function useAck(): [() => AckResult | null, (p: Promise<AckResult | null>) => Promise<void>] {
  const [ack, setAck] = createSignal<AckResult | null>(null);
  const run = async (p: Promise<AckResult | null>) => {
    try {
      setAck(await p);
    } catch (e) {
      setAck({ result: 'err', reason: String(e) });
    }
  };
  return [ack, run];
}

const AXIS_FIELDS: Array<{ key: keyof ServoCal; label: string; step: number }> = [
  { key: 'min', label: 'min (raw)', step: 1 },
  { key: 'max', label: 'max (raw)', step: 1 },
  { key: 'center', label: 'center (raw)', step: 1 },
  { key: 'limit_min_deg', label: 'limit min°', step: 1 },
  { key: 'limit_max_deg', label: 'limit max°', step: 1 },
];

/** Editor for a role's calibration (Apply in-memory, Save to disk). */
export function CalibrationEditor(props: { role: Role }) {
  const stored = createMemo(() => calibrations()[props.role]);
  const [form, setForm] = createSignal<Calibration | null>(null);
  const [status, setStatus] = createSignal('');
  const [error, setError] = createSignal('');

  // Seed the form from the stored calibration when it first loads.
  const current = createMemo(() => form() ?? stored() ?? null);

  function edit(mut: (c: Calibration) => void) {
    const base = structuredClone(current());
    if (!base) return;
    mut(base);
    setForm(base);
  }

  function patchAxis(axis: 'pan' | 'tilt', key: keyof ServoCal, value: number | boolean) {
    edit((c) => {
      const a = c[axis];
      if (a) (a[key] as number | boolean) = value;
    });
  }

  async function doApply() {
    setError('');
    setStatus('applying…');
    const c = current();
    if (!c) return;
    try {
      await updateCalibration(props.role, c);
      setStatus('applied (in-memory)');
    } catch (e) {
      setError(String(e));
      setStatus('');
    }
  }

  async function doSave() {
    setError('');
    setStatus('saving…');
    const c = current();
    if (!c) return;
    try {
      await updateCalibration(props.role, c);
      await saveCalibration(props.role);
      setStatus('saved to disk');
    } catch (e) {
      setError(String(e));
      setStatus('');
    }
  }

  const axisEditor = (axis: 'pan' | 'tilt') => {
    const a = () => current()?.[axis] ?? null;
    return (
      <Show when={a()}>
        {(servo) => (
          <div class="cal-axis">
            <div class="cal-axis-name">{axis}</div>
            <div class="cal-grid">
              <For each={AXIS_FIELDS}>
                {(f) => (
                  <label class="cal-field">
                    <span>{f.label}</span>
                    <input
                      type="number"
                      step={f.step}
                      value={servo()[f.key] as number}
                      onInput={(e) => patchAxis(axis, f.key, parseFloat(e.currentTarget.value))}
                    />
                  </label>
                )}
              </For>
              <label class="cal-field cal-check">
                <input
                  type="checkbox"
                  checked={servo().invert}
                  onChange={(e) => patchAxis(axis, 'invert', e.currentTarget.checked)}
                />
                <span>invert</span>
              </label>
            </div>
          </div>
        )}
      </Show>
    );
  };

  return (
    <div class="cal-editor">
      <h3>Calibration</h3>
      <Show when={current()} fallback={<div class="muted small">no calibration loaded</div>}>
        {axisEditor('pan')}
        {axisEditor('tilt')}
        <Show when={(current()?.fire_presets_ms?.length ?? 0) > 0}>
          <label class="cal-field">
            <span>fire presets (ms, comma-separated)</span>
            <input
              type="text"
              value={(current()?.fire_presets_ms ?? []).join(', ')}
              onInput={(e) =>
                edit((c) => {
                  c.fire_presets_ms = e.currentTarget.value
                    .split(',')
                    .map((s) => parseInt(s.trim(), 10))
                    .filter((n) => Number.isFinite(n) && n > 0);
                })
              }
            />
          </label>
        </Show>
        <div class="btn-row">
          <button onClick={doApply}>Apply</button>
          <button onClick={doSave}>Save to disk</button>
        </div>
        <div class="status-line">
          <Show when={status()}><span class="status">{status()}</span></Show>
          <Show when={error()}><span class="err">{error()}</span></Show>
        </div>
      </Show>
    </div>
  );
}

/** Map a normalised stick value [-1,1] to a degree within an axis' limits. */
export function stickToDeg(v: number, servo: ServoCal | null | undefined): number {
  if (!servo) return 0;
  // Positive stick → toward limit_max_deg, negative → limit_min_deg, 0 → 0°.
  return v >= 0 ? v * servo.limit_max_deg : -v * servo.limit_min_deg;
}
