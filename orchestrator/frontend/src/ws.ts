import { createSignal } from 'solid-js';

export type DisplayEvent = { type: string;[k: string]: unknown };

export interface LogEntry {
  ts: number;
  ev: DisplayEvent | { type: string; binary_bytes: number };
}

export const [currentState, setCurrentState] = createSignal<string>('disconnected');
export const [log, setLog] = createSignal<LogEntry[]>([]);

const STATE_EVENTS = new Set([
  'idle',
  'show_charge',
  'start_plea_recording',
  'stop_plea_recording',
  'transcribing',
  'transcript_ready',
  'verdict',
  'execute_sentence',
  'cooldown',
  'reset',
]);

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

function pushLog(entry: LogEntry) {
  setLog((prev) => {
    const next = prev.concat(entry);
    return next.length > 200 ? next.slice(next.length - 200) : next;
  });
}

export function connect() {
  const url = `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws`;
  socket = new WebSocket(url);

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
        if (STATE_EVENTS.has(ev.type)) {
          setCurrentState(STATE_LABEL[ev.type] ?? ev.type);
        }
        if (ev.type === 'tts_audio') {
          // Phase 1: auto-ack so the state machine advances even if we never get audio bytes.
          socket?.send(JSON.stringify({ type: 'tts_finished' }));
        }
      } catch (e) {
        pushLog({ ts: Date.now(), ev: { type: 'parse_error', raw: String(msg.data) } });
      }
    } else {
      // Binary frame (Phase 3 will be PCM TTS audio).
      const bytes = msg.data instanceof Blob ? msg.data.size : (msg.data as ArrayBuffer).byteLength;
      pushLog({ ts: Date.now(), ev: { type: 'binary_frame', binary_bytes: bytes } });
    }
  };

  socket.onclose = () => {
    setCurrentState('reconnecting');
    setTimeout(connect, reconnectDelay);
    reconnectDelay = Math.min(reconnectDelay * 2, 8000);
  };

  socket.onerror = () => {
    socket?.close();
  };
}

export async function startTrial() {
  await fetch('/operator/start', { method: 'POST' });
}
export async function emergencyStop() {
  await fetch('/operator/estop', { method: 'POST' });
}
