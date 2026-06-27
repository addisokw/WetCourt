// UI state + persistence for the robot-TTS effect (the Judge Mind "voice" knobs).
//
// The effect is local to whichever browser plays TTS audio (the operator /ws
// client) — there's no backend round-trip — so settings live in localStorage
// and are seeded into the Web Audio graph (robot.ts) at startup. Importing this
// module for its side effects performs that seed.

import { createSignal } from 'solid-js';
import {
  setRobotGlitchRate,
  setRobotIntensity,
  setRobotPeakHz,
  setRobotRingHz,
  setRobotSaturation,
} from './robot';

export interface RobotParamSpec {
  key: keyof RobotSettings;
  label: string;
  min: number;
  max: number;
  step: number;
  unit: string;
  /** Format the value for display next to the slider. */
  fmt: (v: number) => string;
}

export interface RobotSettings {
  intensity: number;
  glitchRate: number;
  ringHz: number;
  saturation: number;
  peakHz: number;
}

const DEFAULTS: RobotSettings = {
  intensity: 0.72,
  glitchRate: 1.3,
  ringHz: 52,
  saturation: 0.5,
  peakHz: 2200,
};

export const ROBOT_PARAMS: RobotParamSpec[] = [
  { key: 'intensity', label: 'Intensity', min: 0, max: 1, step: 0.01, unit: '', fmt: (v) => `${Math.round(v * 100)}%` },
  { key: 'glitchRate', label: 'Glitch rate', min: 0, max: 4, step: 0.1, unit: '/s', fmt: (v) => `${v.toFixed(1)}/s` },
  { key: 'ringHz', label: 'Ring-mod', min: 10, max: 400, step: 1, unit: 'Hz', fmt: (v) => `${Math.round(v)} Hz` },
  { key: 'saturation', label: 'Saturation', min: 0, max: 1, step: 0.01, unit: '', fmt: (v) => `${Math.round(v * 100)}%` },
  { key: 'peakHz', label: 'Honk freq', min: 500, max: 5000, step: 50, unit: 'Hz', fmt: (v) => `${Math.round(v)} Hz` },
];

const KEY = 'wetcourt.robot.v2';
const LEGACY_INTENSITY_KEY = 'wetcourt.robotIntensity';

function load(): RobotSettings {
  const out = { ...DEFAULTS };
  try {
    const raw = localStorage.getItem(KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<RobotSettings>;
      for (const k of Object.keys(DEFAULTS) as Array<keyof RobotSettings>) {
        const v = parsed[k];
        if (typeof v === 'number' && Number.isFinite(v)) out[k] = v;
      }
    } else {
      // Migrate the old single-intensity key, if present.
      const legacy = parseFloat(localStorage.getItem(LEGACY_INTENSITY_KEY) ?? '');
      if (Number.isFinite(legacy)) out.intensity = legacy;
    }
  } catch {
    /* ignore — fall back to defaults */
  }
  return out;
}

const initial = load();

export const [intensity, setIntensitySig] = createSignal(initial.intensity);
export const [glitchRate, setGlitchRateSig] = createSignal(initial.glitchRate);
export const [ringHz, setRingHzSig] = createSignal(initial.ringHz);
export const [saturation, setSaturationSig] = createSignal(initial.saturation);
export const [peakHz, setPeakHzSig] = createSignal(initial.peakHz);

const SIGNALS: Record<keyof RobotSettings, { get: () => number; set: (v: number) => void }> = {
  intensity: { get: intensity, set: setIntensitySig },
  glitchRate: { get: glitchRate, set: setGlitchRateSig },
  ringHz: { get: ringHz, set: setRingHzSig },
  saturation: { get: saturation, set: setSaturationSig },
  peakHz: { get: peakHz, set: setPeakHzSig },
};

const APPLY: Record<keyof RobotSettings, (v: number) => void> = {
  intensity: setRobotIntensity,
  glitchRate: setRobotGlitchRate,
  ringHz: setRobotRingHz,
  saturation: setRobotSaturation,
  peakHz: setRobotPeakHz,
};

function persist(): void {
  const snapshot: RobotSettings = {
    intensity: intensity(),
    glitchRate: glitchRate(),
    ringHz: ringHz(),
    saturation: saturation(),
    peakHz: peakHz(),
  };
  try {
    localStorage.setItem(KEY, JSON.stringify(snapshot));
  } catch {
    /* ignore */
  }
}

/** Read a param signal by key (for generic slider rendering). */
export function getParam(key: keyof RobotSettings): number {
  return SIGNALS[key].get();
}

/** Live-apply a robot param: graph + signal + persistence. */
export function applyParam(key: keyof RobotSettings, value: number): void {
  APPLY[key](value); // robot.ts clamps to its valid range
  SIGNALS[key].set(value);
  persist();
}

/** Reset all robot params to the default preset. */
export function resetRobotParams(): void {
  for (const k of Object.keys(DEFAULTS) as Array<keyof RobotSettings>) {
    applyParam(k, DEFAULTS[k]);
  }
}

// Seed the audio graph with the loaded values on startup (before any TTS plays).
for (const k of Object.keys(initial) as Array<keyof RobotSettings>) {
  APPLY[k](initial[k]);
}
