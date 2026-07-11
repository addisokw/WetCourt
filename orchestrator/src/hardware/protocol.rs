use std::fmt;

#[derive(Debug, Clone)]
pub enum PanelPattern {
    Idle,
    Thinking,
    #[allow(dead_code)]
    Verdict,
}
impl fmt::Display for PanelPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PanelPattern::Idle => "idle",
            PanelPattern::Thinking => "thinking",
            PanelPattern::Verdict => "verdict",
        })
    }
}

/// Eye phase for the LED-matrix judge face (`FACE <phase>` on the wire — the
/// firmware's native vocabulary, superseding the legacy `PANEL` patterns).
/// Matches `firmware/judge-face/eye_face.py` PHASES.
#[derive(Debug, Clone)]
pub enum FacePhase {
    Idle,
    Listening,
    Deliberating,
    VerdictGuilty,
    VerdictInnocent,
}
impl fmt::Display for FacePhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FacePhase::Idle => "idle",
            FacePhase::Listening => "listening",
            FacePhase::Deliberating => "deliberating",
            FacePhase::VerdictGuilty => "verdict:guilty",
            FacePhase::VerdictInnocent => "verdict:innocent",
        })
    }
}
impl FacePhase {
    pub fn verdict(guilty: bool) -> Self {
        if guilty { FacePhase::VerdictGuilty } else { FacePhase::VerdictInnocent }
    }
}

#[derive(Debug, Clone)]
pub enum HardwareCommand {
    Fire(u32),
    /// Strike with firmware-default geometry (bare `GAVEL`). The FSM emits this;
    /// the host adapter rewrites it to `GavelStrike` from `gavel.toml` when a
    /// `[gavel]` calibration exists, so real strikes honour the tuned values.
    Gavel,
    /// Strike with host-supplied geometry — all seven values are sent on the wire
    /// (servo µs positions + dwell ms + rap count) so the firmware stays
    /// stateless, like the turret's `AIM`. Built from `gavel.toml` (trials) or
    /// the console form.
    GavelStrike {
        rest: i32,
        raise: i32,
        strike: i32,
        raise_dwell_ms: u32,
        strike_dwell_ms: u32,
        settle_dwell_ms: u32,
        strikes: u32,
    },
    /// Move the gavel servo to a raw pulse-width (µs) and hold — the console's
    /// live position preview while tuning.
    GavelJog(i32),
    /// Point a pan/tilt mechanism. Values are *raw* device units — the host
    /// applies per-device calibration (degrees → raw) before building this.
    /// Owned by the `turret` and `judge-neck` roles (see protocol spec).
    Aim { pan: i32, tilt: i32 },
    /// The neck pose in *degrees*, mirrored to the `judge-face` so the eye's
    /// catchlight can counter-move (specular parallax). Same `AIM` verb on the
    /// wire; degrees because the face has no servo calibration to invert.
    FaceAim { pan: f32, tilt: f32 },
    Panel(PanelPattern),
    /// Set the LED-matrix eye phase — the trial's face choreography.
    Face(FacePhase),
    /// Switch the LED-matrix eye's persona theme (an eye-theme slug from
    /// `firmware/judge-face/personas.py`, carried by the orchestrator persona's
    /// `face_persona` field — not the orchestrator persona id).
    Persona(String),
    Ping,
}

impl HardwareCommand {
    pub fn to_line(&self) -> String {
        match self {
            HardwareCommand::Fire(ms) => format!("FIRE {ms}"),
            HardwareCommand::Gavel => "GAVEL".into(),
            HardwareCommand::GavelStrike {
                rest,
                raise,
                strike,
                raise_dwell_ms,
                strike_dwell_ms,
                settle_dwell_ms,
                strikes,
            } => format!(
                "GAVEL {rest} {raise} {strike} {raise_dwell_ms} {strike_dwell_ms} {settle_dwell_ms} {strikes}"
            ),
            HardwareCommand::GavelJog(us) => format!("GJOG {us}"),
            HardwareCommand::Aim { pan, tilt } => format!("AIM {pan} {tilt}"),
            HardwareCommand::FaceAim { pan, tilt } => format!("AIM {pan:.1} {tilt:.1}"),
            HardwareCommand::Panel(p) => format!("PANEL {p}"),
            HardwareCommand::Face(p) => format!("FACE {p}"),
            HardwareCommand::Persona(slug) => format!("PERSONA {slug}"),
            HardwareCommand::Ping => "PING".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gavel_strike_serialises_all_seven() {
        let line = HardwareCommand::GavelStrike {
            rest: 1500,
            raise: 2000,
            strike: 1100,
            raise_dwell_ms: 180,
            strike_dwell_ms: 120,
            settle_dwell_ms: 160,
            strikes: 3,
        }
        .to_line();
        assert_eq!(line, "GAVEL 1500 2000 1100 180 120 160 3");
    }

    #[test]
    fn gavel_jog_serialises() {
        assert_eq!(HardwareCommand::GavelJog(1750).to_line(), "GJOG 1750");
    }

    #[test]
    fn bare_gavel_is_unqualified() {
        assert_eq!(HardwareCommand::Gavel.to_line(), "GAVEL");
    }

    #[test]
    fn face_phases_use_firmware_vocabulary() {
        assert_eq!(HardwareCommand::Face(FacePhase::Listening).to_line(), "FACE listening");
        assert_eq!(HardwareCommand::Face(FacePhase::verdict(true)).to_line(), "FACE verdict:guilty");
        assert_eq!(
            HardwareCommand::Face(FacePhase::verdict(false)).to_line(),
            "FACE verdict:innocent"
        );
    }

    #[test]
    fn persona_serialises_slug() {
        assert_eq!(HardwareCommand::Persona("cosmic".into()).to_line(), "PERSONA cosmic");
    }
}
