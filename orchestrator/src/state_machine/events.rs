use super::states::Verdict;

#[derive(Debug)]
pub enum Event {
    OperatorStart,
    OperatorEmergencyStop,

    ChargeReady(String),
    #[allow(dead_code)]
    ChargeFailed(String),
    PleaAudioReceived(Vec<u8>),
    PleaRecordingStarted,
    PleaTimeout,
    TranscriptReady(String),
    #[allow(dead_code)]
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
