// Robot voice-effect parameters — now a per-persona attribute owned by the host
// (see src/personas/mod.rs). The host pushes the active persona's params over
// the `robot_params` ws event; this module just describes the fields and applies
// them to the Web Audio graph (robot.ts). No localStorage — the persona record
// is the source of truth.

import {
  setRobotGain,
  setRobotGlitchRate,
  setRobotIntensity,
  setRobotPeakHz,
  setRobotRingHz,
  setRobotSaturation,
} from './robot';

export interface RobotParams {
  intensity: number;
  glitch_rate: number;
  ring_hz: number;
  saturation: number;
  peak_hz: number;
  gain: number;
}

export const ROBOT_DEFAULTS: RobotParams = {
  intensity: 0.72,
  glitch_rate: 1.3,
  ring_hz: 52,
  saturation: 0.5,
  peak_hz: 2200,
  gain: 1,
};

export interface RobotFieldSpec {
  key: keyof RobotParams;
  label: string;
  min: number;
  max: number;
  step: number;
  apply: (v: number) => void;
  fmt: (v: number) => string;
}

// Ranges mirror the clamps in robot.ts and the validation in personas/mod.rs.
export const ROBOT_FIELDS: RobotFieldSpec[] = [
  { key: 'intensity', label: 'Intensity', min: 0, max: 1, step: 0.01, apply: setRobotIntensity, fmt: (v) => `${Math.round(v * 100)}%` },
  { key: 'glitch_rate', label: 'Glitch rate', min: 0, max: 4, step: 0.1, apply: setRobotGlitchRate, fmt: (v) => `${v.toFixed(1)}/s` },
  { key: 'ring_hz', label: 'Ring-mod', min: 10, max: 400, step: 1, apply: setRobotRingHz, fmt: (v) => `${Math.round(v)} Hz` },
  { key: 'saturation', label: 'Saturation', min: 0, max: 1, step: 0.01, apply: setRobotSaturation, fmt: (v) => `${Math.round(v * 100)}%` },
  { key: 'peak_hz', label: 'Honk freq', min: 500, max: 5000, step: 50, apply: setRobotPeakHz, fmt: (v) => `${Math.round(v)} Hz` },
  { key: 'gain', label: 'Gain', min: 0, max: 3, step: 0.05, apply: setRobotGain, fmt: (v) => `${Math.round(v * 100)}%` },
];

/** Apply a full set of robot params to the live audio graph. */
export function applyRobotParamsToGraph(r: RobotParams): void {
  for (const f of ROBOT_FIELDS) f.apply(r[f.key]);
}
