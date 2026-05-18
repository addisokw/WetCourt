use super::states::Verdict;

#[derive(Debug)]
pub enum Event {
    OperatorStart,
    OperatorEmergencyStop,
    /// Context-aware "Plea" trigger from the operator (UE hotkey, browser,
    /// future hardware button): from DisplayingCharge it cuts the charge-
    /// display dwell short and starts plea capture; from AwaitingPlea it
    /// ends plea capture early (same path as PleaTimeout). Ignored in any
    /// other state.
    OperatorPlea,

    ChargeReady(String),
    #[allow(dead_code)]
    ChargeFailed(String),
    PleaAudioReceived(Vec<u8>),
    PleaTimeout,
    TranscriptReady(String),
    #[allow(dead_code)]
    TranscriptFailed(String),
    VerdictReady(Verdict),
    #[allow(dead_code)]
    VerdictFailed(String),
    TtsFinished,
    HardwareAck(String),
    HardwareError(String),

    Tick,
}
