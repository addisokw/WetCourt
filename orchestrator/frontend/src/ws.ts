import { createSignal } from 'solid-js';
import { enqueuePcmFrame, endTtsSession, resumeAudio, startRecording, startTtsSession, stopRecording } from './audio';
import { startTheater, stopTheater } from './theater';

export type DisplayEvent = { type: string;[k: string]: unknown };

export interface LogEntry {
  ts: number;
  ev: DisplayEvent | { type: string; binary_bytes: number };
}

export const [currentState, setCurrentState] = createSignal<string>('disconnected');
export const [log, setLog] = createSignal<LogEntry[]>([]);
export const [deliberation, setDeliberation] = createSignal<string>('');
export const [pleaWindowOpen, setPleaWindowOpen] = createSignal(false);
export const [recording, setRecording] = createSignal(false);
// Face-driving signals — monotonic timestamps the JudgeFace component samples
// each frame to drive mouth lip-sync and "thinking" beats.
export const [ttsActive, setTtsActive] = createSignal(false);
export const [lastTtsChunkAt, setLastTtsChunkAt] = createSignal(0);
export const [lastTokenAt, setLastTokenAt] = createSignal(0);
export const [lastVerdictGuilty, setLastVerdictGuilty] = createSignal<boolean | null>(null);
// Case-view signals — captured from display events for the presentational viewer.
export const [charge, setCharge] = createSignal<string>('');
export const [pleaTranscript, setPleaTranscript] = createSignal<string>('');
export const [verdictRemarks, setVerdictRemarks] = createSignal<string>('');
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

export function connect(opts: { readOnly?: boolean } = {}) {
  readOnly = !!opts.readOnly;
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
  const path = readOnly ? '/ws/view' : '/ws';
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

  socket.onclose = () => {
    setCurrentState('reconnecting');
    setTimeout(() => connect({ readOnly }), reconnectDelay);
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
      setTtsActive(false);
      setCharge('');
      setPleaTranscript('');
      setVerdictRemarks('');
      setCrossQuestion('');
      setPleaRecordingActive(false);
      setPhaseDeadlineAt(0);
      setPhaseDeadlineLabel('');
      if (theaterActive()) {
        setTheaterActive(false);
        if (!readOnly) stopTheater();
      }
      nextBinaryIsAudio = false;
      break;
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
      setTtsActive(true);
      setLastTtsChunkAt(performance.now());
      if (!readOnly) startTtsSession();
      break;
    case 'tts_end':
      nextBinaryIsAudio = false;
      setTtsActive(false);
      if (!readOnly) endTtsSession(() => socket?.send(JSON.stringify({ type: 'tts_finished' })));
      break;
    case 'deliberation_token':
      setDeliberation((prev) => prev + (ev.text as string));
      setLastTokenAt(performance.now());
      break;
    case 'verdict':
      setLastVerdictGuilty(Boolean(ev.guilty));
      setVerdictRemarks(String(ev.remarks ?? ''));
      break;
    case 'start_plea_recording':
      setPleaWindowOpen(true);
      setPleaRecordingActive(false);
      break;
    case 'plea_recording':
      setPleaRecordingActive(Boolean(ev.active));
      break;
    case 'phase_deadline':
      setPhaseDeadlineLabel(String(ev.phase ?? ''));
      setPhaseDeadlineAt(Date.now() + Number(ev.deadline_ms ?? 0));
      break;
    case 'theater_start':
      setTheaterActive(true);
      if (!readOnly) startTheater();
      break;
    case 'theater_end':
      setTheaterActive(false);
      if (!readOnly) stopTheater();
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
  }
}

export async function beginPlea() {
  if (recording()) return;
  if (!pleaWindowOpen()) return;
  try {
    await startRecording();
    setRecording(true);
    socket?.send(JSON.stringify({ type: 'plea_recording_started' }));
  } catch (e) {
    pushLog({ ts: Date.now(), ev: { type: 'mic_error', message: String(e) } });
    socket?.send(JSON.stringify({ type: 'plea_audio_complete' }));
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

export async function startTrial() {
  resumeAudio(); // user gesture so the AudioContext can start producing sound
  await fetch('/operator/start', { method: 'POST' });
}
export async function emergencyStop() {
  await fetch('/operator/estop', { method: 'POST' });
}
