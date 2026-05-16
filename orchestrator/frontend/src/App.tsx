import { createMemo, createSignal, onCleanup, onMount, For, Show } from 'solid-js';
import {
  beginPlea,
  connect,
  currentState,
  deliberation,
  emergencyStop,
  endPlea,
  log,
  phaseDeadlineAt,
  phaseDeadlineLabel,
  pleaWindowOpen,
  recording,
  startTrial,
} from './ws';
import PersonaPanel from './PersonaPanel';
import JudgeFace from './JudgeFace';
import { CaseContent } from './CaseView';

function fmt(ts: number): string {
  const d = new Date(ts);
  return `${d.toLocaleTimeString('en-US', { hour12: false })}.${String(d.getMilliseconds()).padStart(3, '0')}`;
}

function PhaseCountdown() {
  const [now, setNow] = createSignal(Date.now());
  let timer = 0;
  onMount(() => {
    timer = window.setInterval(() => setNow(Date.now()), 200);
  });
  onCleanup(() => {
    if (timer) window.clearInterval(timer);
  });
  const remainingMs = createMemo(() => Math.max(0, phaseDeadlineAt() - now()));
  const show = () => phaseDeadlineAt() > 0 && remainingMs() > 0;
  const secs = () => (remainingMs() / 1000).toFixed(remainingMs() < 10_000 ? 1 : 0);
  return (
    <Show when={show()}>
      <span class="phase-countdown" title={phaseDeadlineLabel()}>
        <span class="phase-countdown-label">{phaseDeadlineLabel().replace(/_/g, ' ')} →</span>
        <span class="phase-countdown-num">{secs()}s</span>
      </span>
    </Show>
  );
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
        <div class="banner-group">
          <h1 class={`banner state-${currentState()}`}>{currentState()}</h1>
          <PhaseCountdown />
        </div>
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
      <section class="monitors">
        <div class="monitor-pane monitor-face">
          <div class="monitor-tag">FACE</div>
          <JudgeFace />
        </div>
        <div class="monitor-pane monitor-case">
          <div class="monitor-tag">CASE</div>
          <div class="case-embed"><CaseContent /></div>
        </div>
      </section>
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
