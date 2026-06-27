import { For } from 'solid-js';
import { sendCommand } from '../maintenance';
import { AckChip, CalibrationEditor, DeviceBadge, useAck } from './common';
import AimControl from './AimControl';

const PATTERNS = ['idle', 'thinking', 'verdict'] as const;

export default function AiJudgePanel() {
  const [ack, run] = useAck();

  return (
    <div class="panel-card">
      <header class="panel-card-head">
        <h2>AI judge — face &amp; gaze</h2>
        <DeviceBadge role="ai_judge" />
      </header>

      <section class="panel-section">
        <h3>Gaze</h3>
        <AimControl role="ai_judge" />
      </section>

      <section class="panel-section">
        <h3>Face panel</h3>
        <div class="btn-row">
          <For each={PATTERNS}>
            {(pattern) => (
              <button onClick={() => void run(sendCommand('ai_judge', { cmd: 'panel', pattern }))}>
                {pattern}
              </button>
            )}
          </For>
          <AckChip ack={ack()} />
        </div>
      </section>

      <section class="panel-section">
        <CalibrationEditor role="ai_judge" />
      </section>
    </div>
  );
}
