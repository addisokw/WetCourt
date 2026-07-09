//! Per-call recording: both legs of the conversation plus an annotated
//! event log, written on hangup as a shareable pair —
//!   <dir>/<timestamp>-<kind>.wav   stereo 8 kHz, caller + lawyer overlaid
//!   <dir>/<timestamp>-<kind>.json  IVR outcome, transcripts, replies, timings
//! The RTP tasks feed audio; the agent annotates. Everything is in-memory
//! until finalize (a 5-minute call is ~10 MB), so a crash loses the call —
//! acceptable for an art piece.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Context, Result};
use serde_json::{json, Value};

pub struct CallRecorder {
    kind: &'static str,
    remote: String,
    started: Instant,
    started_wall: String,
    caller: Mutex<Vec<i16>>,
    lawyer: Mutex<Vec<i16>>,
    events: Mutex<Vec<Value>>,
}

impl CallRecorder {
    pub fn new(kind: &'static str, remote: String) -> Self {
        Self {
            kind,
            remote,
            started: Instant::now(),
            started_wall: chrono::Local::now().format("%Y%m%d-%H%M%S").to_string(),
            caller: Mutex::new(Vec::new()),
            lawyer: Mutex::new(Vec::new()),
            events: Mutex::new(Vec::new()),
        }
    }

    pub fn push_caller(&self, samples: &[i16]) {
        self.caller.lock().unwrap().extend_from_slice(samples);
    }

    pub fn push_lawyer(&self, samples: &[i16]) {
        self.lawyer.lock().unwrap().extend_from_slice(samples);
    }

    /// Annotate the timeline (transcripts, replies, IVR keys, ...).
    pub fn note(&self, kind: &str, detail: impl Into<Value>) {
        let t = self.started.elapsed().as_secs_f32();
        self.events.lock().unwrap().push(json!({
            "t": (t * 10.0).round() / 10.0,
            "kind": kind,
            "detail": detail.into(),
        }));
    }

    /// Write the WAV + JSON pair. Returns the WAV path.
    pub fn finalize(&self, dir: &Path) -> Result<PathBuf> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating recording dir {}", dir.display()))?;
        let stem = format!("{}-{}", self.started_wall, self.kind);

        // Overlay both legs: sum caller + lawyer per sample (clamped), then
        // duplicate that mix to both channels so it plays like a normal call
        // recording — both voices in both ears. They rarely overlap (no
        // barge-in), so clipping on the sum is a non-issue in practice.
        let caller = self.caller.lock().unwrap();
        let lawyer = self.lawyer.lock().unwrap();
        let frames = caller.len().max(lawyer.len());
        let mut interleaved = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let mixed = (caller.get(i).copied().unwrap_or(0) as i32
                + lawyer.get(i).copied().unwrap_or(0) as i32)
                .clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            interleaved.push(mixed); // left
            interleaved.push(mixed); // right
        }

        let wav_path = dir.join(format!("{stem}.wav"));
        std::fs::write(&wav_path, stereo_wav_8k(&interleaved))
            .with_context(|| format!("writing {}", wav_path.display()))?;

        let sidecar = json!({
            "kind": self.kind,
            "remote": self.remote,
            "started": self.started_wall,
            "duration_secs": (self.started.elapsed().as_secs_f32() * 10.0).round() / 10.0,
            "mix": "caller + lawyer overlaid (both channels)",
            "events": *self.events.lock().unwrap(),
        });
        let json_path = dir.join(format!("{stem}.json"));
        std::fs::write(&json_path, serde_json::to_vec_pretty(&sidecar)?)
            .with_context(|| format!("writing {}", json_path.display()))?;

        tracing::info!(wav = %wav_path.display(), "call recorded");
        Ok(wav_path)
    }
}

/// 44-byte-header stereo 8 kHz s16le WAV around interleaved L/R samples.
fn stereo_wav_8k(interleaved: &[i16]) -> Vec<u8> {
    let data_len = interleaved.len() * 2;
    let sample_rate: u32 = 8000;
    let block_align: u16 = 4; // stereo s16
    let byte_rate = sample_rate * block_align as u32;
    let mut out = Vec::with_capacity(44 + data_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes()); // stereo
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in interleaved {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalize_writes_overlaid_stereo_and_pads_shorter_leg() {
        let dir = std::env::temp_dir().join("counsel-rec-test");
        let rec = CallRecorder::new("inbound", "test".into());
        rec.push_caller(&[100; 800]); // 0.1 s
        rec.push_lawyer(&[200; 1600]); // 0.2 s — caller leg padded with 0s
        rec.note("caller", "hello");
        let wav = rec.finalize(&dir).unwrap();
        let bytes = std::fs::read(&wav).unwrap();
        // 1600 frames * 2ch * 2B + 44 header
        assert_eq!(bytes.len(), 44 + 1600 * 4);
        assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 2);
        // First sample: both legs present → 100 + 200 = 300, duplicated L/R.
        let l = i16::from_le_bytes([bytes[44], bytes[45]]);
        let r = i16::from_le_bytes([bytes[46], bytes[47]]);
        assert_eq!((l, r), (300, 300));
        // Past the caller leg: only lawyer → 0 + 200 = 200, still both channels.
        let off = 44 + 900 * 4;
        assert_eq!(i16::from_le_bytes([bytes[off], bytes[off + 1]]), 200);
        let sidecar = wav.with_extension("json");
        let j: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&sidecar).unwrap()).unwrap();
        assert_eq!(j["events"][0]["kind"], "caller");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn overlay_clamps_instead_of_wrapping() {
        let dir = std::env::temp_dir().join("counsel-rec-clamp");
        let rec = CallRecorder::new("inbound", "test".into());
        rec.push_caller(&[30000; 10]);
        rec.push_lawyer(&[30000; 10]); // sum 60000 must clamp, not wrap
        let wav = rec.finalize(&dir).unwrap();
        let bytes = std::fs::read(&wav).unwrap();
        assert_eq!(i16::from_le_bytes([bytes[44], bytes[45]]), i16::MAX);
        std::fs::remove_dir_all(&dir).ok();
    }
}
