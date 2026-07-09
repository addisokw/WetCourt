//! Minimal WAV writer: 8 kHz mono s16le with the classic 44-byte header.
//! Parakeet's server decodes via soundfile and resamples itself, so this is
//! all the container it needs.

pub fn wrap_pcm_8k(samples: &[i16]) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let sample_rate: u32 = 8000;
    let byte_rate = sample_rate * 2; // mono, 16-bit
    let mut out = Vec::with_capacity(44 + data_len);

    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_fields() {
        let wav = wrap_pcm_8k(&[0i16; 800]);
        assert_eq!(wav.len(), 44 + 1600);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 8000);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 1600);
    }
}
