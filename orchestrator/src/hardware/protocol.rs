use std::fmt;

#[derive(Debug, Clone)]
pub enum LightState {
    SplashIdle,
    SplashArming,
    Guilty,
    NotGuilty,
}
impl fmt::Display for LightState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            LightState::SplashIdle => "splash_idle",
            LightState::SplashArming => "splash_arming",
            LightState::Guilty => "guilty",
            LightState::NotGuilty => "not_guilty",
        })
    }
}

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

#[derive(Debug, Clone)]
pub enum HardwareCommand {
    Fire(u32),
    Gavel,
    /// Point a pan/tilt mechanism. Values are *raw* device units — the host
    /// applies per-device calibration (degrees → raw) before building this.
    /// Owned by the `turret` and `ai-judge` roles (see protocol spec).
    Aim { pan: i32, tilt: i32 },
    Lights(LightState),
    Panel(PanelPattern),
    Ping,
}

impl HardwareCommand {
    pub fn to_line(&self) -> String {
        match self {
            HardwareCommand::Fire(ms) => format!("FIRE {ms}"),
            HardwareCommand::Gavel => "GAVEL".into(),
            HardwareCommand::Aim { pan, tilt } => format!("AIM {pan} {tilt}"),
            HardwareCommand::Lights(s) => format!("LIGHTS {s}"),
            HardwareCommand::Panel(p) => format!("PANEL {p}"),
            HardwareCommand::Ping => "PING".into(),
        }
    }
}
