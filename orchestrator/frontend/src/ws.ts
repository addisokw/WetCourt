import { createSignal } from 'solid-js';
import { enqueuePcmFrame, endTtsSession, resumeAudio, startRecording, startTtsSession, stopAllPlayback, stopRecording } from './audio';
import { startTheater, stopTheater } from './theater';
import { onButtonPressed, onDeviceConnected, onDeviceDisconnected, setMaintenanceActive } from './maintenance';
import { applyRobotParamsToGraph } from './robotParams';

export type DisplayEvent = { type: string;[k: string]: unknown };

// Close code the server sends to an operator socket that a newer console has
// superseded (must match WS_SUPERSEDED in display/mod.rs). On this code the
// client goes dormant instead of auto-reconnecting.
const WS_SUPERSEDED = 4000;

export interface LogEntry {
  ts: number;
  ev: DisplayEvent | { type: string; binary_bytes: number };
}

export const [currentState, setCurrentState] = createSignal<string>('disconnected');
export const [log, setLog] = createSignal<LogEntry[]>([]);
export const [deliberation, setDeliberation] = createSignal<string>('');
export const [pleaWindowOpen, setPleaWindowOpen] = createSignal(false);
export const [recording, setRecording] = createSignal(false);
export const [lastVerdictGuilty, setLastVerdictGuilty] = createSignal<boolean | null>(null);
// Case-view signals — captured from display events for the presentational viewer.
export const [charge, setCharge] = createSignal<string>('');
export const [pleaTranscript, setPleaTranscript] = createSignal<string>('');
export const [verdictRemarks, setVerdictRemarks] = createSignal<string>('');
// The 2–4 word factor the judge named as deciding the case ("sincere apology").
// Shown on the case view at the reveal so the crowd learns what wins and loses.
export const [verdictKeyFactor, setVerdictKeyFactor] = createSignal<string>('');
export const [pleaRecordingActive, setPleaRecordingActive] = createSignal<boolean>(false);
// The judge's cross-examination follow-up question (empty when no cross-exam
// this trial). Set on the `cross_question` event, cleared at idle/reset.
export const [crossQuestion, setCrossQuestion] = createSignal<string>('');
// Operator toggle state for cross-examination (mirrors /operator/cross_exam).
export const [crossExamEnabled, setCrossExamEnabled] = createSignal<boolean>(true);
// Generic per-phase deadline countdown. Captured from server `phase_deadline`
// events; absolute Date.now() timestamp at which the current state will time
// out (or 0 if the active state has no deadline).
export const [phaseDeadlineAt, setPhaseDeadlineAt] = createSignal<number>(0);
export const [phaseDeadlineLabel, setPhaseDeadlineLabel] = createSignal<string>('');
// Deliberation theater is on between TheaterStart and TheaterEnd display
// events — independent of any state. Drives the pad (operator audio) and
// the dim/pulse visuals on every view.
export const [theaterActive, setTheaterActive] = createSignal<boolean>(false);
// Eye-safety: set when the orchestrator suppresses a guilty-verdict FIRE because
// vision had no fresh target lock. Surfaced as an operator banner; cleared at
// idle/reset when the next trial begins.
export const [fireHeldReason, setFireHeldReason] = createSignal<string>('');
// Set when the plea/answer fell back to "[no defense offered]" for a technical
// reason (STT failed or timed out) rather than silence — operator banner.
export const [pleaFallbackReason, setPleaFallbackReason] = createSignal<string>('');
// True while the open recording window is a cross-examination *answer* (the
// case view prompts "answer the judge" instead of "begin your defense").
export const [crossAnswerWindow, setCrossAnswerWindow] = createSignal<boolean>(false);
// Frozen remaining ms while the plea/answer clock is paused for a lawyer
// consultation (0 = not paused). Set by clock_paused, cleared by the next
// phase_deadline (resume) or reset.
export const [clockPausedMs, setClockPausedMs] = createSignal<number>(0);

// True while the judge is ringing the defendant's counsel during cross-exam
// (server `lawyer_calling` event). Drives the "pick up the phone" overlay;
// cleared on pickup, cross-window close, or reset/idle.
export const [lawyerCalling, setLawyerCalling] = createSignal<boolean>(false);
// Server-side problem report (printer not ready, print failed, …) — operator
// banner; cleared when the next trial starts.
export const [serverError, setServerError] = createSignal<string>('');

// TTS robot/glitch effect state now lives in robotSettings.ts (local to this
// browser's audio; seeded into the graph at startup via index.tsx).

const STATE_LABEL: Record<string, string> = {
  reset: 'idle',
  idle: 'idle',
  show_charge: 'displaying_charge',
  start_plea_recording: 'awaiting_plea',
  stop_plea_recording: 'transcribing',
  transcribing: 'transcribing',
  cross_question: 'cross_examining',
  transcript_ready: 'deliberating',
  verdict: 'pronouncing_verdict',
  execute_sentence: 'executing_sentence',
};

let socket: WebSocket | null = null;
let reconnectDelay = 500;
// Set when the most recent JSON event was `tts_audio`, so the next binary
// frame is interpreted as audio rather than logged as raw bytes.
let nextBinaryIsAudio = false;

function pushLog(entry: LogEntry) {
  setLog((prev) => {
    const next = prev.concat(entry);
    return next.length > 200 ? next.slice(next.length - 200) : next;
  });
}

let readOnly = false;
// Set when this read-only view opted in as the booth's speakers (?audio=1 on
// /ws/view): it receives + plays the PCM stream like the operator console.
let audioView = false;
// Set when this read-only view opted in as the booth's microphone (?mic=1 on
// /ws/view): it records the plea and uploads it over its own socket.
let micView = false;
// Whether THIS client should produce sound (operator console, or audio view).
const audioEnabled = () => !readOnly || audioView;
// True while a dedicated ?mic=1 kiosk is live somewhere (from mic_owner
// events / the snapshot); the operator console defers to it.
export const [micOwnerPresent, setMicOwnerPresent] = createSignal<boolean>(false);
// Whether THIS client should capture the plea: a mic view always does (the
// server drops uplink from superseded ones); the operator console does only
// when no dedicated mic kiosk is live.
const micEnabled = () => (readOnly ? micView : !micOwnerPresent());

export function connect(opts: { readOnly?: boolean; audio?: boolean; mic?: boolean } = {}) {
  readOnly = !!opts.readOnly;
  audioView = !!opts.audio;
  micView = !!opts.mic;
  // Tear down any prior socket so we don't accumulate listeners. Without this,
  // a Vite HMR reload or accidental double-mount stacks multiple live sockets
  // — every broadcast event then fires its handler N times, and signals like
  // `deliberation` (which use `prev => prev + ev.text`) get appended N times
  // per token. (Operator `/ws` is single-client so it self-protects; the
  // multi-viewer `/ws/view` is the one that needs explicit cleanup.)
  if (socket) {
    socket.onopen = null;
    socket.onmessage = null;
    socket.onclose = null;
    socket.onerror = null;
    if (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING) {
      socket.close();
    }
    socket = null;
  }
  const viewFlags = [audioView && 'audio=1', micView && 'mic=1'].filter(Boolean).join('&');
  const path = readOnly ? `/ws/view${viewFlags ? `?${viewFlags}` : ''}` : '/ws';
  const url = `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}${path}`;
  socket = new WebSocket(url);
  socket.binaryType = 'arraybuffer';

  socket.onopen = () => {
    reconnectDelay = 500;
    setCurrentState('connected');
    if (!readOnly) socket?.send(JSON.stringify({ type: 'ready' }));
  };

  socket.onmessage = (msg) => {
    if (typeof msg.data === 'string') {
      try {
        const ev = JSON.parse(msg.data) as DisplayEvent;
        pushLog({ ts: Date.now(), ev });
        handleEvent(ev);
      } catch {
        pushLog({ ts: Date.now(), ev: { type: 'parse_error' } });
      }
    } else {
      const buf = msg.data as ArrayBuffer;
      if (nextBinaryIsAudio) {
        enqueuePcmFrame(buf);
      } else {
        pushLog({ ts: Date.now(), ev: { type: 'binary_frame', binary_bytes: buf.byteLength } });
      }
    }
  };

  socket.onclose = (event) => {
    // The server closes an operator socket with WS_SUPERSEDED when a newer
    // console connects (last-connection-wins). Go dormant rather than
    // reconnecting — otherwise two open consoles evict each other forever.
    // Reload (or call connect()) to reclaim control in this tab.
    if (event.code === WS_SUPERSEDED) {
      setCurrentState('superseded');
      return;
    }
    setCurrentState('reconnecting');
    setTimeout(() => connect({ readOnly, audio: audioView, mic: micView }), reconnectDelay);
    reconnectDelay = Math.min(reconnectDelay * 2, 8000);
  };

  socket.onerror = () => socket?.close();
}

function handleEvent(ev: DisplayEvent) {
  if (STATE_LABEL[ev.type]) setCurrentState(STATE_LABEL[ev.type]);

  switch (ev.type) {
    case 'reset':
    case 'idle':
      setDeliberation('');
      setLastVerdictGuilty(null);
      setCharge('');
      setPleaTranscript('');
      setVerdictRemarks('');
      setVerdictKeyFactor('');
      setCrossQuestion('');
      setPleaRecordingActive(false);
      setPhaseDeadlineAt(0);
      setPhaseDeadlineLabel('');
      setFireHeldReason('');
      setPleaFallbackReason('');
      setCrossAnswerWindow(false);
      setLawyerCalling(false);
      setClockPausedMs(0);
      setServerError('');
      // E-stop (or any reset) silences speech immediately — already-scheduled
      // buffers included — and clears the queue for the next session.
      stopAllPlayback();
      if (theaterActive()) {
        setTheaterActive(false);
        if (audioEnabled()) stopTheater();
      }
      nextBinaryIsAudio = false;
      break;
    // Connect-time resync: the server's first event on every connection is
    // the live trial view-state, so a mid-trial (re)connect renders the
    // current phase instead of stale idle. Verdict fields only appear once
    // the reveal has happened (executing_sentence).
    case 'snapshot': {
      const phase = String(ev.phase ?? 'idle');
      const crossAnswer = phase === 'cross_answer';
      setCurrentState(crossAnswer ? 'awaiting_plea' : phase);
      setCrossAnswerWindow(crossAnswer);
      setCharge(String(ev.charge ?? ''));
      setPleaTranscript(String(ev.plea ?? ''));
      setCrossQuestion(String(ev.cross_question ?? ''));
      if (ev.verdict_guilty != null) {
        setLastVerdictGuilty(Boolean(ev.verdict_guilty));
        setVerdictRemarks(String(ev.verdict_remarks ?? ''));
        setVerdictKeyFactor(String(ev.verdict_key_factor ?? ''));
      } else {
        setLastVerdictGuilty(null);
      }
      if (ev.clock_paused) {
        setClockPausedMs(Math.max(1, Number(ev.deadline_ms ?? 0)));
        setPhaseDeadlineAt(0);
        setPhaseDeadlineLabel(phase);
      } else if (ev.deadline_ms != null) {
        setClockPausedMs(0);
        setPhaseDeadlineAt(Date.now() + Number(ev.deadline_ms));
        setPhaseDeadlineLabel(phase);
      } else {
        setClockPausedMs(0);
        setPhaseDeadlineAt(0);
        setPhaseDeadlineLabel('');
      }
      setPleaWindowOpen(phase === 'awaiting_plea' || crossAnswer);
      setPleaRecordingActive(false);
      setMicOwnerPresent(Boolean(ev.mic_owner));
      // Reconnected into an open window: restart the mic (recording is
      // browser-local, so whatever was captured died with the old socket).
      if (micEnabled() && (phase === 'awaiting_plea' || crossAnswer)) void beginPlea({ auto: true });
      setMaintenanceActive(Boolean(ev.maintenance));
      if (phase === 'idle') setDeliberation('');
      break;
    }
    case 'show_charge':
      setCharge(String(ev.text ?? ''));
      break;
    case 'transcript_ready':
      setPleaTranscript(String(ev.text ?? ''));
      break;
    case 'cross_question':
      setCrossQuestion(String(ev.text ?? ''));
      break;
    case 'tts_audio':
      // Subsequent binary frames are PCM audio chunks until tts_end.
      nextBinaryIsAudio = true;
      if (audioEnabled()) startTtsSession();
      break;
    case 'tts_end':
      nextBinaryIsAudio = false;
      if (audioEnabled()) {
        // Audio views drain the queue too, but only the operator reports
        // tts_finished (the view socket ignores inbound anyway).
        endTtsSession(() => {
          if (!readOnly) socket?.send(JSON.stringify({ type: 'tts_finished' }));
        });
      }
      break;
    case 'deliberation_token':
      setDeliberation((prev) => prev + (ev.text as string));
      break;
    case 'verdict':
      setLastVerdictGuilty(Boolean(ev.guilty));
      setVerdictRemarks(String(ev.remarks ?? ''));
      setVerdictKeyFactor(String(ev.key_factor ?? ''));
      break;
    case 'start_plea_recording':
      setPleaWindowOpen(true);
      setPleaRecordingActive(false);
      setCrossAnswerWindow(Boolean(ev.cross));
      // The mic opens the moment the window does — no "Plead" press needed.
      // The booth-mic client owns it (a ?mic=1 kiosk when live, else the
      // operator console); P still toggles (early stop / restart) and the
      // defendant's button still closes the window early.
      if (micEnabled()) void beginPlea({ auto: true });
      break;
    case 'mic_owner':
      setMicOwnerPresent(Boolean(ev.present));
      if (readOnly) break;
      if (ev.present) {
        // A mic kiosk appeared while we were capturing: yield without
        // uploading (the kiosk's recording is the plea now).
        if (recording()) void cancelPlea();
      } else if (pleaWindowOpen() && !recording()) {
        // The mic kiosk died mid-window: take the mic back so the plea
        // window isn't silently lost.
        void beginPlea({ auto: true });
      }
      break;
    case 'plea_recording':
      setPleaRecordingActive(Boolean(ev.active));
      break;
    case 'phase_deadline':
      setPhaseDeadlineLabel(String(ev.phase ?? ''));
      setPhaseDeadlineAt(Date.now() + Number(ev.deadline_ms ?? 0));
      setClockPausedMs(0); // any live deadline resumes a paused clock
      break;
    case 'clock_paused':
      setClockPausedMs(Math.max(1, Number(ev.remaining_ms ?? 0)));
      break;
    case 'lawyer_calling':
      setLawyerCalling(Boolean(ev.on));
      break;
    case 'theater_start':
      setTheaterActive(true);
      if (audioEnabled()) startTheater();
      break;
    case 'theater_end':
      setTheaterActive(false);
      if (audioEnabled()) stopTheater();
      break;
    case 'deliberation_complete':
      // No-op; deliberation buffer holds the full text.
      break;
    case 'stop_plea_recording':
      setPleaWindowOpen(false);
      setPleaRecordingActive(false);
      // Window closed (timeout or e-stop) — make sure we flush whatever was captured.
      if (recording()) void endPlea();
      break;
    // ---- Maintenance / hardware test plane ----
    case 'maintenance':
      setMaintenanceActive(Boolean(ev.active));
      break;
    case 'fire_held':
      setFireHeldReason(String(ev.reason ?? 'held for safety'));
      break;
    case 'plea_fallback':
      setPleaFallbackReason(String(ev.reason ?? 'transcription unavailable'));
      break;
    case 'error':
      setServerError(String(ev.message ?? 'unknown problem'));
      break;
    case 'device_connected':
      onDeviceConnected(String(ev.role ?? ''), String(ev.addr ?? ''));
      break;
    case 'device_disconnected':
      onDeviceDisconnected(String(ev.role ?? ''));
      break;
    case 'button_pressed':
      onButtonPressed();
      break;
    case 'robot_params':
      // Active persona's voice colour — apply to this client's audio graph.
      applyRobotParamsToGraph({
        intensity: Number(ev.intensity),
        glitch_rate: Number(ev.glitch_rate),
        ring_hz: Number(ev.ring_hz),
        saturation: Number(ev.saturation),
        peak_hz: Number(ev.peak_hz),
      });
      break;
  }
}

export async function beginPlea(opts: { auto?: boolean } = {}) {
  if (recording()) return;
  if (!pleaWindowOpen()) return;
  try {
    await startRecording();
    setRecording(true);
    socket?.send(JSON.stringify({ type: 'plea_recording_started' }));
  } catch (e) {
    pushLog({ ts: Date.now(), ev: { type: 'mic_error', message: String(e) } });
    // Manual press: concede the plea rather than wedge the trial. Auto-open:
    // leave the window running so the operator can fix the mic and press P;
    // the deadline still closes it if nobody does.
    if (!opts.auto) socket?.send(JSON.stringify({ type: 'plea_audio_complete' }));
  }
}

export async function endPlea() {
  if (!recording()) return;
  setRecording(false);
  const blob = await stopRecording();
  if (blob && blob.size > 0) {
    socket?.send(JSON.stringify({ type: 'plea_audio_chunk' }));
    socket?.send(await blob.arrayBuffer());
  }
  socket?.send(JSON.stringify({ type: 'plea_audio_complete' }));
}

/** Stop capturing WITHOUT uploading — used when a dedicated mic kiosk takes
 * over mid-recording (its capture is the plea; ours would corrupt it). */
async function cancelPlea() {
  if (!recording()) return;
  setRecording(false);
  await stopRecording(); // discard the blob
}

export async function fetchCrossExam(): Promise<void> {
  try {
    const res = await fetch('/operator/cross_exam');
    if (!res.ok) return;
    const data = (await res.json()) as { enabled: boolean };
    setCrossExamEnabled(Boolean(data.enabled));
  } catch {
    // leave the optimistic default in place
  }
}

export async function setCrossExam(enabled: boolean): Promise<void> {
  setCrossExamEnabled(enabled); // optimistic
  try {
    const res = await fetch('/operator/cross_exam', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ enabled }),
    });
    if (!res.ok) { await fetchCrossExam(); return; }
    const data = (await res.json()) as { enabled: boolean };
    setCrossExamEnabled(Boolean(data.enabled));
  } catch {
    await fetchCrossExam();
  }
}

/// Reclaim control in this tab after being superseded by another console.
/// Reconnecting bumps the server generation, so this tab becomes the live one
/// and the other goes dormant.
export function reconnect() {
  reconnectDelay = 500;
  connect({ readOnly, audio: audioView, mic: micView });
}

export async function startTrial() {
  resumeAudio(); // user gesture so the AudioContext can start producing sound
  await fetch('/operator/start', { method: 'POST' });
}
export async function emergencyStop() {
  await fetch('/operator/estop', { method: 'POST' });
}
