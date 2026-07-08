import { createMemo, createSignal, onCleanup, onMount, Show } from 'solid-js';
import {
  Calibration,
  calibrations,
  saveCalibration,
  sendCommand,
  updateCalibration,
} from '../maintenance';
import { startGamepad } from '../gamepad';
import { AckChip, CalibrationEditor, DeviceBadge, useAck } from './common';
import AimControl from './AimControl';

// Fallback until the squirt role's calibration loads (matches squirt.toml seed).
const DEFAULT_FIRE_MS = 150;
// The firmware hard-caps the relay pulse; keep the console honest about it.
const FIRE_MS_MAX = 1000;

export default function TurretPanel() {
  // Aim is the `turret` board; firing is a separate `squirt` board (the NanoC6
  // has no spare GPIO for the relay alongside the servo-board I2C bus).
  const [fireAck, runFire] = useAck();

  // Stored = the squirt role's persisted calibration; `form` holds the in-progress
  // fire_ms edit. `cur` is what the input shows: edit → stored → default.
  const storedCal = createMemo<Calibration | null>(() => calibrations()['squirt'] ?? null);
  const [form, setForm] = createSignal<number | null>(null);
  const cur = createMemo<number>(() => form() ?? storedCal()?.fire_ms ?? DEFAULT_FIRE_MS);
  const [status, setStatus] = createSignal('');
  const [error, setError] = createSignal('');

  // Fold the current fire_ms back into the full squirt Calibration for PUT.
  function asCalibration(): Calibration {
    const base = storedCal();
    return base ? { ...base, fire_ms: cur() } : { role: 'squirt', fire_ms: cur() };
  }

  // Fire the relay for the current (possibly unsaved) duration.
  function fire() {
    void runFire(sendCommand('squirt', { cmd: 'fire', ms: cur() }));
  }

  async function persist(toDisk: boolean) {
    setError('');
    setStatus(toDisk ? 'saving…' : 'applying…');
    try {
      await updateCalibration('squirt', asCalibration());
      if (toDisk) await saveCalibration('squirt');
      setForm(null); // re-sync the input to the now-stored value
      setStatus(toDisk ? 'saved to disk' : 'applied (in-memory)');
    } catch (e) {
      setError(String(e));
      setStatus('');
    }
  }

  onMount(() => {
    // Right trigger (index 7) fires the current set duration; A (0) is unused here.
    const stop = startGamepad({
      onButtonDown: (i) => {
        if (i === 7) fire();
      },
    });
    onCleanup(stop);
  });

  return (
    <div class="panel-card">
      <header class="panel-card-head">
        <h2>Squirt-gun turret</h2>
        <DeviceBadge role="turret" />
        <DeviceBadge role="squirt" />
      </header>

      <section class="panel-section">
        <h3>Aim</h3>
        <AimControl role="turret" />
      </section>

      <section class="panel-section">
        <h3>Fire <span class="muted small">(RT fires the set duration)</span></h3>
        <p class="muted small">
          Set the relay-open time, Fire to test it, then Apply/Save. The firmware
          clamps to {FIRE_MS_MAX} ms.
        </p>
        <div class="btn-row fire-row">
          <label class="cal-field">
            <span>fire time (ms)</span>
            <input
              type="number"
              step={10}
              min={10}
              max={FIRE_MS_MAX}
              value={cur()}
              onInput={(e) => setForm(parseInt(e.currentTarget.value, 10))}
            />
          </label>
          <button class="fire-btn" onClick={fire}>Fire {cur()} ms</button>
          <button onClick={() => void persist(false)}>Apply</button>
          <button onClick={() => void persist(true)}>Save to disk</button>
          <AckChip ack={fireAck()} />
        </div>
        <div class="status-line">
          <Show when={status()}><span class="status">{status()}</span></Show>
          <Show when={error()}><span class="err">{error()}</span></Show>
        </div>
      </section>

      <section class="panel-section">
        <CalibrationEditor role="turret" />
      </section>
    </div>
  );
}
