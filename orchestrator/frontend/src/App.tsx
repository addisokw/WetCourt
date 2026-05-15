import { onMount, For, Show } from 'solid-js';
import { connect, currentState, deliberation, log, startTrial, emergencyStop } from './ws';

function fmt(ts: number): string {
  const d = new Date(ts);
  return `${d.toLocaleTimeString('en-US', { hour12: false })}.${String(d.getMilliseconds()).padStart(3, '0')}`;
}

export default function App() {
  onMount(() => {
    connect();
    window.addEventListener('keydown', (e) => {
      if (e.code === 'Space') { e.preventDefault(); startTrial(); }
      if (e.code === 'Escape') { e.preventDefault(); emergencyStop(); }
    });
  });

  return (
    <div class="app">
      <header>
        <h1 class={`banner state-${currentState()}`}>{currentState()}</h1>
        <div class="controls">
          <button onClick={startTrial}>Start (Space)</button>
          <button class="estop" onClick={emergencyStop}>E-Stop (Esc)</button>
        </div>
      </header>
      <Show when={deliberation()}>
        <section class="deliberation">{deliberation()}</section>
      </Show>
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
