import { createMemo, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import {
  Calibration,
  GavelCal,
  calibrations,
  saveCalibration,
  sendCommand,
  updateCalibration,
} from '../maintenance';
import { startGamepad } from '../gamepad';
import { AckChip, DeviceBadge, useAck } from './common';

// Matches the firmware's compiled fallback / gavel.toml seed — used only until
// the role's calibration loads.
const DEFAULT_GAVEL: GavelCal = {
  rest: 1500,
  raise: 2000,
  strike: 1100,
  raise_dwell_ms: 180,
  strike_dwell_ms: 120,
  settle_dwell_ms: 160,
};

// Servo-position fields get a live "Jog" button; dwell fields are plain inputs.
const POS_FIELDS: Array<{ key: 'rest' | 'raise' | 'strike'; label: string }> = [
  { key: 'rest', label: 'rest (µs)' },
  { key: 'raise', label: 'raise (µs)' },
  { key: 'strike', label: 'strike (µs)' },
];
const DWELL_FIELDS: Array<{
  key: 'raise_dwell_ms' | 'strike_dwell_ms' | 'settle_dwell_ms';
  label: string;
}> = [
  { key: 'raise_dwell_ms', label: 'raise dwell (ms)' },
  { key: 'strike_dwell_ms', label: 'strike dwell (ms)' },
  { key: 'settle_dwell_ms', label: 'settle dwell (ms)' },
];

export default function GavelPanel() {
  const [ack, run] = useAck();

  // Stored = the gavel role's persisted calibration; `form` holds in-progress
  // edits. `cur` is what the controls show: edits → stored → defaults.
  const storedCal = createMemo<Calibration | null>(() => calibrations()['gavel'] ?? null);
  const [form, setForm] = createSignal<GavelCal | null>(null);
  const cur = createMemo<GavelCal>(() => form() ?? storedCal()?.gavel ?? DEFAULT_GAVEL);
  const [status, setStatus] = createSignal('');
  const [error, setError] = createSignal('');

  function patch(key: keyof GavelCal, value: number) {
    setForm({ ...cur(), [key]: value });
  }

  // Fold the current geometry back into the full Calibration for PUT.
  function asCalibration(): Calibration {
    const base = storedCal();
    return base
      ? { ...base, gavel: cur() }
      : { role: 'gavel', fire_presets_ms: [], gavel: cur() };
  }

  // Strike with the saved geometry (also the gamepad A action).
  function strikeSaved() {
    void run(sendCommand('gavel', { cmd: 'gavel' }));
  }
  // Strike with the current (possibly unsaved) form values.
  function testStrike() {
    void run(sendCommand('gavel', { cmd: 'gavel_strike', ...cur() }));
  }
  function jog(us: number) {
    void run(sendCommand('gavel', { cmd: 'gavel_jog', us }));
  }

  async function persist(toDisk: boolean) {
    setError('');
    setStatus(toDisk ? 'saving…' : 'applying…');
    try {
      await updateCalibration('gavel', asCalibration());
      if (toDisk) await saveCalibration('gavel');
      setForm(null); // re-sync the controls to the now-stored values
      setStatus(toDisk ? 'saved to disk' : 'applied (in-memory)');
    } catch (e) {
      setError(String(e));
      setStatus('');
    }
  }

  onMount(() => {
    // A button (index 0) strikes the gavel with the saved geometry.
    const stop = startGamepad({ onButtonDown: (i) => i === 0 && strikeSaved() });
    onCleanup(stop);
  });

  return (
    <div class="panel-card">
      <header class="panel-card-head">
        <h2>Gavel</h2>
        <DeviceBadge role="gavel" />
      </header>

      <section class="panel-section">
        <h3>Strike <span class="muted small">(gamepad: A)</span></h3>
        <div class="btn-row">
          <button class="gavel-btn" onClick={strikeSaved}>Strike gavel</button>
          <button onClick={() => void run(sendCommand('gavel', { cmd: 'ping' }))}>Ping</button>
          <AckChip ack={ack()} />
        </div>
      </section>

      <section class="panel-section">
        <h3>Strike geometry</h3>
        <p class="muted small">
          Jog a position to eyeball it, Test strike to feel the full rap, then Save.
          Saved values drive real verdict strikes too.
        </p>
        <div class="cal-grid">
          <For each={POS_FIELDS}>
            {(f) => (
              <label class="cal-field">
                <span>{f.label}</span>
                <div class="btn-row">
                  <input
                    type="number"
                    step={10}
                    value={cur()[f.key]}
                    onInput={(e) => patch(f.key, parseInt(e.currentTarget.value, 10))}
                  />
                  <button onClick={() => jog(cur()[f.key])}>Jog</button>
                </div>
              </label>
            )}
          </For>
          <For each={DWELL_FIELDS}>
            {(f) => (
              <label class="cal-field">
                <span>{f.label}</span>
                <input
                  type="number"
                  step={10}
                  value={cur()[f.key]}
                  onInput={(e) => patch(f.key, parseInt(e.currentTarget.value, 10))}
                />
              </label>
            )}
          </For>
        </div>
        <div class="btn-row">
          <button onClick={testStrike}>Test strike</button>
          <button onClick={() => void persist(false)}>Apply</button>
          <button onClick={() => void persist(true)}>Save to disk</button>
        </div>
        <div class="status-line">
          <Show when={status()}><span class="status">{status()}</span></Show>
          <Show when={error()}><span class="err">{error()}</span></Show>
        </div>
      </section>
    </div>
  );
}
