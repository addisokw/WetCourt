//! Latency-cover audio: 8 kHz mono s16 WAV assets, µ-law-encoded once at
//! startup and looped by the mixer while inference runs. Missing assets are
//! non-fatal — the line just falls back to silence.

use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};

use crate::rtp::g711;

pub struct CoverAssets {
    /// "Office ambience": keyboard clatter / hold music while thinking.
    pub thinking: Option<Arc<Vec<u8>>>,
    /// IVR greeting recording, played before the lawyer picks up.
    pub ivr_prompt: Option<Arc<Vec<u8>>>,
    /// Hold-gag reserve ("let me put you on a brief hold") — loaded and
    /// ready; nothing plays it yet.
    #[allow(dead_code)]
    pub hold_music: Option<Arc<Vec<u8>>>,
}

impl CoverAssets {
    pub fn load(dir: &Path) -> Self {
        let load = |name: &str| match load_ulaw_8k(&dir.join(name)) {
            Ok(u) => {
                tracing::info!(asset = name, bytes = u.len(), "cover asset loaded");
                Some(Arc::new(u))
            }
            Err(e) => {
                tracing::warn!(asset = name, "cover asset unavailable: {e:#}");
                None
            }
        };
        Self {
            thinking: load("keyboard_clatter_8k.wav"),
            ivr_prompt: load("ivr_prompt_8k.wav"),
            hold_music: load("hold_music_8k.wav"),
        }
    }
}

/// Read an 8 kHz mono s16le WAV and µ-law encode it. Strict on format —
/// these are our own generated assets, not arbitrary uploads.
pub fn load_ulaw_8k(path: &Path) -> Result<Vec<u8>> {
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if data.len() < 44 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        bail!("not a RIFF/WAVE file");
    }
    let channels = u16::from_le_bytes([data[22], data[23]]);
    let rate = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
    let bits = u16::from_le_bytes([data[34], data[35]]);
    if channels != 1 || rate != 8000 || bits != 16 {
        bail!("expected 8 kHz mono s16 (got {channels}ch {rate}Hz {bits}bit) — regenerate with: ffmpeg -i in -ar 8000 -ac 1 -sample_fmt s16 out.wav");
    }
    // Find the data chunk (fmt may be followed by LIST etc.).
    let mut pos = 12;
    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let len = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if id == b"data" {
            let end = (pos + 8 + len).min(data.len());
            let samples: Vec<i16> = data[pos + 8..end]
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect();
            if samples.is_empty() {
                bail!("empty data chunk");
            }
            return Ok(g711::encode(&samples));
        }
        pos += 8 + len + (len & 1);
    }
    bail!("no data chunk found");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::wav::wrap_pcm_8k;

    #[test]
    fn roundtrip_through_our_own_wav() {
        let dir = std::env::temp_dir();
        let path = dir.join("counsel_cover_test.wav");
        let samples: Vec<i16> = (0..1600).map(|i| ((i % 100) * 300 - 15000) as i16).collect();
        std::fs::write(&path, wrap_pcm_8k(&samples)).unwrap();
        let ulaw = load_ulaw_8k(&path).unwrap();
        assert_eq!(ulaw.len(), samples.len());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rejects_wrong_rate() {
        let dir = std::env::temp_dir();
        let path = dir.join("counsel_cover_bad.wav");
        let mut wav = wrap_pcm_8k(&[0i16; 100]);
        wav[24..28].copy_from_slice(&16000u32.to_le_bytes());
        std::fs::write(&path, wav).unwrap();
        assert!(load_ulaw_8k(&path).is_err());
        std::fs::remove_file(&path).ok();
    }
}
