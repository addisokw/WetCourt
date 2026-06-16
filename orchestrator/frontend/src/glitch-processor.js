// AudioWorklet processor: the "degraded machine" tail of the TTS robot chain.
//
// Three effects, all running on the audio thread:
//   1. Bitcrush      — quantise samples to `bits` levels for digital grit.
//   2. Decimation    — sample-and-hold every `reduction` samples (lo-fi
//                      sample-rate reduction / aliasing).
//   3. Glitch        — occasionally drop out (silence) or stutter (replay a
//                      short recent segment), like a buffer underrun.
//
// Mono in → mono out. A `wet` param blends the degraded signal back with the
// clean input so intelligibility stays tunable. Loaded by robot.ts via
// audioWorklet.addModule() and inserted after the native robot graph.

class GlitchProcessor extends AudioWorkletProcessor {
  static get parameterDescriptors() {
    return [
      // Bit depth for the crusher (lower = grittier).
      { name: 'bits', defaultValue: 6, minValue: 1, maxValue: 16, automationRate: 'k-rate' },
      // Sample-and-hold factor (1 = off, higher = more aliasing/lo-fi).
      { name: 'reduction', defaultValue: 3, minValue: 1, maxValue: 16, automationRate: 'k-rate' },
      // Roughly how many glitch events per second to trigger.
      { name: 'glitchRate', defaultValue: 1.2, minValue: 0, maxValue: 20, automationRate: 'k-rate' },
      // Degraded/clean blend (1 = fully degraded).
      { name: 'wet', defaultValue: 0.85, minValue: 0, maxValue: 1, automationRate: 'k-rate' },
    ];
  }

  constructor() {
    super();
    this.RING = 8192;
    this.MASK = this.RING - 1;
    this.ring = new Float32Array(this.RING); // recent samples, for stutter replay
    this.ringPos = 0;
    this.decPhase = 0;       // decimation counter
    this.held = 0;           // sample-and-hold value
    this.glitchLeft = 0;     // samples remaining in the current glitch
    this.glitchMode = 0;     // 0 none, 1 dropout, 2 stutter
    this.stutterPtr = 0;     // read cursor into the ring during a stutter
    this.stutterStart = 0;   // start of the looped segment
    this.stutterLen = 1;     // length of the looped segment
    this.stutterStep = 0;    // position within the current loop pass
  }

  process(inputs, outputs, params) {
    const input = inputs[0];
    const output = outputs[0];
    if (!output || output.length === 0) return true;
    const frames = output[0].length;
    const inp = input && input[0] ? input[0] : null;

    const bits = params.bits[0];
    const reduction = Math.max(1, params.reduction[0] | 0);
    const glitchRate = params.glitchRate[0];
    const wet = params.wet[0];
    const step = 2 / (Math.pow(2, bits) - 1);
    // Per-sample probability of starting a glitch, derived from the desired
    // per-second rate.
    const pGlitch = glitchRate / sampleRate;

    for (let i = 0; i < frames; i++) {
      const x = inp ? inp[i] : 0;

      // Keep a rolling history for stutter replays.
      this.ring[this.ringPos] = x;
      this.ringPos = (this.ringPos + 1) & this.MASK;

      // Maybe kick off a new glitch when we're not already in one.
      if (this.glitchLeft <= 0 && Math.random() < pGlitch) {
        if (Math.random() < 0.45) {
          this.glitchMode = 1; // dropout
          this.glitchLeft = 200 + ((Math.random() * 1600) | 0);
        } else {
          this.glitchMode = 2; // stutter
          this.stutterLen = 48 + ((Math.random() * 480) | 0);
          this.stutterStart = (this.ringPos - this.stutterLen) & this.MASK;
          this.stutterPtr = this.stutterStart;
          this.stutterStep = 0;
          this.glitchLeft = this.stutterLen * (2 + ((Math.random() * 5) | 0));
        }
      }

      // Source sample for this frame: clean, silenced, or replayed.
      let g = x;
      if (this.glitchLeft > 0) {
        if (this.glitchMode === 1) {
          g = 0;
        } else {
          if (this.stutterStep >= this.stutterLen) {
            this.stutterPtr = this.stutterStart;
            this.stutterStep = 0;
          }
          g = this.ring[this.stutterPtr & this.MASK];
          this.stutterPtr++;
          this.stutterStep++;
        }
        this.glitchLeft--;
      }

      // Decimate (sample-and-hold) then bitcrush the held value.
      if (this.decPhase <= 0) {
        this.held = Math.round(g / step) * step;
        this.decPhase = reduction;
      }
      this.decPhase--;

      const degraded = wet * this.held + (1 - wet) * x;
      for (let ch = 0; ch < output.length; ch++) output[ch][i] = degraded;
    }
    return true;
  }
}

registerProcessor('glitch-processor', GlitchProcessor);
