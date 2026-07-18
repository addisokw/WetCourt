// Robot-aesthetic post-processing for TTS playback.
//
// Every TTS PCM chunk (see audio.ts) connects into this persistent effects
// graph instead of straight to the speakers, so the colour is uniform across
// personas and continuous across chunk boundaries (the effect nodes live for
// the lifetime of the AudioContext; only the per-chunk BufferSources churn).
//
// Signal path:
//   in ─┬─ dry ───────────────────────────────────────────────┐
//       └─ bandpass ─ peak ─ saturate ─ ringMod ─┬─────── wet ─┤
//                                                └─ comb ──────┘
//   (dry+wet) ─ tail ─► [glitch worklet: bitcrush/decimate/stutter]
//                     ─► master gain ─► limiter ─► out
//
// The glitch worklet loads asynchronously; until it's ready the tail feeds the
// master gain directly (native robot only), then we splice the worklet in.
// The master gain boosts the whole voice (a venue-loudness knob, per-persona
// like the rest); the limiter after it stops boosts above unity from hard-
// clipping at the destination.

import glitchUrl from './glitch-processor.js?url';

// ---- Tuning knobs (the "glitchy / degraded" preset) ----
const ROBOT_AMOUNT = 0.72; // default wet/dry blend; the operator slider overrides
const RING_HZ = 52; // ring-mod carrier — the core metallic buzz
const PEAK_HZ = 2200; // resonant honk frequency
const COMB_SECS = 0.004; // short comb delay → tube/metal resonance
const COMB_FEEDBACK = 0.35;
const SATURATION = 0.5; // soft-clip amount
const MAX_GLITCH_RATE = 1.8; // glitches/sec at full intensity

interface RobotChain {
  ctx: AudioContext;
  input: GainNode;
  tail: GainNode;
  master: GainNode;
  limiter: DynamicsCompressorNode;
  /// Lawyer-call audio joins here: straight into the limiter, bypassing both
  /// the robot effects and the per-persona master gain (the phone is not the
  /// judge). Created on first use.
  callInput?: GainNode;
  wet: GainNode;
  dry: GainNode;
  // Live-tunable nodes, kept so the Judge Mind robot controls can adjust the
  // colour without rebuilding the graph.
  peak: BiquadFilterNode;
  shaper: WaveShaperNode;
  carrier: OscillatorNode;
  glitch?: AudioWorkletNode;
}

let chain: RobotChain | null = null;
let workletLoading: Promise<void> | null = null;
// Live "robot" parameters. `intensity` is the 0..1 wet/dry blend; the rest were
// previously build-time constants and are now adjustable from the Judge Mind
// tab. Defaults reproduce the original "glitchy / degraded" preset. The glitch
// rate is decoupled from intensity (its own knob), seeded to the old coupled
// value (ROBOT_AMOUNT * MAX_GLITCH_RATE) for continuity.
let intensity = ROBOT_AMOUNT;
let glitchRate = ROBOT_AMOUNT * MAX_GLITCH_RATE;
let ringHz = RING_HZ;
let saturation = SATURATION;
let peakHz = PEAK_HZ;
let gain = 1;

/// Soft-clip transfer curve for the WaveShaper (digital harshness without
/// hard-clipping crunch).
function makeSaturationCurve(amount: number) {
  const n = 1024;
  const curve = new Float32Array(n);
  const k = amount * 40;
  for (let i = 0; i < n; i++) {
    const x = (i / (n - 1)) * 2 - 1;
    curve[i] = ((1 + k) * x) / (1 + k * Math.abs(x));
  }
  return curve;
}

/// Build (once) the native robot graph and return its input node. Every TTS
/// chunk connects here. Kicks off the glitch-worklet load on first use.
export function getRobotInput(ctx: AudioContext): AudioNode {
  if (chain && chain.ctx === ctx) return chain.input;

  const input = ctx.createGain();
  const tail = ctx.createGain();
  const dry = ctx.createGain();
  const wet = ctx.createGain();

  const bp = ctx.createBiquadFilter();
  bp.type = 'bandpass';
  bp.frequency.value = 1600;
  bp.Q.value = 0.6;

  const peak = ctx.createBiquadFilter();
  peak.type = 'peaking';
  peak.frequency.value = PEAK_HZ;
  peak.Q.value = 5;
  peak.gain.value = 9;

  const shaper = ctx.createWaveShaper();
  shaper.curve = makeSaturationCurve(SATURATION);
  shaper.oversample = '2x';

  // Ring modulation, native-node trick: an oscillator drives a gain's
  // AudioParam (base 0), so the signal is multiplied by the ±1 carrier.
  const ring = ctx.createGain();
  ring.gain.value = 0;
  const carrier = ctx.createOscillator();
  carrier.type = 'sine';
  carrier.frequency.value = RING_HZ;
  carrier.connect(ring.gain);
  carrier.start();

  // Short comb delay for a metallic resonance.
  const delay = ctx.createDelay(0.05);
  delay.delayTime.value = COMB_SECS;
  const fb = ctx.createGain();
  fb.gain.value = COMB_FEEDBACK;
  delay.connect(fb);
  fb.connect(delay);

  // Dry path.
  input.connect(dry);
  dry.connect(tail);
  // Wet path: EQ → saturate → ring-mod, with the comb as a parallel resonance.
  input.connect(bp);
  bp.connect(peak);
  peak.connect(shaper);
  shaper.connect(ring);
  ring.connect(wet);
  ring.connect(delay);
  delay.connect(wet);
  wet.connect(tail);

  // Master gain then a limiter: the gain is the loudness knob, the limiter
  // keeps boosts above unity from hard-clipping at the destination (the soft
  // clipper upstream only shapes the wet path).
  const master = ctx.createGain();
  const limiter = ctx.createDynamicsCompressor();
  limiter.threshold.value = -3;
  limiter.knee.value = 0;
  limiter.ratio.value = 20;
  limiter.attack.value = 0.002;
  limiter.release.value = 0.15;
  master.connect(limiter);
  limiter.connect(ctx.destination);

  // Until the worklet loads, the native chain feeds the master stage directly.
  tail.connect(master);

  chain = { ctx, input, tail, master, limiter, wet, dry, peak, shaper, carrier };
  applyParams();
  maybeInsertGlitch();
  return input;
}

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

/// Input node for lawyer-call playback (see audio.ts): joins the graph at the
/// limiter so the phone audio shares the clip protection but skips the robot
/// colour and the per-persona master gain — Dewey already sounds like a phone.
export function getCallAudioInput(ctx: AudioContext): AudioNode {
  getRobotInput(ctx); // ensure the chain (and its limiter) exists
  const c = chain!;
  if (!c.callInput) {
    c.callInput = ctx.createGain();
    c.callInput.connect(c.limiter);
  }
  return c.callInput;
}

/// Push the current parameters onto the live AudioParams / nodes. Safe to call
/// before the chain or worklet exist — it applies to whatever is present.
function applyParams(): void {
  if (!chain) return;
  chain.wet.gain.value = intensity;
  chain.dry.gain.value = 1 - intensity;
  chain.master.gain.value = gain;
  chain.carrier.frequency.value = ringHz;
  chain.peak.frequency.value = peakHz;
  chain.shaper.curve = makeSaturationCurve(saturation);
  const params = chain.glitch?.parameters;
  if (params) {
    const wetParam = params.get('wet');
    if (wetParam) wetParam.value = intensity;
    const rateParam = params.get('glitchRate');
    if (rateParam) rateParam.value = glitchRate;
  }
}

/// Live control: 0 = clean voice, 1 = full robot + glitch (wet/dry blend).
export function setRobotIntensity(amount: number): void {
  intensity = clamp(amount, 0, 1);
  applyParams();
}
export function getRobotIntensity(): number {
  return intensity;
}

/// Glitch tail rate in glitches/second, independent of intensity.
export function setRobotGlitchRate(rate: number): void {
  glitchRate = clamp(rate, 0, MAX_GLITCH_RATE * 2.5);
  applyParams();
}
/// Ring-modulation carrier frequency (Hz) — the core metallic buzz.
export function setRobotRingHz(hz: number): void {
  ringHz = clamp(hz, 10, 400);
  applyParams();
}
/// Soft-clip saturation amount (0..1) — digital harshness.
export function setRobotSaturation(amount: number): void {
  saturation = clamp(amount, 0, 1);
  applyParams();
}
/// Resonant "honk" peaking-filter frequency (Hz).
export function setRobotPeakHz(hz: number): void {
  peakHz = clamp(hz, 500, 5000);
  applyParams();
}
/// Master output gain (1 = unity). Values above 1 are caught by the limiter
/// rather than hard-clipping.
export function setRobotGain(amount: number): void {
  gain = clamp(amount, 0, 3);
  applyParams();
}

/// Begin loading the glitch worklet module. Safe to call repeatedly and before
/// the chain exists; the splice happens once both are ready.
export function initRobotWorklet(ctx: AudioContext): void {
  if (workletLoading) {
    maybeInsertGlitch();
    return;
  }
  workletLoading = ctx.audioWorklet
    .addModule(glitchUrl)
    .then(() => maybeInsertGlitch())
    .catch((e) => {
      // Native robot still works; we just lose the bitcrush/glitch tail.
      console.warn('glitch worklet failed to load; using native robot only', e);
    });
}

/// Splice the glitch worklet between the native tail and the destination, once
/// both the module and the chain are available.
function maybeInsertGlitch(): void {
  if (!chain || chain.glitch) return;
  const ctx = chain.ctx;
  // The module is registered process-wide once addModule resolves; constructing
  // the node throws if it isn't, so guard on the load promise having settled.
  if (!workletLoading) {
    initRobotWorklet(ctx);
    return;
  }
  void workletLoading.then(() => {
    if (!chain || chain.glitch) return;
    try {
      const glitch = new AudioWorkletNode(chain.ctx, 'glitch-processor', {
        numberOfInputs: 1,
        numberOfOutputs: 1,
        outputChannelCount: [1],
      });
      chain.tail.disconnect();
      chain.tail.connect(glitch);
      glitch.connect(chain.master);
      chain.glitch = glitch;
      applyParams(); // sync the worklet's wet/glitchRate to the current params

    } catch (e) {
      console.warn('could not construct glitch node; using native robot only', e);
    }
  });
}
