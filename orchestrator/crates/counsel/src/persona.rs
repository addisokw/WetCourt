//! The lawyer persona. Deliberately its own tiny format rather than the
//! booth's `Persona` — no guilty_bias, no judging engine, plus phone-specific
//! canned lines (greeting/reprompt/signoff/fallbacks).

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct LawyerPersona {
    pub id: String,
    pub display_name: String,
    /// Kokoro voice — keep it clean and distinct from the judge's
    /// robot-processed one; the 8 kHz phone path is the effect.
    pub tts_voice: String,
    #[serde(default)]
    pub tts_speed: Option<f32>,
    /// Spoken when the lawyer picks up.
    pub greeting: String,
    /// Post-IVR hold announcement, read by `hold_voice` over the hold music;
    /// `{n}` is replaced with a random queue number. `None` (or a missing
    /// hold_music asset) skips the hold gag.
    #[serde(default)]
    pub hold_line: Option<String>,
    /// The office/IVR voice — distinct from the lawyer's.
    #[serde(default = "d_hold_voice")]
    pub hold_voice: String,
    /// Spoken after a long client silence.
    pub reprompt: String,
    /// Spoken before the lawyer hangs up (max call length / dead line).
    pub signoff: String,
    /// The exchange-cap exits: scripted mishaps that befall the lawyer and
    /// force him off the line ("the copier's got my tie—"), one picked per
    /// call. Empty → the plain `signoff` is used instead.
    #[serde(default)]
    pub hangup_lines: Vec<String>,
    /// In-character lines for inference failures, cycled.
    pub fallback_lines: Vec<String>,
    pub system_prompt: String,
}

fn d_hold_voice() -> String {
    "af_sarah".into()
}

impl LawyerPersona {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading persona {}", path.display()))?;
        let p: LawyerPersona = toml::from_str(&raw)
            .with_context(|| format!("parsing persona {}", path.display()))?;
        anyhow::ensure!(!p.system_prompt.is_empty(), "persona system_prompt is empty");
        anyhow::ensure!(!p.tts_voice.is_empty(), "persona tts_voice is empty");
        anyhow::ensure!(!p.fallback_lines.is_empty(), "persona needs fallback_lines");
        Ok(p)
    }

    pub fn fallback(&self, n: usize) -> &str {
        &self.fallback_lines[n % self.fallback_lines.len()]
    }

    /// The exchange-cap exit line; `n` varies the pick per call.
    pub fn hangup(&self, n: usize) -> &str {
        if self.hangup_lines.is_empty() {
            &self.signoff
        } else {
            &self.hangup_lines[n % self.hangup_lines.len()]
        }
    }
}
