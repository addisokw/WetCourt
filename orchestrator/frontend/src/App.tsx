import { createMemo, createSignal, onCleanup, onMount, For, Show } from 'solid-js';
import {
  beginPlea,
  connect,
  currentState,
  reconnect,
  deliberation,
  emergencyStop,
  endPlea,
  fetchCrossExam,
  fireHeldReason,
  pleaFallbackReason,
  log,
  phaseDeadlineAt,
  phaseDeadlineLabel,
  pleaWindowOpen,
  recording,
  startTrial,
} from './ws';
import VisionFeed from './panels/VisionFeed';
import { CaseContent } from './CaseView';

function fmt(ts: number): string {
  const d = new Date(ts);
  return `${d.toLocaleTimeString('en-US', { hour12: false })}.${String(d.getMilliseconds()).padStart(3, '0')}`;
}

// Read-only turret camera monitor for the operator console (the judge's face
// moved to the physical LED matrix, so this pane shows what the turret sees
// instead). Controls live in the Vision tab; this only polls /vision/state to
// tell "offline" apart from a dropped frame.
function TurretMonitor() {
  const [online, setOnline] = createSignal(false);
  let timer = 0;
  async function poll() {
    try {
      setOnline((await fetch('/vision/state')).ok);
    } catch {
      setOnline(false);
    }
  }
  onMount(() => {
    void poll();
    timer = window.setInterval(poll, 2000);
  });
  onCleanup(() => {
    if (timer) window.clearInterval(timer);
  });
  return (
    <VisionFeed online={online()}>
      <p>vision process offline</p>
      <p class="muted small">use the Vision tab to set up targeting</p>
    </VisionFeed>
  );
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
    void fetchCrossExam();
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
      <Show when={fireHeldReason()}>
        <div class="fire-held-banner">
          <span class="dot" /> Shot held for safety — no fresh target lock at sentencing
          ({fireHeldReason()}). The sentence advanced without firing.
        </div>
      </Show>
      <Show when={pleaFallbackReason()}>
        <div class="plea-fallback-banner">
          <span class="dot" /> No plea captured — {pleaFallbackReason()}. The defendant is being
          judged as offering no defense; E-Stop to retry the trial.
        </div>
      </Show>
      <Show when={currentState() === 'superseded'}>
        <div class="superseded-banner">
          <span class="dot" /> This console was taken over by another operator window.
          <button class="mini" onClick={reconnect}>Take control here</button>
        </div>
      </Show>
      <section class="monitors">
        <div class="monitor-pane monitor-vision">
          <div class="monitor-tag">TURRET</div>
          <TurretMonitor />
        </div>
        <div class="monitor-pane monitor-case">
          <div class="monitor-tag">CASE</div>
          <div class="case-embed"><CaseContent /></div>
        </div>
      </section>
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
