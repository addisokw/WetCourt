import { createMemo, For, onCleanup, onMount, Show } from 'solid-js';
import { calibrations, sendCommand } from '../maintenance';
import { startGamepad } from '../gamepad';
import { AckChip, CalibrationEditor, DeviceBadge, useAck } from './common';
import AimControl from './AimControl';

export default function TurretPanel() {
  // Aim is the `turret` board; firing is a separate `squirt` board (the NanoC6
  // has no spare GPIO for the relay alongside the servo-board I2C bus).
  const presets = createMemo(() => calibrations().squirt?.fire_presets_ms ?? []);
  const [fireAck, runFire] = useAck();

  function fire(ms: number) {
    void runFire(sendCommand('squirt', { cmd: 'fire', ms }));
  }

  onMount(() => {
    // Right trigger (index 7) fires the median preset; A (0) is unused here.
    const stop = startGamepad({
      onButtonDown: (i) => {
        if (i === 7) {
          const p = presets();
          if (p.length) fire(p[Math.floor(p.length / 2)]);
        }
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
        <h3>Fire <span class="muted small">(RT fires median preset)</span></h3>
        <div class="btn-row fire-row">
          <For each={presets()}>
            {(ms) => (
              <button class="fire-btn" onClick={() => fire(ms)}>
                {ms} ms
              </button>
            )}
          </For>
          <Show when={presets().length === 0}>
            <span class="muted small">no fire presets — add some in calibration</span>
          </Show>
          <AckChip ack={fireAck()} />
        </div>
      </section>

      <section class="panel-section">
        <CalibrationEditor role="turret" />
      </section>
    </div>
  );
}
