import { onMount, onCleanup, createSignal, For, Show } from 'solid-js';
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
import { mountFace, dispose as disposeFace } from './face';

function fmt(ts: number): string {
  const d = new Date(ts);
  return `${d.toLocaleTimeString('en-US', { hour12: false })}.${String(d.getMilliseconds()).padStart(3, '0')}`;
}

export default function App() {
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const [faceError, setFaceError] = createSignal<string | null>(null);
  let faceEl: HTMLDivElement | undefined;

  onMount(() => {
    connect();
    if (faceEl) {
      void mountFace(faceEl).catch((e) => setFaceError(String(e)));
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.repeat) return;
      if (e.code === 'Space') { e.preventDefault(); startTrial(); }
      if (e.code === 'Escape') { e.preventDefault(); emergencyStop(); }
      if (e.code === 'KeyP' && pleaWindowOpen()) {
        e.preventDefault();
        if (recording()) void endPlea(); else void beginPlea();
      }
      if (e.code === 'Backquote') { e.preventDefault(); setDrawerOpen((v) => !v); }
    };
    window.addEventListener('keydown', onKey);
    onCleanup(() => {
      window.removeEventListener('keydown', onKey);
      disposeFace();
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
      <main class="stage">
        <div class="face" ref={faceEl}>
          <Show when={faceError()}>
            <div class="face-error">judge unavailable: {faceError()}</div>
          </Show>
        </div>
        <Show when={deliberation()}>
          <section class="deliberation">{deliberation()}</section>
        </Show>
      </main>
      <aside class={`drawer ${drawerOpen() ? 'open' : ''}`}>
        <div class="drawer-header">event log <span class="hint">(`)</span></div>
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
      </aside>
    </div>
  );
}
