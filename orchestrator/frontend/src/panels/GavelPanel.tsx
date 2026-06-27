import { onCleanup, onMount } from 'solid-js';
import { sendCommand } from '../maintenance';
import { startGamepad } from '../gamepad';
import { AckChip, DeviceBadge, useAck } from './common';

export default function GavelPanel() {
  const [ack, run] = useAck();

  function strike() {
    void run(sendCommand('gavel', { cmd: 'gavel' }));
  }

  onMount(() => {
    // A button (index 0) strikes the gavel.
    const stop = startGamepad({ onButtonDown: (i) => i === 0 && strike() });
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
          <button class="gavel-btn" onClick={strike}>Strike gavel</button>
          <button onClick={() => void run(sendCommand('gavel', { cmd: 'ping' }))}>Ping</button>
          <AckChip ack={ack()} />
        </div>
      </section>
    </div>
  );
}
