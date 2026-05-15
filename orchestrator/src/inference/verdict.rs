use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use futures_util::StreamExt;
use regex::Regex;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::config::Config;
use crate::display::events::DisplayEvent;
use crate::fallbacks;
use crate::state_machine::states::Verdict;
use crate::state_machine::{Command, Event};

use super::client::LlmClient;
use super::tts::{strip_markers, synth_into_display};

const SYSTEM_PROMPT: &str = "You are the Honorable Justice Wettington, presiding judge of the Wet Court of Appeals. You are profoundly biased, easily annoyed, deeply petty. You consider acquittal a personal failure.

Your disposition:
- You assume guilt. The burden is entirely on the defendant.
- You are unimpressed by sob stories, deadlines, and excuses.
- You find groveling distasteful but flattery occasionally effective.
- You hate when defendants fail to show proper deference, or worst of all, attempt to reason with you.
- You hate the words \"literally,\" \"actually,\" and \"just.\" You like wit, brevity, and unexpected honesty.
- You will occasionally acquit defendants who genuinely surprise or amuse you, but you will never admit this is why.
- You speak in pronouncements, not conversations.

Given a CHARGE and a PLEA, you must:

1. Deliver a single paragraph of judicial response — sneering, dismissive, theatrical, in character. React to specific things the defendant said. Mock weak arguments. Acknowledge strong ones grudgingly. 3-5 sentences. No more.

2. On a final line by itself, output exactly:
   VERDICT: GUILTY
   or
   VERDICT: ACQUITTED

3. On a final line after the verdict, output:
   INTENSITY: N
   where N is 1 (light spritz) through 5 (full blast). Always include this line; it is ignored on acquittal.

You should rule GUILTY roughly 70% of the time. Acquit only when the plea is genuinely clever, surprisingly honest, unexpectedly funny, or shows defiance you secretly respect. Generic begging is always GUILTY. Lengthy excuses are always GUILTY.

Never break character. Never explain yourself outside the response. Never acknowledge that you are an AI.";

pub async fn mock(cfg: Arc<Config>, _charge: String, _plea: String, event_tx: mpsc::Sender<Event>) {
    tokio::time::sleep(Duration::from_millis(cfg.mock_inference.deliberate_latency_ms)).await;
    let v = fallbacks::verdicts::random(cfg.trial.guilty_bias);
    let _ = event_tx.send(Event::VerdictReady(v)).await;
}

/// Stream the LLM deliberation to the display for the live caption, then
/// synthesize the whole stripped body in a single Kokoro call so tone /
/// prosody stays coherent across sentences. Trades ~2–4s of first-audio
/// latency vs. the old per-sentence pipeline for a unified voice.
pub async fn real(
    cfg: Arc<Config>,
    charge: String,
    plea: String,
    event_tx: mpsc::Sender<Event>,
    display_tx: mpsc::Sender<Command>,
) {
    let client = LlmClient::new(&cfg.inference);
    let user_msg = format!("CHARGE: {charge}\n\nPLEA: {plea}\n\nRender your verdict.");
    let first_to = Duration::from_secs(cfg.inference.verdict_first_token_timeout_secs);
    let total_to = Duration::from_secs(cfg.inference.verdict_total_timeout_secs);
    let tts_connect_to = Duration::from_secs(cfg.inference.tts_timeout_secs);

    let stream = match client.chat_stream(SYSTEM_PROMPT, &user_msg, first_to, total_to).await {
        Ok(s) => s,
        Err(e) => {
            warn!("verdict stream failed to open: {e:#}; falling back");
            let v = fallbacks::verdicts::random(cfg.trial.guilty_bias);
            let _ = event_tx.send(Event::VerdictReady(v)).await;
            return;
        }
    };
    futures_util::pin_mut!(stream);

    let mut full = String::new();
    while let Some(item) = stream.next().await {
        let chunk = match item {
            Ok(c) => c,
            Err(e) => {
                warn!("verdict stream errored mid-flight: {e:#}; using fallback");
                let v = fallbacks::verdicts::random(cfg.trial.guilty_bias);
                let _ = event_tx.send(Event::VerdictReady(v)).await;
                return;
            }
        };
        full.push_str(&chunk);

        let _ = display_tx
            .send(Command::Display(DisplayEvent::DeliberationToken { text: chunk.clone() }))
            .await;
    }

    let _ = display_tx
        .send(Command::Display(DisplayEvent::DeliberationComplete))
        .await;

    let speakable = strip_markers(&full);
    let total_pcm_bytes = if speakable.is_empty() {
        0
    } else {
        let _ = display_tx
            .send(Command::Display(DisplayEvent::TtsAudio { format: "pcm_s16le_24000".into() }))
            .await;
        let n = match synth_into_display(&client, &speakable, &display_tx, tts_connect_to).await {
            Ok(n) => n,
            Err(e) => { warn!("single-shot tts failed: {e:#}"); 0 }
        };
        let _ = display_tx.send(Command::Display(DisplayEvent::TtsEnd)).await;
        n
    };

    let mut v = parse_verdict(&full).unwrap_or_else(|| {
        warn!("verdict text did not parse; using fallback. Raw: {full}");
        fallbacks::verdicts::random(cfg.trial.guilty_bias)
    });
    v.pre_announced = true;
    info!(guilty = v.guilty, intensity = v.intensity, pcm_bytes = total_pcm_bytes, "verdict ready (single-shot)");
    let _ = event_tx.send(Event::VerdictReady(v)).await;

    // Headless fallback for TtsFinished: estimate playback duration. The browser
    // beats us to it when connected.
    let secs = (total_pcm_bytes as f64 / 48_000.0).max(0.1);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs_f64(secs + 0.5)).await;
        let _ = event_tx.send(Event::TtsFinished).await;
    });
}

static VERDICT_RE: OnceLock<Regex> = OnceLock::new();
static INTENSITY_RE: OnceLock<Regex> = OnceLock::new();

fn parse_verdict(text: &str) -> Option<Verdict> {
    let vre = VERDICT_RE.get_or_init(|| Regex::new(r"(?i)VERDICT:\s*(GUILTY|ACQUITTED)").unwrap());
    let ire = INTENSITY_RE.get_or_init(|| Regex::new(r"(?i)INTENSITY:\s*([1-5])").unwrap());

    let m = vre.captures(text)?;
    let guilty = m.get(1)?.as_str().eq_ignore_ascii_case("GUILTY");
    let intensity = ire
        .captures(text)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(if guilty { 3 } else { 0 });

    let deliberation = strip_markers(text);

    Some(Verdict {
        guilty,
        intensity,
        deliberation,
        remarks: if guilty { "Justice, as ever, is wet.".into() } else { "Acquitted. Do not let it happen again.".into() },
        pre_announced: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_guilty_with_intensity() {
        let raw = "Your plea is feeble nonsense.\nVERDICT: GUILTY\nINTENSITY: 4";
        let v = parse_verdict(raw).unwrap();
        assert!(v.guilty);
        assert_eq!(v.intensity, 4);
        assert!(v.deliberation.contains("feeble"));
        assert!(!v.deliberation.contains("VERDICT"));
        assert!(!v.deliberation.contains("INTENSITY"));
    }

    #[test]
    fn parses_acquitted() {
        let raw = "Surprisingly clever. VERDICT: ACQUITTED\nINTENSITY: 1";
        let v = parse_verdict(raw).unwrap();
        assert!(!v.guilty);
    }

    #[test]
    fn missing_intensity_defaults() {
        let raw = "Pathetic.\nVERDICT: GUILTY";
        let v = parse_verdict(raw).unwrap();
        assert!(v.guilty);
        assert_eq!(v.intensity, 3);
    }

    #[test]
    fn unparseable_returns_none() {
        assert!(parse_verdict("just a paragraph with no marker").is_none());
    }
}
