use bytes::Bytes;

use crate::display::events::DisplayEvent;
use crate::hardware::protocol::HardwareCommand;
use crate::state_machine::states::CrossExam;

#[derive(Debug)]
pub enum Command {
    GenerateCharge,
    Transcribe(Vec<u8>),
    /// Generate the judge's one cross-examination follow-up question.
    CrossExamine { charge: String, plea: String },
    Deliberate { charge: String, plea: String, cross: Option<CrossExam> },
    Speak(String),

    Hardware(HardwareCommand),
    Display(DisplayEvent),
    /// Raw binary frame to push down the WebSocket — typically a TTS audio
    /// chunk, preceded by a `DisplayEvent::TtsAudio` JSON header.
    DisplayBinary(Bytes),
}
