use crate::display::events::DisplayEvent;
use crate::hardware::protocol::HardwareCommand;

#[derive(Debug)]
pub enum Command {
    GenerateCharge,
    Transcribe(Vec<f32>),
    Deliberate { charge: String, plea: String },
    Speak(String),

    Hardware(HardwareCommand),
    Display(DisplayEvent),
}
