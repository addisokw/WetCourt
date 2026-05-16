import { getPlaybackCtx } from './audio';

// "Deliberation theater" ambient pad — a low, slow detuned chord with a
// breathing low-pass filter sweep. Plays during the deliberating state to
// add gravitas under the streaming verdict TTS. Fades in/out on start/stop
// so transitions in/out of the state don't click.

interface ActivePad {
  oscillators: OscillatorNode[];
  lfo: OscillatorNode;
  master: GainNode;
  filter: BiquadFilterNode;
  endsAt: number;
}

let active: ActivePad | null = null;

const FADE_IN_S = 1.2;
const FADE_OUT_S = 1.5;
const PEAK_GAIN = 0.07; // ambient — sits well under TTS

/// Start the deliberation pad. Safe to call repeatedly; subsequent calls are
/// no-ops while a pad is already active.
export function startTheater() {
  if (active) return;
  const ctx = getPlaybackCtx();
  const now = ctx.currentTime;

  // A minor 7 chord — the "deliberating jury" sound. Root low to feel weighty.
  // Frequencies tuned to a low A minor: A2, C3, E3, G3 with slight detune.
  const baseFreqs = [110.0, 130.81, 164.81, 196.0];

  const master = ctx.createGain();
  master.gain.value = 0;
  master.gain.setValueAtTime(0, now);
  master.gain.linearRampToValueAtTime(PEAK_GAIN, now + FADE_IN_S);

  const filter = ctx.createBiquadFilter();
  filter.type = 'lowpass';
  filter.frequency.value = 600;
  filter.Q.value = 4;

  // LFO sweeps the filter cutoff slowly — the "breathing" of the pad.
  const lfo = ctx.createOscillator();
  lfo.frequency.value = 0.15; // ~6.6s period
  const lfoGain = ctx.createGain();
  lfoGain.gain.value = 350;
  lfo.connect(lfoGain).connect(filter.frequency);
  lfo.start(now);

  const oscillators: OscillatorNode[] = [];
  for (const f of baseFreqs) {
    // Two detuned saws per note for chorus/width
    for (const detune of [-7, +7]) {
      const osc = ctx.createOscillator();
      osc.type = 'sawtooth';
      osc.frequency.value = f;
      osc.detune.value = detune;
      const voiceGain = ctx.createGain();
      voiceGain.gain.value = 1 / (baseFreqs.length * 2);
      osc.connect(voiceGain).connect(filter);
      osc.start(now);
      oscillators.push(osc);
    }
  }
  filter.connect(master).connect(ctx.destination);

  active = { oscillators, lfo, master, filter, endsAt: 0 };
}

/// Fade the pad out and stop all sources after the fade completes.
export function stopTheater() {
  if (!active) return;
  const ctx = getPlaybackCtx();
  const now = ctx.currentTime;
  const cur = active.master.gain.value;
  active.master.gain.cancelScheduledValues(now);
  active.master.gain.setValueAtTime(cur, now);
  active.master.gain.linearRampToValueAtTime(0, now + FADE_OUT_S);
  const stopAt = now + FADE_OUT_S + 0.05;
  for (const osc of active.oscillators) osc.stop(stopAt);
  active.lfo.stop(stopAt);
  active = null;
}

export function isTheaterActive(): boolean {
  return active !== null;
}
