//! The intake menu: "press 1 if guilty, press 2 if very guilty." Plays the
//! recorded prompt, waits briefly for a key, and hands whatever happened to
//! the agent as context. Doubles as latency cover — the trial-context fetch
//! and prompt assembly run while this plays.

use std::time::Duration;

use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;

use crate::http::Shared;
use crate::rtp::{MixerHandle, RtpEvent};

/// Grace period after the prompt finishes before giving up on a key.
const AFTER_PROMPT_GRACE: Duration = Duration::from_secs(6);

pub enum IvrOutcome {
    Digit(char),
    NoInput,
    Skipped,
}

impl IvrOutcome {
    /// The note folded into the conversation history for the lawyer.
    pub fn history_note(&self) -> Option<String> {
        match self {
            IvrOutcome::Digit('1') => Some(
                "(Intake menu: the client pressed 1 — 'guilty'.)".to_string(),
            ),
            IvrOutcome::Digit('2') => Some(
                "(Intake menu: the client pressed 2 — 'very guilty'.)".to_string(),
            ),
            IvrOutcome::Digit(d) => Some(format!(
                "(Intake menu: the client pressed {d}, which is not one of the two options. Note it.)"
            )),
            IvrOutcome::NoInput => Some(
                "(Intake menu: the client pressed nothing. A refusal to self-classify.)"
                    .to_string(),
            ),
            IvrOutcome::Skipped => None,
        }
    }
}

pub async fn menu(
    shared: &Shared,
    mixer: &MixerHandle,
    events: &mut Receiver<RtpEvent>,
    token: &CancellationToken,
) -> IvrOutcome {
    let Some(prompt) = &shared.cover.ivr_prompt else {
        return IvrOutcome::Skipped;
    };
    mixer.queue_speech(prompt);

    // Prompt length + grace, at 8000 µ-law bytes per second.
    let deadline = tokio::time::Instant::now()
        + Duration::from_millis(prompt.len() as u64 / 8)
        + AFTER_PROMPT_GRACE;

    loop {
        tokio::select! {
            _ = token.cancelled() => return IvrOutcome::Skipped,
            _ = tokio::time::sleep_until(deadline) => return IvrOutcome::NoInput,
            ev = events.recv() => match ev {
                None => return IvrOutcome::Skipped,
                Some(RtpEvent::Dtmf(digit)) => {
                    tracing::info!(%digit, "IVR selection");
                    mixer.clear_speech(); // cut the prompt, they chose
                    return IvrOutcome::Digit(digit);
                }
                Some(RtpEvent::Audio(_)) => {} // menu ignores talking, of course
            }
        }
    }
}
