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
    /// Drive the trial's vision-targeting sequence (arm/freeze/idle). Executed by
    /// the `TargetingController` in the Runtime; a no-op when unconfigured.
    Targeting(TargetingCue),
}

/// One step of the trial's turret-aiming choreography.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetingCue {
    /// Reset the aim to center and arm, so vision sweeps the gun onto the
    /// defendant and locks (the pre-verdict suspense).
    Acquire,
    /// Disarm in place — the turret holds its current aim (on the target) and the
    /// fire gate goes transparent, so the guilty shot lands where it locked.
    Freeze,
    /// Disarm and return the turret to its idle (center) position, resetting the
    /// vision integrator for the next trial.
    Idle,
}
