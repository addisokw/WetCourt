//! 24 kHz → 8 kHz downsampler for the Kokoro→phone path. Exact 3:1
//! decimation behind a windowed-sinc FIR low-pass (cutoff ~3.4 kHz) so
//! telephone-band content survives and everything above 4 kHz dies before
//! the sample-rate drop aliases it.

const DECIMATION: usize = 3;
const TAPS: usize = 31;

/// Streaming decimator: feed arbitrary-length 24 kHz chunks, get 8 kHz out.
/// Carries filter history across calls so chunk boundaries are seamless.
pub struct Decimator {
    coeffs: [f32; TAPS],
    history: Vec<i16>, // last TAPS-1 input samples
    phase: usize,      // input-sample counter mod DECIMATION
}

impl Decimator {
    pub fn new() -> Self {
        Self {
            coeffs: design_lowpass(),
            history: vec![0; TAPS - 1],
            phase: 0,
        }
    }

    pub fn process(&mut self, input: &[i16]) -> Vec<i16> {
        let mut out = Vec::with_capacity(input.len() / DECIMATION + 1);
        // Work over history + input so the window can straddle the seam.
        let mut buf = Vec::with_capacity(self.history.len() + input.len());
        buf.extend_from_slice(&self.history);
        buf.extend_from_slice(input);

        // Output sample for every DECIMATION-th input sample. buf[i] is the
        // newest sample of the window ending at absolute input index
        // (i - (TAPS-1)) relative to this call's first new sample.
        for i in (TAPS - 1)..buf.len() {
            if self.phase == 0 {
                let mut acc = 0.0f32;
                for (k, &c) in self.coeffs.iter().enumerate() {
                    acc += c * buf[i - k] as f32;
                }
                out.push(acc.clamp(i16::MIN as f32, i16::MAX as f32) as i16);
            }
            self.phase = (self.phase + 1) % DECIMATION;
        }

        // Keep the tail as history for the next call.
        let keep = (TAPS - 1).min(buf.len());
        self.history = buf[buf.len() - keep..].to_vec();
        out
    }
}

/// Windowed-sinc low-pass: cutoff 3400 Hz at fs=24000, Hamming window,
/// normalized to unity DC gain.
fn design_lowpass() -> [f32; TAPS] {
    let fc = 3400.0 / 24000.0; // normalized cutoff
    let m = (TAPS - 1) as f32;
    let mut c = [0.0f32; TAPS];
    for (n, tap) in c.iter_mut().enumerate() {
        let x = n as f32 - m / 2.0;
        let sinc = if x.abs() < 1e-9 {
            2.0 * fc
        } else {
            (2.0 * std::f32::consts::PI * fc * x).sin() / (std::f32::consts::PI * x)
        };
        let hamming =
            0.54 - 0.46 * (2.0 * std::f32::consts::PI * n as f32 / m).cos();
        *tap = sinc * hamming;
    }
    let sum: f32 = c.iter().sum();
    for tap in c.iter_mut() {
        *tap /= sum;
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, fs: f32, n: usize, amp: f32) -> Vec<i16> {
        (0..n)
            .map(|i| {
                (amp * (2.0 * std::f32::consts::PI * freq * i as f32 / fs).sin())
                    as i16
            })
            .collect()
    }

    fn rms(s: &[i16]) -> f32 {
        (s.iter().map(|&x| (x as f32).powi(2)).sum::<f32>() / s.len() as f32)
            .sqrt()
    }

    #[test]
    fn output_rate_is_one_third() {
        let mut d = Decimator::new();
        let out = d.process(&vec![0i16; 2400]);
        // Exact count depends on filter warmup; must be within one sample of
        // 2400/3 accounting for the (TAPS-1) delay line.
        assert!((out.len() as i32 - 800).unsigned_abs() <= (TAPS as u32));
    }

    #[test]
    fn passband_survives_stopband_dies() {
        let fs = 24000.0;
        let n = 24000;
        // 1 kHz should pass nearly untouched.
        let mut d = Decimator::new();
        let low = d.process(&sine(1000.0, fs, n, 10000.0));
        let low_rms = rms(&low[100..]); // skip warmup
        assert!(low_rms > 6000.0, "1 kHz attenuated too much: rms {low_rms}");

        // 9 kHz would alias to 1 kHz after 3:1 decimation; must be crushed.
        let mut d = Decimator::new();
        let high = d.process(&sine(9000.0, fs, n, 10000.0));
        let high_rms = rms(&high[100..]);
        assert!(high_rms < 700.0, "9 kHz leaked through: rms {high_rms}");
    }

    #[test]
    fn chunked_equals_whole() {
        let signal = sine(700.0, 24000.0, 4800, 8000.0);
        let mut whole = Decimator::new();
        let a = whole.process(&signal);
        let mut chunked = Decimator::new();
        let mut b = Vec::new();
        for chunk in signal.chunks(311) {
            b.extend(chunked.process(chunk));
        }
        assert_eq!(a, b, "chunk boundaries changed the output");
    }
}
