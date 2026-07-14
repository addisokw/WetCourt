import { createMemo, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { buttonPresses, LedMode, sendCommand } from '../maintenance';
import { AckChip, DeviceBadge, useAck } from './common';

// Lamp modes in bringup order, with what each cue means on the booth.
const LED_MODES: Array<{ mode: LedMode; label: string; hint: string }> = [
  { mode: 'off', label: 'Off', hint: 'pressing does nothing' },
  { mode: 'on', label: 'On', hint: 'steady / press registered' },
  { mode: 'blink', label: 'Blink', hint: '“press me” attract' },
  { mode: 'pulse', label: 'Pulse', hint: 'ack window open' },
];

// How long the press dot stays lit after a wire BUTTON arrives.
const PRESS_FLASH_MS = 1500;

export default function SwearInPanel() {
  const [ack, run] = useAck();
  const [lastMode, setLastMode] = createSignal<LedMode | null>(null);
  const [simStatus, setSimStatus] = createSignal('');

  // A ticking "now" so the press indicator's age + flash decay re-render.
  const [now, setNow] = createSignal(Date.now());
  onMount(() => {
    const t = setInterval(() => setNow(Date.now()), 250);
    onCleanup(() => clearInterval(t));
  });

  const presses = buttonPresses;
  const pressLit = createMemo(() => presses().at > 0 && now() - presses().at < PRESS_FLASH_MS);
  const pressAge = createMemo(() => {
    const at = presses().at;
    if (!at) return '';
    const s = Math.max(0, Math.round((now() - at) / 1000));
    return s < 60 ? `${s}s ago` : `${Math.floor(s / 60)}m ago`;
  });

  function setLed(mode: LedMode) {
    setLastMode(mode);
    void run(sendCommand('swear_in', { cmd: 'led', mode }));
  }

  // Inject the same trial-start event a wire BUTTON produces, so the start
  // path is testable without the physical button. NOTE: the FSM ignores it in
  // maintenance mode; from live Idle it starts a real trial.
  async function simulatePress() {
    setSimStatus('');
    try {
      const res = await fetch('/operator/start', { method: 'POST' });
      setSimStatus(res.ok ? 'start event injected' : `failed: ${res.status}`);
    } catch (e) {
      setSimStatus(`failed: ${String(e)}`);
    }
  }

  return (
    <div class="panel-card">
      <header class="panel-card-head">
        <h2>Swear-in button</h2>
        <DeviceBadge role="swear_in" />
      </header>

      <section class="panel-section">
        <h3>Lamp</h3>
        <p class="muted small">
          The button's light is the defendant's cue: it should track what the
          booth wants from them. Firmware keeps it dark while its link is down
          and flashes it briefly on every press regardless of mode.
        </p>
        <div class="btn-row">
          <For each={LED_MODES}>
            {(m) => (
              <button
                classList={{ active: lastMode() === m.mode }}
                title={m.hint}
                onClick={() => setLed(m.mode)}
              >
                {m.label}
              </button>
            )}
          </For>
          <button onClick={() => void run(sendCommand('swear_in', { cmd: 'ping' }))}>Ping</button>
          <AckChip ack={ack()} />
        </div>
      </section>

      <section class="panel-section">
        <h3>Press</h3>
        <div class="btn-row">
          <span class={`press-indicator ${pressLit() ? 'lit' : ''}`}>
            <span class="dot" />
            <Show when={presses().count > 0} fallback={<>no presses yet</>}>
              {presses().count} press{presses().count === 1 ? '' : 'es'} · last {pressAge()}
            </Show>
          </span>
        </div>
        <p class="muted small">
          Lights on every wire <code>BUTTON</code>, even while the trial FSM
          ignores it (maintenance mode / mid-trial) — press the physical button
          and watch this dot to verify switch + debounce.
        </p>
        <div class="btn-row">
          <button onClick={() => void simulatePress()}>Simulate press</button>
          <Show when={simStatus()}><span class="muted small">{simStatus()}</span></Show>
        </div>
        <p class="muted small">
          Simulate injects the same trial-start event as a real press (no
          firmware involved). In maintenance mode the FSM ignores it; from
          live Idle it starts a real trial.
        </p>
      </section>
    </div>
  );
}
