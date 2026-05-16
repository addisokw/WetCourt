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

export async function mountFace(el: HTMLElement, opts: { avatarUrl?: string } = {}) {
  const url = opts.avatarUrl ?? '/avatars/judge.glb';
  head = new TalkingHead(el, {
    cameraView: 'head',
    avatarMute: true,        // we drive lipsync ourselves; no TTS playback inside TH
    modelFPS: 60,
    lipsyncLang: 'en',
    avatarMood: 'neutral',
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
  if (!head || !analyser || !timeBuf) return;
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
