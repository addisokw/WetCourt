import { onCleanup, onMount, createSignal } from 'solid-js';
import {
  currentState,
  lastTokenAt,
  lastTtsChunkAt,
  lastVerdictGuilty,
  theaterActive,
  ttsActive,
} from './ws';

const COLS = 48;
const ROWS = 13;
const EYE_ROW = 5;
const EYE_LEFT_COL = 14;
const EYE_RIGHT_COL = 32;
const MOUTH_ROW = 9;
const MOUTH_COL = 21;
const NOISE_CHARS = '.,`\'';

type FaceMood = 'idle' | 'listening' | 'thinking' | 'speaking' | 'guilty' | 'innocent';

function moodFromState(state: string, recentToken: boolean, speaking: boolean, verdict: boolean | null): FaceMood {
  if (state === 'pronouncing_verdict' || state === 'executing_sentence') {
    if (verdict === true) return 'guilty';
    if (verdict === false) return 'innocent';
  }
  if (speaking) return 'speaking';
  if (state === 'deliberating' || recentToken) return 'thinking';
  if (state === 'awaiting_plea' || state === 'transcribing') return 'listening';
  return 'idle';
}

function eyeGlyphs(mood: FaceMood): [string, string] {
  switch (mood) {
    case 'listening': return ['-', '-'];
    case 'thinking':  return ['O', 'O'];
    case 'speaking':  return ['o', 'o'];
    case 'guilty':    return ['x', 'x'];
    case 'innocent':  return ['^', '^'];
    default:          return ['o', 'o'];
  }
}

function mouthGlyph(mood: FaceMood, phase: number): string {
  if (mood === 'speaking') {
    const frames = [' ___ ', ' vvv ', ' \\_/ ', ' --- '];
    return frames[Math.floor(phase * frames.length) % frames.length];
  }
  if (mood === 'thinking') {
    const frames = [' vvv ', ' v.v ', ' ___ '];
    return frames[Math.floor(phase * frames.length) % frames.length];
  }
  if (mood === 'guilty')    return ' ^^^ ';
  if (mood === 'innocent')  return ' \\_/ ';
  if (mood === 'listening') return ' --- ';
  return ' ___ ';
}

function densityFor(mood: FaceMood): number {
  switch (mood) {
    case 'guilty':    return 90;
    case 'thinking':  return 50;
    case 'speaking':  return 35;
    case 'listening': return 18;
    case 'innocent':  return 8;
    default:          return 14;
  }
}

function moodColor(mood: FaceMood): string {
  switch (mood) {
    case 'thinking':  return '#a371f7';
    case 'speaking':  return '#d2a8ff';
    case 'guilty':    return '#ff6b6b';
    case 'innocent':  return '#7ee787';
    case 'listening': return '#f0883e';
    default:          return '#8b949e';
  }
}

function renderLayers(mood: FaceMood, phase: number): { noise: string; features: string } {
  const density = densityFor(mood);
  const noiseGrid: string[][] = [];
  const featGrid: string[][] = [];
  for (let r = 0; r < ROWS; r++) {
    noiseGrid.push(new Array<string>(COLS).fill(' '));
    featGrid.push(new Array<string>(COLS).fill(' '));
  }
  // Noise field
  for (let i = 0; i < density; i++) {
    const c = Math.floor(Math.random() * COLS);
    const r = Math.floor(Math.random() * ROWS);
    noiseGrid[r][c] = NOISE_CHARS[Math.floor(Math.random() * NOISE_CHARS.length)];
  }
  // Carve breathing room around features so noise doesn't visually clash with them
  for (let dr = -1; dr <= 1; dr++) {
    for (let dc = -1; dc <= 1; dc++) {
      const r = EYE_ROW + dr;
      [EYE_LEFT_COL + dc, EYE_RIGHT_COL + dc].forEach((c) => {
        if (r >= 0 && r < ROWS && c >= 0 && c < COLS) noiseGrid[r][c] = ' ';
      });
    }
  }
  for (let dc = -2; dc <= 6; dc++) {
    const c = MOUTH_COL + dc;
    if (MOUTH_ROW >= 0 && MOUTH_ROW < ROWS && c >= 0 && c < COLS) noiseGrid[MOUTH_ROW][c] = ' ';
  }
  // Features
  const [eL, eR] = eyeGlyphs(mood);
  featGrid[EYE_ROW][EYE_LEFT_COL] = eL;
  featGrid[EYE_ROW][EYE_RIGHT_COL] = eR;
  const mouth = mouthGlyph(mood, phase);
  for (let i = 0; i < mouth.length && MOUTH_COL + i < COLS; i++) {
    if (mouth[i] !== ' ') featGrid[MOUTH_ROW][MOUTH_COL + i] = mouth[i];
  }
  return {
    noise: noiseGrid.map((r) => r.join('')).join('\n'),
    features: featGrid.map((r) => r.join('')).join('\n'),
  };
}

export default function JudgeFace() {
  const [noise, setNoise] = createSignal<string>('');
  const [features, setFeatures] = createSignal<string>('');
  const [mood, setMood] = createSignal<FaceMood>('idle');
  let rafId = 0;
  let lastFrame = 0;

  onMount(() => {
    const tick = (now: number) => {
      if (now - lastFrame >= 33) {
        lastFrame = now;
        const recentToken = now - lastTokenAt() < 800;
        const speaking = ttsActive() || (now - lastTtsChunkAt() < 250);
        const m = moodFromState(currentState(), recentToken, speaking, lastVerdictGuilty());
        setMood(m);
        const phase = (now / 250) % 1;
        const layers = renderLayers(m, phase);
        setNoise(layers.noise);
        setFeatures(layers.features);
      }
      rafId = requestAnimationFrame(tick);
    };
    rafId = requestAnimationFrame(tick);
  });

  onCleanup(() => {
    if (rafId) cancelAnimationFrame(rafId);
  });

  return (
    <section class={`judge-face mood-${mood()} ${theaterActive() ? 'theater-active' : ''}`}>
      <div class="face-stack">
        <pre class="face-noise">{noise()}</pre>
        <pre class="face-features" style={{ color: moodColor(mood()) }}>{features()}</pre>
      </div>
    </section>
  );
}
