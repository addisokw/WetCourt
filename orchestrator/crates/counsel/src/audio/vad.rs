//! Energy (RMS) voice-activity endpointing over 20 ms frames. The handset
//! is a close mic in a quiet-ish booth, so an amplitude gate with hangover
//! does the job; if the venue proves it wrong, this trait boundary is where
//! a Silero model drops in.

use crate::config::AudioConfig;
use crate::rtp::SAMPLES_PER_FRAME;

const FRAME_MS: u64 = 20;

pub trait Vad: Send {
    /// Feed one 20 ms frame; returns a finished utterance when endpointed.
    fn push(&mut self, frame: &[i16]) -> Option<Vec<i16>>;
    /// Frames observed since the last utterance ended (silence gauge).
    fn idle_ms(&self) -> u64;
    fn reset(&mut self);
}

pub struct EnergyVad {
    threshold: f32,
    start_frames: u32,
    hangover_frames: u32,
    preroll_frames: usize,
    min_frames: usize,
    max_frames: usize,
    debug_rms: bool,

    preroll: Vec<Vec<i16>>,
    collecting: Option<Collecting>,
    consecutive_speech: u32,
    idle_frames: u64,
}

struct Collecting {
    samples: Vec<i16>,
    frames: usize,
    silent_run: u32,
}

impl EnergyVad {
    pub fn new(cfg: &AudioConfig) -> Self {
        Self {
            threshold: cfg.vad_rms_threshold,
            start_frames: cfg.vad_start_frames,
            hangover_frames: (cfg.vad_hangover_ms / FRAME_MS).max(1) as u32,
            preroll_frames: (cfg.vad_preroll_ms / FRAME_MS).max(1) as usize,
            min_frames: (cfg.min_utterance_ms / FRAME_MS).max(1) as usize,
            max_frames: (cfg.max_utterance_ms / FRAME_MS).max(1) as usize,
            debug_rms: cfg.debug_rms,
            preroll: Vec::new(),
            collecting: None,
            consecutive_speech: 0,
            idle_frames: 0,
        }
    }

    fn rms(frame: &[i16]) -> f32 {
        if frame.is_empty() {
            return 0.0;
        }
        // Remove DC before measuring — telephony paths can ride an offset.
        let mean: f32 = frame.iter().map(|&s| s as f32).sum::<f32>() / frame.len() as f32;
        let sq: f32 = frame
            .iter()
            .map(|&s| (s as f32 - mean).powi(2))
            .sum::<f32>()
            / frame.len() as f32;
        sq.sqrt()
    }
}

impl Vad for EnergyVad {
    fn push(&mut self, frame: &[i16]) -> Option<Vec<i16>> {
        let rms = Self::rms(frame);
        let is_speech = rms >= self.threshold;
        if self.debug_rms {
            tracing::debug!(rms = %rms as u32, speech = is_speech, "vad frame");
        }

        match self.collecting.as_mut() {
            None => {
                self.idle_frames += 1;
                self.preroll.push(frame.to_vec());
                if self.preroll.len() > self.preroll_frames {
                    self.preroll.remove(0);
                }
                if is_speech {
                    self.consecutive_speech += 1;
                    if self.consecutive_speech >= self.start_frames {
                        let mut samples =
                            Vec::with_capacity(SAMPLES_PER_FRAME * self.max_frames);
                        for f in self.preroll.drain(..) {
                            samples.extend(f);
                        }
                        let frames = samples.len() / SAMPLES_PER_FRAME;
                        self.collecting = Some(Collecting { samples, frames, silent_run: 0 });
                        self.consecutive_speech = 0;
                    }
                } else {
                    self.consecutive_speech = 0;
                }
                None
            }
            Some(c) => {
                c.samples.extend_from_slice(frame);
                c.frames += 1;
                c.silent_run = if is_speech { 0 } else { c.silent_run + 1 };

                let ended = c.silent_run >= self.hangover_frames || c.frames >= self.max_frames;
                if !ended {
                    return None;
                }
                let done = self.collecting.take().unwrap();
                self.idle_frames = 0;
                let voiced_frames = done.frames.saturating_sub(done.silent_run as usize);
                if voiced_frames < self.min_frames {
                    tracing::debug!(frames = voiced_frames, "utterance too short, dropped");
                    return None;
                }
                Some(done.samples)
            }
        }
    }

    fn idle_ms(&self) -> u64 {
        self.idle_frames * FRAME_MS
    }

    fn reset(&mut self) {
        self.preroll.clear();
        self.collecting = None;
        self.consecutive_speech = 0;
        self.idle_frames = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AudioConfig {
        AudioConfig {
            vad_rms_threshold: 500.0,
            vad_start_frames: 3,
            vad_hangover_ms: 200, // 10 frames
            vad_preroll_ms: 100,  // 5 frames
            min_utterance_ms: 100,
            max_utterance_ms: 2000,
            ..Default::default()
        }
    }

    fn loud() -> Vec<i16> {
        (0..SAMPLES_PER_FRAME)
            .map(|i| if i % 2 == 0 { 3000 } else { -3000 })
            .collect()
    }

    fn quiet() -> Vec<i16> {
        vec![10; SAMPLES_PER_FRAME]
    }

    #[test]
    fn endpointing_with_preroll_and_hangover() {
        let mut vad = EnergyVad::new(&cfg());
        // Silence, then speech.
        for _ in 0..20 {
            assert!(vad.push(&quiet()).is_none());
        }
        for _ in 0..25 {
            assert!(vad.push(&loud()).is_none());
        }
        // Trailing silence closes it after 10 hangover frames.
        let mut got = None;
        for _ in 0..12 {
            if let Some(u) = vad.push(&quiet()) {
                got = Some(u);
                break;
            }
        }
        let utterance = got.expect("utterance endpointed");
        // Preroll (≤5) + speech (25) + hangover (10) frames of samples.
        let frames = utterance.len() / SAMPLES_PER_FRAME;
        assert!((30..=42).contains(&frames), "unexpected frame count {frames}");
    }

    #[test]
    fn short_blips_are_dropped() {
        let mut vad = EnergyVad::new(&cfg());
        for _ in 0..3 {
            vad.push(&loud()); // trigger (3 start frames), then immediate stop
        }
        let mut emitted = false;
        for _ in 0..15 {
            if vad.push(&quiet()).is_some() {
                emitted = true;
            }
        }
        assert!(!emitted, "sub-min utterance should be dropped");
    }

    #[test]
    fn two_start_frames_do_not_trigger() {
        let mut vad = EnergyVad::new(&cfg());
        for _ in 0..2 {
            assert!(vad.push(&loud()).is_none());
        }
        for _ in 0..20 {
            assert!(vad.push(&quiet()).is_none());
        }
        assert!(vad.idle_ms() > 0);
    }

    #[test]
    fn max_length_force_closes() {
        let mut vad = EnergyVad::new(&cfg());
        let mut got = None;
        for _ in 0..150 {
            if let Some(u) = vad.push(&loud()) {
                got = Some(u);
                break;
            }
        }
        let u = got.expect("force-closed at max_utterance");
        assert!(u.len() / SAMPLES_PER_FRAME <= 105);
    }

    #[test]
    fn idle_counter_tracks_silence() {
        let mut vad = EnergyVad::new(&cfg());
        for _ in 0..50 {
            vad.push(&quiet());
        }
        assert_eq!(vad.idle_ms(), 1000);
    }
}
