use super::states::Verdict;

#[derive(Debug)]
pub enum Event {
    OperatorStart,
    /// The defendant pressed the swear-in arcade button (a wire `BUTTON`).
    /// From `Idle` it starts a trial exactly like `OperatorStart`; during an
    /// open plea/answer window it means "I'm done talking" and closes the
    /// window early. Every other state ignores it.
    DefendantButton,
    OperatorEmergencyStop,

    /// Enter the maintenance/test plane (only honoured from `Idle`).
    EnterMaintenance,
    /// Leave maintenance, returning to `Idle`.
    ExitMaintenance,

    ChargeReady(String),
    PleaAudioReceived(Vec<u8>),
    PleaRecordingStarted,
    TranscriptReady(String),
    /// Test hook (`POST /operator/test/inject_plea`): feed a canned plea/answer
    /// transcript straight into the plea or cross-answer window, bypassing the
    /// mic + STT. For driving a full trial when no booth mic is available.
    InjectPlea(String),
    /// STT errored (distinct from a silent defendant) — the FSM falls back to
    /// "[no defense offered]" AND raises the operator PleaFallback banner.
    TranscriptFailed(String),
    CrossQuestionReady(String),
    /// Question generation failed/empty (logged at the source) — cross-exam is
    /// skipped and the trial proceeds straight to deliberation.
    CrossQuestionFailed,
    VerdictReady(Verdict),
    /// The pre_announced verdict service reached its reveal beat (verdict word
    /// about to play). The FSM strikes the gavel here — through the hardware
    /// adapter, so the strike honours the console-tuned gavel.toml geometry.
    VerdictRevealed,
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
