import { For } from 'solid-js';
import { sendCommand } from '../maintenance';
import { AckChip, CalibrationEditor, DeviceBadge, useAck } from './common';
import AimControl from './AimControl';

const PATTERNS = ['idle', 'thinking', 'verdict'] as const;

// The judge head is two independently-flashed boards: the LED-matrix face
// (role judge_face, owns PANEL) and the pan/tilt gaze neck (role judge_neck,
// owns AIM). They share this one console tab.
export default function JudgeBodyPanel() {
  const [ack, run] = useAck();

  return (
    <div class="panel-card">
      <header class="panel-card-head">
        <h2>Judge body — face &amp; neck</h2>
      </header>

      <section class="panel-section">
        <h3>Face <DeviceBadge role="judge_face" /></h3>
        <div class="btn-row">
          <For each={PATTERNS}>
            {(pattern) => (
              <button onClick={() => void run(sendCommand('judge_face', { cmd: 'panel', pattern }))}>
                {pattern}
              </button>
            )}
          </For>
          <AckChip ack={ack()} />
        </div>
      </section>

      <section class="panel-section">
        <h3>Neck (gaze) <DeviceBadge role="judge_neck" /></h3>
        <AimControl role="judge_neck" />
      </section>

      <section class="panel-section">
        <CalibrationEditor role="judge_neck" />
      </section>
    </div>
  );
}
