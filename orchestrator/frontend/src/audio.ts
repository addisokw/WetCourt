// AudioContext-based PCM playback queue and mic capture.
//
// Playback: PCM s16le @ 24kHz mono comes in as binary WS frames. We decode
// each chunk into an AudioBuffer and schedule a BufferSource at the tail of
// the playback queue, so multiple sentences play back-to-back with no gaps.
//
// Capture: MediaRecorder produces a single webm/opus blob on stop, which we
// upload as one binary frame followed by a `plea_audio_complete` JSON event.
// Parakeet on the backend accepts standard formats — no client-side resampling.

import { getRobotInput, initRobotWorklet } from './robot';

const TTS_SAMPLE_RATE = 24000;
// F5: lawyer call audio arrives phone-band at 8 kHz (µ-law round-tripped in
// counsel). Played through a telephone band-pass instead of the robot worklet.
const PHONE_SAMPLE_RATE = 8000;
let phoneRoute = false;
let phoneFilter: BiquadFilterNode | null = null;

/** F5: route the next audio session through the telephone filter (lawyer call)
 * instead of the judge's robot voice. Each audio header re-sets this. */
export function setPhoneRoute(on: boolean) {
  phoneRoute = on;
}

/** Lazily-built ~300–3400 Hz band-pass giving lawyer audio a tinny handset
 * timbre. Returns the chain's input node (→ destination). */
function getPhoneInput(ctx: AudioContext): AudioNode {
  if (!phoneFilter || phoneFilter.context !== ctx) {
    const hp = ctx.createBiquadFilter();
    hp.type = 'highpass';
    hp.frequency.value = 300;
    const lp = ctx.createBiquadFilter();
    lp.type = 'lowpass';
    lp.frequency.value = 3400;
    hp.connect(lp).connect(ctx.destination);
    phoneFilter = hp;
  }
  return phoneFilter;
}
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
// Every scheduled-but-not-finished BufferSource, so an e-stop can silence
// speech that is already queued in the AudioContext.
const liveSources = new Set<AudioBufferSourceNode>();

/** Hard-stop TTS playback (e-stop / trial reset): kill every scheduled buffer
 * source and reset the queue state — including any half-carried PCM byte,
 * which would misalign the next session's first samples — so the next
 * session starts clean. */
export function stopAllPlayback() {
  for (const source of liveSources) {
    source.onended = null;
    try { source.stop(); } catch { /* already stopped */ }
  }
  liveSources.clear();
  nextStartTime = 0;
  queueDepth = 0;
  endingSession = false;
  sessionStartPending = false;
  onSessionDrained = null;
  pcmResidue = null;
}

function ensureCtx(): AudioContext {
  if (!playCtx) {
    playCtx = new AudioContext({ sampleRate: TTS_SAMPLE_RATE });
    // Preload the glitch worklet now (inside the Start-click gesture) so the
    // robot chain is fully armed well before the first TTS chunk arrives.
    initRobotWorklet(playCtx);
  }
  // Browser autoplay policies require resumption inside a user gesture; the
  // Start button click handler triggers this.
  if (playCtx.state === 'suspended') void playCtx.resume();
  return playCtx;
}

export function resumeAudio() {
  ensureCtx();
}

/// Shared playback AudioContext, for things that want to mix with TTS
/// (e.g. the deliberation-theater synth pad).
export function getPlaybackCtx(): AudioContext {
  return ensureCtx();
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
  // Web Audio resamples the buffer to the device rate, so an 8 kHz phone buffer
  // plays at the right pitch; the low rate is itself part of the phone timbre.
  const rate = phoneRoute ? PHONE_SAMPLE_RATE : TTS_SAMPLE_RATE;
  const audioBuf = ctx.createBuffer(1, samples.length, rate);
  const channel = audioBuf.getChannelData(0);
  for (let i = 0; i < samples.length; i++) channel[i] = samples[i] / 32768;

  const source = ctx.createBufferSource();
  source.buffer = audioBuf;
  source.connect(phoneRoute ? getPhoneInput(ctx) : getRobotInput(ctx));
  const earliest = sessionStartPending
    ? ctx.currentTime + TTS_LEAD_IN_SECS
    : ctx.currentTime;
  const startAt = Math.max(earliest, nextStartTime);
  source.start(startAt);
  nextStartTime = startAt + audioBuf.duration;
  sessionStartPending = false;
  queueDepth++;
  liveSources.add(source);
  source.onended = () => {
    liveSources.delete(source);
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
