import { createSignal } from 'solid-js';
import {
  appendPcmChunk,
  enqueuePcmFrame,
  endTtsSession,
  getFaceAnalyser,
  resumeAudio,
  startPcmAccumulation,
  startRecording,
  startTtsSession,
  stopRecording,
  takeAccumulatedPcm,
} from './audio';
import { bindAnalyser, isMounted as isFaceMounted, playUtterance, setMood, type Mood } from './face';

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
// `true` for the current TTS session if we're routing PCM into face.ts's
// accumulator (TalkingHead plays + lipsyncs on tts_end). `false` falls back
// to audio.ts's chunk-by-chunk Web Audio queue.
let routeToFace = false;

// Mirror of orchestrator's `strip_markers` (verdict.rs:103). Removes the
// VERDICT/INTENSITY lines from the streamed deliberation so they're not
// counted as words to lipsync; the speakable text is what Kokoro actually
// synthesized.
function stripMarkers(text: string): string {
  return text
    .split('\n')
    .filter((l) => {
      const t = l.trimStart();
      return !t.startsWith('VERDICT:') && !t.startsWith('INTENSITY:');
    })
    .join('\n')
    .trim();
}

function pushLog(entry: LogEntry) {
  setLog((prev) => {
    const next = prev.concat(entry);
    return next.length > 200 ? next.slice(next.length - 200) : next;
  });
}

export function connect() {
  const url = `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws`;
  socket = new WebSocket(url);
  socket.binaryType = 'arraybuffer';

  socket.onopen = () => {
    reconnectDelay = 500;
    setCurrentState('connected');
    socket?.send(JSON.stringify({ type: 'ready' }));
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
        if (routeToFace) appendPcmChunk(buf);
        else enqueuePcmFrame(buf);
      } else {
        pushLog({ ts: Date.now(), ev: { type: 'binary_frame', binary_bytes: buf.byteLength } });
      }
    }
  };

  socket.onclose = () => {
    setCurrentState('reconnecting');
    setTimeout(connect, reconnectDelay);
    reconnectDelay = Math.min(reconnectDelay * 2, 8000);
  };

  socket.onerror = () => socket?.close();
}

// Per-state default mood. Verdict outcomes override `verdict` and `execute_sentence`.
const STATE_MOOD: Record<string, Mood> = {
  reset: 'neutral',
  idle: 'neutral',
  show_charge: 'stern',
  start_plea_recording: 'neutral',
  stop_plea_recording: 'neutral',
  transcribing: 'neutral',
  transcript_ready: 'stern',
  cooldown: 'neutral',
};

function handleEvent(ev: DisplayEvent) {
  if (STATE_LABEL[ev.type]) setCurrentState(STATE_LABEL[ev.type]);
  if (STATE_MOOD[ev.type]) setMood(STATE_MOOD[ev.type]);

  switch (ev.type) {
    case 'reset':
    case 'idle':
      setDeliberation('');
      nextBinaryIsAudio = false;
      break;
    case 'tts_audio':
      // Subsequent binary frames are PCM audio chunks until tts_end.
      nextBinaryIsAudio = true;
      // When the face is mounted, accumulate the full utterance so
      // TalkingHead can play it via speakAudio and run full speaking-state
      // animation. Otherwise fall back to the chunked Web Audio queue.
      routeToFace = isFaceMounted();
      if (routeToFace) {
        startPcmAccumulation();
      } else {
        startTtsSession();
      }
      break;
    case 'tts_end':
      nextBinaryIsAudio = false;
      if (routeToFace) {
        const pcm = takeAccumulatedPcm();
        const speakable = stripMarkers(deliberation());
        playUtterance(pcm, speakable, () => socket?.send(JSON.stringify({ type: 'tts_finished' })));
        routeToFace = false;
      } else {
        endTtsSession(() => socket?.send(JSON.stringify({ type: 'tts_finished' })));
      }
      break;
    case 'deliberation_token':
      setDeliberation((prev) => prev + (ev.text as string));
      break;
    case 'deliberation_complete':
      // No-op; deliberation buffer holds the full text.
      break;
    case 'verdict': {
      // Guilty: stern satisfaction. Acquitted: disgust (Wettington considers
      // acquittal a personal failure).
      const guilty = (ev as { guilty?: boolean }).guilty;
      setMood(guilty === false ? 'disappointed' : 'stern');
      break;
    }
    case 'start_plea_recording':
      setPleaWindowOpen(true);
      break;
    case 'stop_plea_recording':
      setPleaWindowOpen(false);
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
  // The face's analyser depends on the AudioContext that resumeAudio just
  // created. Bind once now; later starts are no-ops since the analyser
  // identity doesn't change.
  bindAnalyser(getFaceAnalyser());
  await fetch('/operator/start', { method: 'POST' });
}
export async function emergencyStop() {
  await fetch('/operator/estop', { method: 'POST' });
}
