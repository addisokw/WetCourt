//! G.711 µ-law codec (PCMU, RTP payload type 0). Algorithmic Sun/ITU
//! reference implementation — at 8000 samples/s a table buys nothing.

const BIAS: i32 = 0x84;
const CLIP: i32 = 32635;

pub fn linear_to_ulaw(sample: i16) -> u8 {
    let mut s = sample as i32;
    let sign = if s < 0 {
        s = -s;
        0x80u8
    } else {
        0
    };
    if s > CLIP {
        s = CLIP;
    }
    s += BIAS;
    // Segment = position of the highest set bit above bit 7 (0..=7).
    let exponent = (31 - (s as u32 >> 7).leading_zeros().min(31)).min(7) as i32;
    let mantissa = (s >> (exponent + 3)) & 0x0F;
    !(sign | ((exponent as u8) << 4) | mantissa as u8)
}

pub fn ulaw_to_linear(ulaw: u8) -> i16 {
    let u = !ulaw;
    let exponent = ((u & 0x70) >> 4) as i32;
    let mantissa = (u & 0x0F) as i32;
    let mut t = (mantissa << 3) + BIAS;
    t <<= exponent;
    if u & 0x80 != 0 {
        (BIAS - t) as i16
    } else {
        (t - BIAS) as i16
    }
}

pub fn encode(samples: &[i16]) -> Vec<u8> {
    samples.iter().map(|&s| linear_to_ulaw(s)).collect()
}

pub fn decode(ulaw: &[u8]) -> Vec<i16> {
    ulaw.iter().map(|&b| ulaw_to_linear(b)).collect()
}

/// µ-law silence (linear 0 encodes to 0xFF).
pub const ULAW_SILENCE: u8 = 0xFF;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        // ITU reference points.
        assert_eq!(linear_to_ulaw(0), 0xFF);
        assert_eq!(ulaw_to_linear(0xFF), 0);
        assert_eq!(ulaw_to_linear(0x7F), -0); // negative zero collapses
        // Max positive segment.
        assert_eq!(linear_to_ulaw(32635), 0x80);
        assert_eq!(linear_to_ulaw(-32635), 0x00);
    }

    #[test]
    fn roundtrip_error_within_quantization() {
        for s in (-32000..32000).step_by(17) {
            let s = s as i16;
            let rt = ulaw_to_linear(linear_to_ulaw(s));
            // µ-law worst-case quantization error grows with amplitude;
            // top segment step is 1024, so half-step ≈ 512 plus bias slop.
            let err = (rt as i32 - s as i32).abs();
            assert!(err <= 600, "sample {s} roundtripped to {rt} (err {err})");
        }
    }

    #[test]
    fn encode_is_monotonic_in_magnitude() {
        // Decoded values must be non-decreasing as input grows.
        let mut prev = ulaw_to_linear(linear_to_ulaw(-32700));
        for s in (-32700..32700).step_by(50) {
            let cur = ulaw_to_linear(linear_to_ulaw(s as i16));
            assert!(cur >= prev, "non-monotonic at {s}: {cur} < {prev}");
            prev = cur;
        }
    }

    #[test]
    fn silence_constant_matches() {
        assert_eq!(linear_to_ulaw(0), ULAW_SILENCE);
    }
}
