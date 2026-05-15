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
    Lights(LightState),
    Panel(PanelPattern),
    Ping,
}

impl HardwareCommand {
    pub fn to_line(&self) -> String {
        match self {
            HardwareCommand::Fire(ms) => format!("FIRE {ms}"),
            HardwareCommand::Gavel => "GAVEL".into(),
            HardwareCommand::Lights(s) => format!("LIGHTS {s}"),
            HardwareCommand::Panel(p) => format!("PANEL {p}"),
            HardwareCommand::Ping => "PING".into(),
        }
    }
}
