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
  clockPausedMs,
  serverError,
  log,
  micOwnerPresent,
  phaseDeadlineAt,
  phaseDeadlineLabel,
  pleaRecordingActive,
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
  const paused = () => clockPausedMs() > 0;
  const remainingMs = createMemo(() =>
    paused() ? clockPausedMs() : Math.max(0, phaseDeadlineAt() - now()),
  );
  const show = () => paused() || (phaseDeadlineAt() > 0 && remainingMs() > 0);
  const secs = () => (remainingMs() / 1000).toFixed(remainingMs() < 10_000 ? 1 : 0);
  return (
    <Show when={show()}>
      <span class="phase-countdown" title={phaseDeadlineLabel()}>
        <span class="phase-countdown-label">
          {paused() ? '⏸ counsel consultation —' : `${phaseDeadlineLabel().replace(/_/g, ' ')} →`}
        </span>
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
        void pleaAction();
      }
    });
  });

  // The plea control adapts to who owns the booth mic. Local mic (no kiosk):
  // toggle this console's recorder. Kiosk mic (?mic=1 view live): the kiosk
  // records; the console's press closes the window early through the server
  // (same path as the defendant's done-talking button), which flushes the
  // kiosk's capture.
  async function pleaAction() {
    if (micOwnerPresent()) {
      if (pleaWindowOpen()) await fetch('/operator/defendant_press', { method: 'POST' });
      return;
    }
    if (recording()) void endPlea(); else void beginPlea();
  }

  const pleaButtonLabel = () => {
    if (micOwnerPresent()) {
      if (pleaRecordingActive()) return 'End plea (P) — kiosk mic';
      if (pleaWindowOpen()) return 'Close plea window (P) — kiosk mic';
      return 'Plea (kiosk mic)';
    }
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
            class={`plea ${recording() || (micOwnerPresent() && pleaRecordingActive()) ? 'recording' : ''}`}
            onClick={() => void pleaAction()}
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
      <Show when={!recording() && micOwnerPresent() && pleaRecordingActive()}>
        <div class="recording-banner">
          <span class="dot" /> Recording on the booth mic (kiosk) — P closes the window early.
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
      <Show when={serverError()}>
        <div class="fire-held-banner">
          <span class="dot" /> {serverError()}
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
