use bytes::Bytes;

use crate::display::events::DisplayEvent;
use crate::hardware::protocol::HardwareCommand;

#[derive(Debug)]
pub enum Command {
    GenerateCharge,
    Transcribe(Vec<u8>),
    Deliberate { charge: String, plea: String },
    Speak(String),

    Hardware(HardwareCommand),
    Display(DisplayEvent),
    /// Raw binary frame to push down the WebSocket — typically a TTS audio
    /// chunk, preceded by a `DisplayEvent::TtsAudio` JSON header.
    DisplayBinary(Bytes),
}
