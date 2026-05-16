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
// Small lead-in for the first chunk of each TTS session so the output device
// has time to warm up; without it the leading ~100ms (often the first word)
// is scheduled inside the audio output latency window and gets clipped.
const TTS_LEAD_IN_SECS = 0.12;

let playCtx: AudioContext | null = null;
let nextStartTime = 0;
let queueDepth = 0;
let endingSession = false;
// Set true on `tts_audio`; the next enqueued chunk gets a lead-in and clears
// the flag so subsequent chunks chain seamlessly.
let sessionStartPending = false;
// Carries a trailing odd byte across WS frames so we never feed an
// odd-length buffer to Int16Array (which would byte-swap every subsequent
// sample and produce bursts of white noise at chunk seams).
let pcmResidue: number | null = null;

// Shared analyser tapped between each BufferSource and ctx.destination.
// face.ts reads time-domain samples from this to drive the avatar's jaw.
// The audio still routes to the speakers normally; the analyser is a
// passive observer in the graph.
let faceAnalyser: AnalyserNode | null = null;

function ensureCtx(): AudioContext {
  if (!playCtx) {
    playCtx = new AudioContext({ sampleRate: TTS_SAMPLE_RATE });
    faceAnalyser = playCtx.createAnalyser();
    faceAnalyser.fftSize = 512;      // ~21 ms @ 24 kHz
    faceAnalyser.smoothingTimeConstant = 0;
    faceAnalyser.connect(playCtx.destination);
  }
  // Browser autoplay policies require resumption inside a user gesture; the
  // Start button click handler triggers this.
  if (playCtx.state === 'suspended') void playCtx.resume();
  return playCtx;
}

/// Get the shared analyser node for face-driving. Returns null before the
/// first audio interaction; the face module should poll on its tick if it
/// matters, or call `resumeAudio()` first.
export function getFaceAnalyser(): AnalyserNode | null {
  ensureCtx();
  return faceAnalyser;
}

export function resumeAudio() {
  ensureCtx();
}

/// Mark the start of a TTS session so the first chunk gets a small lead-in.
export function startTtsSession() {
  ensureCtx();
  sessionStartPending = true;
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
  // Route through the analyser (which is itself connected to destination)
  // so the face module sees the same energy the speakers play.
  source.connect(faceAnalyser ?? ctx.destination);
  const earliest = sessionStartPending
    ? ctx.currentTime + TTS_LEAD_IN_SECS
    : ctx.currentTime;
  const startAt = Math.max(earliest, nextStartTime);
  source.start(startAt);
  nextStartTime = startAt + audioBuf.duration;
  sessionStartPending = false;
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

// ---------- PCM accumulator (for TalkingHead's speakAudio path) ----------
//
// When the face is mounted, ws.ts buffers PCM across the TTS session and
// hands the concatenated bytes to face.ts on tts_end. TalkingHead then plays
// the whole utterance through its own pipeline and drives proper visemes +
// "speaking" state head/brow animation that the chunked path can't trigger.

let pcmAccumulator: Uint8Array[] = [];
let pcmAccumulatorBytes = 0;

export function startPcmAccumulation() {
  pcmAccumulator = [];
  pcmAccumulatorBytes = 0;
  pcmResidue = null;
}

export function appendPcmChunk(buf: ArrayBuffer) {
  const u8 = new Uint8Array(buf);
  pcmAccumulator.push(u8);
  pcmAccumulatorBytes += u8.length;
}

export function takeAccumulatedPcm(): ArrayBuffer {
  // Kokoro emits even-length chunks but be defensive about a trailing byte
  // (the chunked path has dedicated residue handling; this path doesn't need
  // the same machinery since we get everything in one shot).
  const total = pcmAccumulatorBytes & ~1;
  const out = new Uint8Array(total);
  let off = 0;
  for (const c of pcmAccumulator) {
    if (off + c.length <= total) {
      out.set(c, off);
      off += c.length;
    } else {
      out.set(c.subarray(0, total - off), off);
      off = total;
      break;
    }
  }
  pcmAccumulator = [];
  pcmAccumulatorBytes = 0;
  return out.buffer;
}

export const TTS_PCM_SAMPLE_RATE = TTS_SAMPLE_RATE;

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
