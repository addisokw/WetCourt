import { createSignal } from 'solid-js';
import { enqueuePcmFrame, endTtsSession, resumeAudio, startRecording, startTtsSession, stopRecording } from './audio';

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
export const [verdictIntensity, setVerdictIntensity] = createSignal<number>(0);
export const [pleaRecordingActive, setPleaRecordingActive] = createSignal<boolean>(false);
// Generic per-phase deadline countdown. Captured from server `phase_deadline`
// events; absolute Date.now() timestamp at which the current state will time
// out (or 0 if the active state has no deadline).
export const [phaseDeadlineAt, setPhaseDeadlineAt] = createSignal<number>(0);
export const [phaseDeadlineLabel, setPhaseDeadlineLabel] = createSignal<string>('');

const STATE_LABEL: Record<string, string> = {
  reset: 'idle',
  idle: 'idle',
  show_charge: 'displaying_charge',
  start_plea_recording: 'awaiting_plea',
  stop_plea_recording: 'transcribing',
  transcribing: 'transcribing',
  transcript_ready: 'deliberating',
  verdict: 'pronouncing_verdict',
  execute_sentence: 'executing_sentence',
  cooldown: 'cooldown',
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
      setVerdictIntensity(0);
      setPleaRecordingActive(false);
      setPhaseDeadlineAt(0);
      setPhaseDeadlineLabel('');
      nextBinaryIsAudio = false;
      break;
    case 'show_charge':
      setCharge(String(ev.text ?? ''));
      break;
    case 'transcript_ready':
      setPleaTranscript(String(ev.text ?? ''));
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
      setVerdictIntensity(Number(ev.intensity ?? 0));
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

export async function startTrial() {
  resumeAudio(); // user gesture so the AudioContext can start producing sound
  await fetch('/operator/start', { method: 'POST' });
}
export async function emergencyStop() {
  await fetch('/operator/estop', { method: 'POST' });
}
