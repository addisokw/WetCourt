//! Energy (RMS) voice-activity endpointing over 20 ms frames, with an
//! adaptive threshold: speech must stand `floor_ratio` above the rolling
//! noise floor (minimum frame RMS over the last few seconds). A fixed gate
//! proved useless at a loud venue — the ambient floor sat 3–7× above any
//! threshold tuned in a quiet room, so utterances only ever ended at the
//! max_utterance force-close. If energy proves insufficient outright, this
//! trait boundary is where a Silero model drops in.

use std::collections::VecDeque;

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
    min_threshold: f32,
    floor_ratio: f32,
    floor_window: usize,
    start_frames: u32,
    hangover_frames: u32,
    preroll_frames: usize,
    min_frames: usize,
    max_frames: usize,
    debug_rms: bool,

    /// Recent frame RMS values (up to `floor_window`); their minimum is the
    /// noise-floor estimate. Survives `reset()` on purpose — the floor is a
    /// property of the line/venue, not of one turn.
    recent_rms: VecDeque<f32>,
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
            min_threshold: cfg.vad_rms_threshold,
            floor_ratio: cfg.vad_floor_ratio.max(1.0),
            floor_window: (cfg.vad_floor_window_ms / FRAME_MS).max(1) as usize,
            start_frames: cfg.vad_start_frames,
            hangover_frames: (cfg.vad_hangover_ms / FRAME_MS).max(1) as u32,
            preroll_frames: (cfg.vad_preroll_ms / FRAME_MS).max(1) as usize,
            min_frames: (cfg.min_utterance_ms / FRAME_MS).max(1) as usize,
            max_frames: (cfg.max_utterance_ms / FRAME_MS).max(1) as usize,
            debug_rms: cfg.debug_rms,
            recent_rms: VecDeque::new(),
            preroll: Vec::new(),
            collecting: None,
            consecutive_speech: 0,
            idle_frames: 0,
        }
    }

    /// Threshold this frame must beat: the configured minimum, or the ratio
    /// over the tracked noise floor, whichever is higher. Also feeds `rms`
    /// into the floor window — the floor reacts to the venue getting quieter
    /// within one window, and speech's inter-word dips keep it honest while
    /// someone talks.
    fn effective_threshold(&mut self, rms: f32) -> f32 {
        self.recent_rms.push_back(rms);
        if self.recent_rms.len() > self.floor_window {
            self.recent_rms.pop_front();
        }
        let floor = self
            .recent_rms
            .iter()
            .copied()
            .fold(f32::INFINITY, f32::min);
        self.min_threshold.max(floor * self.floor_ratio)
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
        let threshold = self.effective_threshold(rms);
        let is_speech = rms >= threshold;
        if self.debug_rms {
            tracing::debug!(
                rms = %rms as u32,
                thr = %threshold as u32,
                speech = is_speech,
                "vad frame"
            );
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
        // 10 frames min: the 5-frame preroll (quiet, but counted as voiced)
        // plus a 3-frame blip must still land under the bar.
        let mut c = cfg();
        c.min_utterance_ms = 200;
        let mut vad = EnergyVad::new(&c);
        for _ in 0..10 {
            vad.push(&quiet()); // establish the floor
        }
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
        for _ in 0..10 {
            vad.push(&quiet());
        }
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
        for _ in 0..10 {
            vad.push(&quiet());
        }
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

    /// The loud-venue regression: a constant ambient floor far above the
    /// configured minimum threshold must not stall endpointing — the
    /// adaptive floor has to let speech trigger AND let the hangover close
    /// the utterance when speech stops (with a fixed gate this ran to the
    /// max_utterance force-close every time).
    #[test]
    fn noisy_venue_still_endpoints() {
        let mut c = cfg();
        c.vad_rms_threshold = 100.0; // the live-tuned quiet-room value
        let mut vad = EnergyVad::new(&c);
        // Venue noise at RMS ~400: 4× the fixed threshold.
        let noise: Vec<i16> = (0..SAMPLES_PER_FRAME)
            .map(|i| if i % 2 == 0 { 400 } else { -400 })
            .collect();
        for _ in 0..50 {
            assert!(vad.push(&noise).is_none(), "ambient must not trigger");
        }
        assert!(vad.idle_ms() > 0, "noise frames must count as idle");
        for _ in 0..25 {
            assert!(vad.push(&loud()).is_none());
        }
        // Back to ambient: hangover (10 frames) must close it.
        let mut got = None;
        for _ in 0..12 {
            if let Some(u) = vad.push(&noise) {
                got = Some(u);
                break;
            }
        }
        let u = got.expect("utterance endpointed over venue noise");
        let frames = u.len() / SAMPLES_PER_FRAME;
        assert!((30..=42).contains(&frames), "unexpected frame count {frames}");
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
