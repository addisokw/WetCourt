import { onMount, For, Show } from 'solid-js';
import {
  beginPlea,
  connect,
  currentState,
  deliberation,
  emergencyStop,
  endPlea,
  log,
  pleaWindowOpen,
  recording,
  startTrial,
} from './ws';
import PersonaPanel from './PersonaPanel';

function fmt(ts: number): string {
  const d = new Date(ts);
  return `${d.toLocaleTimeString('en-US', { hour12: false })}.${String(d.getMilliseconds()).padStart(3, '0')}`;
}

export default function App() {
  onMount(() => {
    connect();
    window.addEventListener('keydown', (e) => {
      if (e.repeat) return;
      // Escape is always global — it's an emergency stop.
      if (e.code === 'Escape') { e.preventDefault(); emergencyStop(); return; }
      // Other shortcuts shouldn't hijack typing or button activation.
      const t = e.target as HTMLElement | null;
      const tag = t?.tagName;
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || tag === 'BUTTON' || t?.isContentEditable) return;
      if (e.code === 'Space') { e.preventDefault(); startTrial(); }
      if (e.code === 'KeyP' && pleaWindowOpen()) {
        e.preventDefault();
        if (recording()) void endPlea(); else void beginPlea();
      }
    });
  });

  const pleaButtonLabel = () => {
    if (recording()) return 'Stop pleading (P)';
    if (pleaWindowOpen()) return 'Plead (P)';
    return 'Plea (waiting for plea window)';
  };

  return (
    <div class="app">
      <header>
        <h1 class={`banner state-${currentState()}`}>{currentState()}</h1>
        <div class="controls">
          <button onClick={startTrial}>Start (Space)</button>
          <button
            class={`plea ${recording() ? 'recording' : ''}`}
            onClick={() => (recording() ? endPlea() : beginPlea())}
            disabled={!pleaWindowOpen() && !recording()}
          >
            {pleaButtonLabel()}
          </button>
          <button class="estop" onClick={emergencyStop}>E-Stop (Esc)</button>
        </div>
      </header>
      <Show when={recording()}>
        <div class="recording-banner">
          <span class="dot" /> Recording — speak your plea, click Stop or press P when done.
        </div>
      </Show>
      <Show when={deliberation()}>
        <section class="deliberation">{deliberation()}</section>
      </Show>
      <PersonaPanel />
      <section class="log">
        <For each={log().slice().reverse()}>
          {(entry) => (
            <div class="row">
              <span class="ts">{fmt(entry.ts)}</span>
              <span class={`type type-${entry.ev.type}`}>{entry.ev.type}</span>
              <span class="payload">{JSON.stringify(entry.ev)}</span>
            </div>
          )}
        </For>
      </section>
    </div>
  );
}
