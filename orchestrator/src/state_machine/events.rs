use super::states::Verdict;

#[derive(Debug)]
pub enum Event {
    OperatorStart,
    OperatorEmergencyStop,

    /// Enter the maintenance/test plane (only honoured from `Idle`).
    EnterMaintenance,
    /// Leave maintenance, returning to `Idle`.
    ExitMaintenance,

    ChargeReady(String),
    #[allow(dead_code)]
    ChargeFailed(String),
    PleaAudioReceived(Vec<u8>),
    PleaRecordingStarted,
    PleaTimeout,
    TranscriptReady(String),
    /// STT errored (distinct from a silent defendant) — the FSM falls back to
    /// "[no defense offered]" AND raises the operator PleaFallback banner.
    TranscriptFailed(String),
    CrossQuestionReady(String),
    #[allow(dead_code)]
    CrossQuestionFailed(String),
    VerdictReady(Verdict),
    #[allow(dead_code)]
    VerdictFailed(String),
    TtsFinished,
    HardwareAck(String),
    HardwareError(String),

    Tick,
}
