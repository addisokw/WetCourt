// AudioContext-based PCM playback queue and mic capture.
//
// Playback: PCM s16le @ 24kHz mono comes in as binary WS frames. We decode
// each chunk into an AudioBuffer and schedule a BufferSource at the tail of
// the playback queue, so multiple sentences play back-to-back with no gaps.
//
// Capture: MediaRecorder produces a single webm/opus blob on stop, which we
// upload as one binary frame followed by a `plea_audio_complete` JSON event.
// Parakeet on the backend accepts standard formats — no client-side resampling.

const TTS_SAMPLE_RATE = 24000;

let playCtx: AudioContext | null = null;
let nextStartTime = 0;
let queueDepth = 0;
let endingSession = false;
// Carries a trailing odd byte across WS frames so we never feed an
// odd-length buffer to Int16Array (which would byte-swap every subsequent
// sample and produce bursts of white noise at chunk seams).
let pcmResidue: number | null = null;

function ensureCtx(): AudioContext {
  if (!playCtx) {
    playCtx = new AudioContext({ sampleRate: TTS_SAMPLE_RATE });
  }
  // Browser autoplay policies require resumption inside a user gesture; the
  // Start button click handler triggers this.
  if (playCtx.state === 'suspended') void playCtx.resume();
  return playCtx;
}

export function resumeAudio() {
  ensureCtx();
}

export function enqueuePcmFrame(buf: ArrayBuffer) {
  const ctx = ensureCtx();
  const incoming = new Uint8Array(buf);
  const hasResidue = pcmResidue !== null;
  const totalLen = incoming.length + (hasResidue ? 1 : 0);
  if (totalLen < 2) {
    if (incoming.length === 1) pcmResidue = incoming[0];
    return;
  }
  const evenLen = totalLen - (totalLen & 1);
  const aligned = new Uint8Array(evenLen);
  if (hasResidue) {
    aligned[0] = pcmResidue!;
    aligned.set(incoming.subarray(0, evenLen - 1), 1);
  } else {
    aligned.set(incoming.subarray(0, evenLen));
  }
  pcmResidue = totalLen & 1 ? incoming[incoming.length - 1] : null;
  const samples = new Int16Array(aligned.buffer, aligned.byteOffset, evenLen / 2);
  if (samples.length === 0) return;
  const audioBuf = ctx.createBuffer(1, samples.length, TTS_SAMPLE_RATE);
  const channel = audioBuf.getChannelData(0);
  for (let i = 0; i < samples.length; i++) channel[i] = samples[i] / 32768;

  const source = ctx.createBufferSource();
  source.buffer = audioBuf;
  source.connect(ctx.destination);
  const startAt = Math.max(ctx.currentTime, nextStartTime);
  source.start(startAt);
  nextStartTime = startAt + audioBuf.duration;
  queueDepth++;
  source.onended = () => {
    queueDepth--;
    if (endingSession && queueDepth <= 0) {
      endingSession = false;
      onSessionDrained?.();
    }
  };
}

let onSessionDrained: (() => void) | null = null;

/// Called when the backend signals `tts_end`. If buffers are still playing,
/// invoke `cb` once the last one finishes; otherwise call it immediately.
export function endTtsSession(cb: () => void) {
  pcmResidue = null;
  if (queueDepth > 0) {
    endingSession = true;
    onSessionDrained = cb;
  } else {
    onSessionDrained = null;
    cb();
  }
}

// ---------- Mic capture ----------

let recorder: MediaRecorder | null = null;
let chunks: Blob[] = [];

export async function startRecording() {
  chunks = [];
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: { echoCancellation: true, noiseSuppression: true, channelCount: 1 },
  });
  // Pick a mime type the browser supports. Chrome on Windows: audio/webm;codecs=opus.
  const mime =
    MediaRecorder.isTypeSupported('audio/webm;codecs=opus') ? 'audio/webm;codecs=opus'
    : MediaRecorder.isTypeSupported('audio/ogg;codecs=opus') ? 'audio/ogg;codecs=opus'
    : '';
  recorder = mime ? new MediaRecorder(stream, { mimeType: mime }) : new MediaRecorder(stream);
  recorder.ondataavailable = (e) => {
    if (e.data && e.data.size > 0) chunks.push(e.data);
  };
  recorder.start();
}

/// Stop recording and return the captured blob. The caller uploads it.
export async function stopRecording(): Promise<Blob | null> {
  if (!recorder) return null;
  return new Promise((resolve) => {
    recorder!.onstop = () => {
      const all = new Blob(chunks, { type: recorder!.mimeType || 'audio/webm' });
      recorder!.stream.getTracks().forEach((t) => t.stop());
      recorder = null;
      resolve(all);
    };
    recorder!.stop();
  });
}
