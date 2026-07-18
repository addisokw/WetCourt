import { createMemo, createSignal, Match, onCleanup, onMount, Show, Switch } from 'solid-js';
import { resumeAudio } from './audio';
import {
  charge,
  clockPausedMs,
  connect,
  crossAnswerWindow,
  crossQuestion,
  currentState,
  deliberation,
  lastVerdictGuilty,
  lawyerCalling,
  phaseDeadlineAt,
  pleaRecordingActive,
  pleaTranscript,
  theaterActive,
  verdictRemarks,
  verdictKeyFactor,
} from './ws';

// Presentational read-only view served at /case — meant for the visitor /
// accused-facing monitor. Shows the charge, instructions on what to do, the
// plea transcript, and the verdict. No controls, no log, no operator chrome.

function StateInstruction() {
  return (
    <Switch>
      <Match when={currentState() === 'idle' || currentState() === 'connected'}>
        <p class="instruction big">PRESS THE BUTTON TO STAND TRIAL</p>
      </Match>
      <Match when={currentState() === 'displaying_charge'}>
        <p class="instruction">Listen carefully to the charge against you.</p>
      </Match>
      <Match when={currentState() === 'awaiting_plea' && clockPausedMs() > 0}>
        <p class="instruction big">You are consulting your counsel. The clock is stopped.</p>
      </Match>
      <Match when={currentState() === 'awaiting_plea' && !pleaRecordingActive()}>
        <p class="instruction big">
          {crossAnswerWindow()
            ? 'Press the plea button to answer the judge.'
            : 'Press the plea button to begin your defense.'}
        </p>
      </Match>
      <Match when={currentState() === 'awaiting_plea' && pleaRecordingActive()}>
        <p class="instruction big">
          {crossAnswerWindow()
            ? 'Press the button again to finish your answer.'
            : 'Press the button again to end your plea.'}
        </p>
      </Match>
      <Match when={currentState() === 'transcribing'}>
        <p class="instruction">Transcribing your plea…</p>
      </Match>
      <Match when={currentState() === 'cross_examining'}>
        <p class="instruction big">The judge has a question for you.</p>
      </Match>
      <Match when={currentState() === 'deliberating'}>
        <p class="instruction">The court is deliberating.</p>
      </Match>
      <Match when={currentState() === 'pronouncing_verdict'}>
        <p class="instruction">Hear the verdict.</p>
      </Match>
      <Match when={currentState() === 'executing_sentence'}>
        <p class="instruction">Sentence is being carried out.</p>
      </Match>
      <Match when={currentState() === 'disconnected' || currentState() === 'reconnecting'}>
        <p class="instruction muted">Connecting to court…</p>
      </Match>
    </Switch>
  );
}

function PleaCountdown() {
  const [now, setNow] = createSignal(Date.now());
  let timer = 0;
  onMount(() => {
    timer = window.setInterval(() => setNow(Date.now()), 200);
  });
  onCleanup(() => {
    if (timer) window.clearInterval(timer);
  });
  const paused = () => clockPausedMs() > 0;
  const remaining = createMemo(() =>
    paused()
      ? Math.max(0, Math.ceil(clockPausedMs() / 1000))
      : Math.max(0, Math.ceil((phaseDeadlineAt() - now()) / 1000)),
  );
  const label = () =>
    paused()
      ? 'seconds held — counsel consultation'
      : pleaRecordingActive()
        ? 'seconds remaining'
        : crossAnswerWindow()
          ? 'seconds to answer'
          : 'seconds to make your case';
  return (
    <Show when={(phaseDeadlineAt() > 0 || paused()) && currentState() === 'awaiting_plea'}>
      <div class={`countdown ${pleaRecordingActive() ? 'recording' : ''} ${paused() ? 'paused' : ''}`}>
        <span class="countdown-num">{remaining()}</span>
        <span class="countdown-label">{label()}</span>
      </div>
    </Show>
  );
}

function VerdictPanel() {
  const guilty = () => lastVerdictGuilty();
  return (
    <Show when={guilty() !== null}>
      <div class={`verdict-panel ${guilty() ? 'guilty' : 'not-guilty'}`}>
        <div class="verdict-word">{guilty() ? 'GUILTY' : 'NOT GUILTY'}</div>
        <Show when={verdictKeyFactor().length > 0}>
          <div class="verdict-key-factor">
            <span class="key-factor-label">WHAT DECIDED IT</span>
            <span class="key-factor-value">{verdictKeyFactor()}</span>
          </div>
        </Show>
        <Show when={verdictRemarks().length > 0}>
          <div class="verdict-remarks">{verdictRemarks()}</div>
        </Show>
      </div>
    </Show>
  );
}

// Charge stays hidden once we're pronouncing/executing/cooling-down; the
// verdict panel only reveals after TTS has actually drained on the client
// (state advances past pronouncing_verdict on the TtsFinished round-trip).
const CHARGE_HIDDEN_STATES = new Set(['pronouncing_verdict', 'executing_sentence']);
// Verdict word reveals as the judge says "GUILTY!" — at pronouncing_verdict
// entry (the server fires the Verdict display event after the theater beat).
const VERDICT_REVEAL_STATES = new Set(['pronouncing_verdict', 'executing_sentence']);
const DELIBERATION_VISIBLE_STATES = new Set(['deliberating', 'pronouncing_verdict']);

/// Reusable inner content — used by the standalone /case fullscreen view AND
/// embedded in the operator panel split-monitor preview.
export function CaseContent() {
  const showCharge = () => charge().length > 0 && !CHARGE_HIDDEN_STATES.has(currentState());
  const showPlea = () =>
    pleaTranscript().length > 0 &&
    (currentState() === 'deliberating' || currentState() === 'transcribing');
  // The cross-exam question stays up through the question, the answer window,
  // and the answer transcription, then clears as deliberation begins.
  const showCrossQuestion = () =>
    crossQuestion().length > 0 &&
    (currentState() === 'cross_examining' ||
      currentState() === 'awaiting_plea' ||
      currentState() === 'transcribing');
  // Marker lines are filtered server-side now (StreamMarkerFilter), so the
  // deliberation buffer is display-ready as-is.
  const cleanedDeliberation = () => deliberation();
  // Hide the deliberation as soon as the verdict reveals — otherwise it sits
  // below the GUILTY panel for the post-fire hold, which crowds the screen.
  const showDeliberation = () =>
    DELIBERATION_VISIBLE_STATES.has(currentState()) &&
    cleanedDeliberation().length > 0 &&
    lastVerdictGuilty() === null;

  return (
    <>
      <header class="case-header">
        <span class="case-mark">WET COURT OF APPEALS</span>
        <span class={`case-state state-${currentState()}`}>{currentState().replace(/_/g, ' ')}</span>
      </header>

      <main class="case-main">
        <Show when={showCharge()}>
          <section class="charge-block">
            <div class="charge-label">YOU ARE CHARGED WITH</div>
            <div class="charge-text">{charge()}</div>
          </section>
        </Show>

        <Show when={VERDICT_REVEAL_STATES.has(currentState())}>
          <VerdictPanel />
        </Show>

        <Show when={showPlea()}>
          <section class="plea-block">
            <div class="plea-label">YOUR PLEA</div>
            <div class="plea-text">“{pleaTranscript()}”</div>
          </section>
        </Show>

        <Show when={showCrossQuestion()}>
          <section class="cross-block">
            <div class="cross-label">THE JUDGE ASKS</div>
            <div class="cross-text">“{crossQuestion()}”</div>
          </section>
        </Show>

        <Show when={showDeliberation()}>
          <section class="deliberation-block">{cleanedDeliberation()}</section>
        </Show>

        <StateInstruction />
        <PleaCountdown />
      </main>

      <Show when={lawyerCalling()}>
        <div class="lawyer-calling-overlay">
          <div class="lawyer-calling-ring">☎</div>
          <div class="lawyer-calling-title">YOUR LAWYER IS CALLING</div>
          <div class="lawyer-calling-sub">Pick up the phone.</div>
        </div>
      </Show>
    </>
  );
}

export default function CaseView() {
  onMount(() => {
    // /case?audio=1 makes this kiosk the booth's speakers: it receives and
    // plays the TTS PCM + theater pad, decoupling show audio from the
    // operator laptop. /case?mic=1 makes it the booth's microphone: it
    // records the plea and uploads it over its own socket (the operator
    // console defers while a mic kiosk is live). Launch the kiosk browser
    // with --autoplay-policy=no-user-gesture-required (or click once) so the
    // AudioContext may start, and grant mic permission (kiosk setup uses
    // --use-fake-ui-for-media-stream to auto-accept — see deploy/spark-kiosk).
    const params = new URLSearchParams(location.search);
    const wantAudio = params.has('audio');
    const wantMic = params.has('mic');
    connect({ readOnly: true, audio: wantAudio, mic: wantMic });
    if (wantAudio) window.addEventListener('pointerdown', resumeAudio);
  });
  return (
    <div class={`case-view ${theaterActive() ? 'theater-active' : ''}`}>
      <CaseContent />
    </div>
  );
}
