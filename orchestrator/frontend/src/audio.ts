// AudioContext-based PCM playback queues and mic capture.
//
// Playback: two independent streams arrive as tagged binary WS frames (see
// ws.ts). Judge TTS is PCM s16le @ 24 kHz framed by tts_audio/tts_end; each
// chunk is decoded into an AudioBuffer and scheduled at the tail of the
// queue, so sentences play back-to-back with no gaps. Lawyer-call audio is a
// continuous 8 kHz s16le mirror of the phone earpiece, played through its
// own queue (no session events — the stream just runs while a call is live).
//
// Capture: MediaRecorder produces a single webm/opus blob on stop, which we
// upload as one binary frame followed by a `plea_audio_complete` JSON event.
// Parakeet on the backend accepts standard formats — no client-side resampling.

import { getCallAudioInput, getRobotInput, initRobotWorklet } from './robot';

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
// Every scheduled-but-not-finished BufferSource, so an e-stop can silence
// speech that is already queued in the AudioContext.
const liveSources = new Set<AudioBufferSourceNode>();

/** Hard-stop all playback (e-stop / trial reset): kill every scheduled buffer
 * source and reset the queue state — including any half-carried PCM byte,
 * which would misalign the next session's first samples — so the next
 * session starts clean. Call audio is silenced too; if the call is still
 * live, its stream re-buffers within a lead-in. */
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
  for (const source of callLiveSources) {
    source.onended = null;
    try { source.stop(); } catch { /* already stopped */ }
  }
  callLiveSources.clear();
  callNextStartTime = 0;
  callResidue = null;
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

/** Join `buf` with a carried odd byte from the previous chunk, returning
 * whole s16 samples and the new carry (chunks can split mid-sample). */
function alignPcm(
  buf: ArrayBuffer,
  residue: number | null,
): { samples: Int16Array | null; residue: number | null } {
  const incoming = new Uint8Array(buf);
  const hasResidue = residue !== null;
  const totalLen = incoming.length + (hasResidue ? 1 : 0);
  if (totalLen < 2) {
    return { samples: null, residue: incoming.length === 1 ? incoming[0] : residue };
  }
  const evenLen = totalLen - (totalLen & 1);
  const aligned = new Uint8Array(evenLen);
  if (hasResidue) {
    aligned[0] = residue!;
    aligned.set(incoming.subarray(0, evenLen - 1), 1);
  } else {
    aligned.set(incoming.subarray(0, evenLen));
  }
  return {
    samples: new Int16Array(aligned.buffer, aligned.byteOffset, evenLen / 2),
    residue: totalLen & 1 ? incoming[incoming.length - 1] : null,
  };
}

/** s16le samples → mono AudioBuffer at the given rate (the context resamples
 * on playback, so 8 kHz call audio plays fine in the 24 kHz context). */
function toAudioBuffer(ctx: AudioContext, samples: Int16Array, sampleRate: number): AudioBuffer {
  const audioBuf = ctx.createBuffer(1, samples.length, sampleRate);
  const channel = audioBuf.getChannelData(0);
  for (let i = 0; i < samples.length; i++) channel[i] = samples[i] / 32768;
  return audioBuf;
}

export function enqueuePcmFrame(buf: ArrayBuffer) {
  const ctx = ensureCtx();
  const { samples, residue } = alignPcm(buf, pcmResidue);
  pcmResidue = residue;
  if (!samples || samples.length === 0) return;
  const audioBuf = toAudioBuffer(ctx, samples, TTS_SAMPLE_RATE);
  const source = ctx.createBufferSource();
  source.buffer = audioBuf;
  source.connect(getRobotInput(ctx));
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

// ---------- Lawyer-call audio ----------
// Real-time mirror of the phone earpiece, paced by the RTP clock on the far
// end. A dry queue (call start, or after a network hiccup) re-arms a short
// jitter lead-in; otherwise chunks chain gaplessly like the TTS queue.

const CALL_SAMPLE_RATE = 8000;
const CALL_LEAD_IN_SECS = 0.15;
let callNextStartTime = 0;
let callResidue: number | null = null;
const callLiveSources = new Set<AudioBufferSourceNode>();

export function enqueueCallFrame(buf: ArrayBuffer) {
  const ctx = ensureCtx();
  const { samples, residue } = alignPcm(buf, callResidue);
  callResidue = residue;
  if (!samples || samples.length === 0) return;
  const audioBuf = toAudioBuffer(ctx, samples, CALL_SAMPLE_RATE);
  const source = ctx.createBufferSource();
  source.buffer = audioBuf;
  source.connect(getCallAudioInput(ctx));
  const dry = callNextStartTime <= ctx.currentTime;
  const startAt = dry ? ctx.currentTime + CALL_LEAD_IN_SECS : callNextStartTime;
  source.start(startAt);
  callNextStartTime = startAt + audioBuf.duration;
  callLiveSources.add(source);
  source.onended = () => callLiveSources.delete(source);
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
