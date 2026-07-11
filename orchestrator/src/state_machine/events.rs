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
    PleaAudioReceived(Vec<u8>),
    PleaRecordingStarted,
    TranscriptReady(String),
    /// STT errored (distinct from a silent defendant) — the FSM falls back to
    /// "[no defense offered]" AND raises the operator PleaFallback banner.
    TranscriptFailed(String),
    CrossQuestionReady(String),
    /// Question generation failed/empty (logged at the source) — cross-exam is
    /// skipped and the trial proceeds straight to deliberation.
    CrossQuestionFailed,
    VerdictReady(Verdict),
    TtsFinished,
    /// Payload is the raw ack/err line off the wire — nothing branches on it,
    /// but it shows up in event debugging; kept deliberately.
    HardwareAck(#[allow(dead_code)] String),
    HardwareError(#[allow(dead_code)] String),

    /// The defendant picked up the lawyer phone (a counsel call went live).
    /// Pauses the plea / cross-answer countdown while they consult.
    LawyerCallStarted,
    /// The lawyer call ended — resume the paused countdown.
    LawyerCallEnded,

    Tick,
}
