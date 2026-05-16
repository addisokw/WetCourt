// Justice Wettington's face.
//
// Renders a glTF avatar via @met4citizen/talkinghead with TalkingHead's own
// lipsync/audio pipeline disabled (`avatarMute: true`). We drive the mouth
// morph targets ourselves from a Web Audio AnalyserNode tapped into the
// existing TTS playback chain. Mood is driven by kiosk state transitions.

import { TalkingHead } from '@met4citizen/talkinghead';

export type Mood = 'neutral' | 'stern' | 'pleased' | 'surprised' | 'disappointed';

const MOOD_MAP: Record<Mood, string> = {
  neutral: 'neutral',
  stern: 'angry',          // Wettington's resting state during deliberation
  pleased: 'happy',        // grudging recognition (rare)
  surprised: 'fear',       // when a defendant says something genuinely clever
  disappointed: 'disgust', // when forced to acquit
};

// RMS amplitude → mouth weight. The compressor avoids consonant transients
// slamming the jaw fully open.
const JAW_GAIN = 6;
const SUPPORT_RATIO = 0.3;
const SILENCE_RMS = 0.005;
const SUPPORT_MORPHS = ['mouthFunnel', 'mouthLowerDownLeft', 'mouthLowerDownRight'];

let head: TalkingHead | null = null;
let analyser: AnalyserNode | null = null;
// AnalyserNode.getByteTimeDomainData requires Uint8Array<ArrayBuffer>
// (not the broader ArrayBufferLike) per the DOM lib types.
let timeBuf: Uint8Array<ArrayBuffer> | null = null;
let lastWasSilent = true;
// While TalkingHead is driving speech via speakAudio it writes visemes
// through the animation queue (newvalue priority). Our amplitude path
// writes the realtime channel, which has higher priority, so we suppress
// it during TalkingHead-driven speech to avoid clobbering the visemes.
let speakingViaTH = false;

export function isMounted(): boolean {
  return head !== null;
}

export async function mountFace(el: HTMLElement, opts: { avatarUrl?: string } = {}) {
  const url = opts.avatarUrl ?? '/avatars/judge.glb';
  head = new TalkingHead(el, {
    cameraView: 'head',
    modelFPS: 60,
    lipsyncLang: 'en',
    avatarMood: 'neutral',
    pcmSampleRate: 24000,    // matches Kokoro PCM
    avatarIdleEyeContact: 0.4,
    avatarIdleHeadMove: 0.7,
    avatarSpeakingEyeContact: 0.6,
    avatarSpeakingHeadMove: 0.8,
    update: tick,
  });
  try {
    await head.showAvatar({ url, body: 'M', avatarMood: 'neutral', lipsyncLang: 'en' });
  } catch (e) {
    console.warn('[face] avatar load failed; tearing down', e);
    try { head?.stop(); } catch {}
    head = null;
    throw e;
  }
}

/// Hand TalkingHead a whole-utterance PCM buffer + the spoken text. TH does
/// playback, visemes, and full speaking-state head/brow animation. Word
/// timings are estimated from per-char distribution since Kokoro's stream
/// doesn't give us alignment.
export function playUtterance(rawPcm: ArrayBuffer, speakableText: string, onDone: () => void) {
  if (!head) { onDone(); return; }
  const audio = head.pcmToAudioBuffer(rawPcm);
  const text = speakableText.trim();
  if (!text || audio.duration < 0.05) {
    // Nothing to lipsync; just play the audio as-is with a marker for the ack.
    head.speakAudio({ audio, markers: [onDone], mtimes: [audio.duration * 1000 + 50] });
    speakingViaTH = true;
    return;
  }
  const words = text.split(/\s+/).filter((w) => w.length > 0);
  // Per-character time, including the implicit trailing space. Stressed
  // syllables actually take longer in real speech; this is an estimate.
  const totalUnits = words.reduce((s, w) => s + w.length + 1, 0);
  const perUnit = (audio.duration * 1000) / totalUnits;
  let t = 0;
  const wtimes: number[] = [];
  const wdurations: number[] = [];
  for (const w of words) {
    wtimes.push(t);
    const d = (w.length + 1) * perUnit;
    wdurations.push(d);
    t += d;
  }
  speakingViaTH = true;
  head.speakAudio(
    {
      audio,
      words,
      wtimes,
      wdurations,
      markers: [() => { speakingViaTH = false; onDone(); }],
      mtimes: [audio.duration * 1000 + 50],
    },
    { lipsyncLang: 'en' },
  );
}

export function bindAnalyser(node: AnalyserNode | null) {
  analyser = node;
  timeBuf = node ? new Uint8Array(new ArrayBuffer(node.fftSize)) : null;
}

export function setMood(m: Mood) {
  if (!head) return;
  try { head.setMood(MOOD_MAP[m]); } catch (e) {
    console.warn('[face] setMood failed:', e);
  }
}

export function dispose() {
  try { head?.stop(); } catch {}
  head = null;
  analyser = null;
  timeBuf = null;
}

function writeRealtime(mt: string, val: number | null) {
  if (!head) return;
  const o = head.mtAvatar[mt];
  if (!o) return; // avatar lacks this morph target
  o.realtime = val;
  o.needsUpdate = true;
}

function tick(_dt: number) {
  if (!head || !analyser || !timeBuf || speakingViaTH) return;
  analyser.getByteTimeDomainData(timeBuf);
  let sum = 0;
  for (let i = 0; i < timeBuf.length; i++) {
    const v = (timeBuf[i] - 128) / 128;
    sum += v * v;
  }
  const rms = Math.sqrt(sum / timeBuf.length);

  if (rms < SILENCE_RMS) {
    if (!lastWasSilent) {
      // Release the realtime channel so idle (blink, breathing, sway) resumes.
      writeRealtime('jawOpen', null);
      for (const m of SUPPORT_MORPHS) writeRealtime(m, null);
      lastWasSilent = true;
    }
    return;
  }
  lastWasSilent = false;
  // Use TalkingHead's internal `realtime` morph channel, not setFixedValue.
  // setFixedValue runs through exponential smoothing tuned for mood transitions
  // (~hundreds of ms to reach target); realtime writes apply directly each frame.
  const jaw = Math.min(1, rms * JAW_GAIN);
  writeRealtime('jawOpen', jaw);
  for (const m of SUPPORT_MORPHS) writeRealtime(m, jaw * SUPPORT_RATIO);
}
