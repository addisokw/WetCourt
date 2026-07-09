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

/// Hold music before the announcement lands (also hides TTS latency).
const HOLD_INTRO: Duration = Duration::from_millis(3200);
/// A beat of music after the announcement before Dewey "picks up".
const HOLD_OUTRO: Duration = Duration::from_millis(1400);

/// The post-IVR hold gag: hold music, broken by the office voice announcing
/// an absurd queue position ("you are next in line for our most available
/// attorney"). Skipped when the persona has no hold_line or the hold-music
/// asset is missing.
pub async fn hold_gag(shared: &Shared, mixer: &MixerHandle, token: &CancellationToken) {
    let Some(template) = &shared.persona.hold_line else {
        return;
    };
    let Some(music) = &shared.cover.hold_music else {
        return;
    };
    mixer.set_cover(Some(music.clone()));

    let position = 2_000_000 + (rand::random::<u32>() % 97_000_000) as u64;
    let line = template.replace("{n}", &number_words(position));
    tracing::info!(position, "hold gag: queueing announcement");

    // Music establishes itself while (and after) the announcement
    // synthesizes; the speech queue preempts the loop when bytes land.
    let synth = crate::call::agent::synth_to_queue(
        shared,
        mixer,
        &line,
        &shared.persona.hold_voice,
        Some(0.95),
    );
    let (synth_result, _) = tokio::join!(synth, tokio::time::sleep(HOLD_INTRO));
    if let Err(e) = synth_result {
        tracing::warn!("hold announcement tts failed, music only: {e:#}");
    }

    tokio::select! {
        _ = token.cancelled() => return,
        _ = mixer.wait_drained() => {}
    }
    tokio::select! {
        _ = token.cancelled() => return,
        _ = tokio::time::sleep(HOLD_OUTRO) => {}
    }
    mixer.set_cover(None);
}

/// Spell a number out in words so the TTS reads the whole absurd thing
/// ("eight million four hundred two thousand seventeen") instead of
/// gambling on numeral normalization.
pub fn number_words(n: u64) -> String {
    const ONES: [&str; 20] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight",
        "nine", "ten", "eleven", "twelve", "thirteen", "fourteen", "fifteen",
        "sixteen", "seventeen", "eighteen", "nineteen",
    ];
    const TENS: [&str; 10] = [
        "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy",
        "eighty", "ninety",
    ];

    fn under_thousand(n: u64, out: &mut Vec<String>) {
        if n >= 100 {
            out.push(format!("{} hundred", ONES[(n / 100) as usize]));
        }
        let rem = n % 100;
        if rem == 0 {
            return;
        }
        if rem < 20 {
            out.push(ONES[rem as usize].to_string());
        } else if rem % 10 == 0 {
            out.push(TENS[(rem / 10) as usize].to_string());
        } else {
            out.push(format!("{} {}", TENS[(rem / 10) as usize], ONES[(rem % 10) as usize]));
        }
    }

    if n == 0 {
        return "zero".to_string();
    }
    let mut parts = Vec::new();
    for (scale, name) in [(1_000_000_000, "billion"), (1_000_000, "million"), (1_000, "thousand")] {
        let q = (n / scale) % 1000;
        if q > 0 {
            let mut chunk = Vec::new();
            under_thousand(q, &mut chunk);
            parts.push(format!("{} {name}", chunk.join(" ")));
        }
    }
    let mut tail = Vec::new();
    under_thousand(n % 1000, &mut tail);
    parts.extend(tail);
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::number_words;

    #[test]
    fn spells_numbers() {
        assert_eq!(number_words(0), "zero");
        assert_eq!(number_words(14), "fourteen");
        assert_eq!(number_words(40), "forty");
        assert_eq!(number_words(97), "ninety seven");
        assert_eq!(number_words(305), "three hundred five");
        assert_eq!(number_words(2_000_000), "two million");
        assert_eq!(
            number_words(8_402_017),
            "eight million four hundred two thousand seventeen"
        );
        assert_eq!(
            number_words(98_999_999),
            "ninety eight million nine hundred ninety nine thousand nine hundred ninety nine"
        );
    }

    #[test]
    fn queue_position_range_spells_cleanly() {
        for _ in 0..50 {
            let n = 2_000_000 + (rand::random::<u32>() % 97_000_000) as u64;
            let words = number_words(n);
            assert!(words.contains("million"), "{n} → {words}");
            assert!(!words.contains("  "), "double space in {words}");
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
    // Hold music behind the prompt: silent while the prompt plays (speech
    // preempts cover), then fills the keypress grace window instead of dead
    // air, and runs seamlessly into the hold gag.
    mixer.set_cover(shared.cover.hold_music.clone());

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
