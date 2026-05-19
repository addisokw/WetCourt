use std::collections::BTreeMap;
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

/// The 10 A2F-3D emotion sliders. Lowercase keys match what the LLM emits and
/// what the UE renderer expects on the wire.
const A2F_EMOTIONS: &[&str] = &[
    "amazement", "anger", "cheekiness", "disgust", "fear",
    "grief", "joy", "outofbreath", "pain", "sadness",
];

/// Multiplier applied to LLM-emitted emotion weights before they're sent to
/// the ACE renderer (clamped to 1.0). The model is conservative — it'll write
/// `anger=0.7` for a clearly furious verdict — so this scales those up to
/// something the face actually reads at conversational distance. Tune in
/// concert with `intensity_to_strength`; this directly scales the per-emotion
/// slider, that scales the global multiplier the renderer applies after the
/// emotion blend.
const LLM_EMOTION_SCALE: f32 = 1.5;

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

1. Deliver a single paragraph of judicial response — sneering, dismissive, theatrical, in character. React to specific things the defendant said. Mock weak arguments. Acknowledge strong ones grudgingly. EXACTLY 4-5 substantive sentences (no fewer than 4, no more than 5). Each sentence should be 10+ words of genuine judicial bloviation, not a clipped pronouncement. A verdict that comes in at 3 sentences or fewer, or relies on terse fragments, fails to dispense proper theatre and is not acceptable.

2. On a final line by itself, output exactly:
   VERDICT: GUILTY
   or
   VERDICT: ACQUITTED

3. On a final line after the verdict, output:
   INTENSITY: N
   where N is 1 (light spritz) through 5 (full blast). Always include this line; it is ignored on acquittal.

4. On a final line after intensity, output:
   EMOTION: anger=0.6, disgust=0.4
   A comma-separated list of emotion=weight pairs (weight 0.0-1.0). Pick 1-4 emotions that color the delivery of your judicial response. Available emotions: amazement, anger, cheekiness, disgust, fear, grief, joy, outofbreath, pain, sadness. Use anger/disgust for harsh guilty verdicts, cheekiness/joy for amused acquittals, grief/pain for theatrical lamentation, amazement for genuine surprise. Always include this line.

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
///
/// Pipelined TTS was attempted and reverted: per-sentence synth produces
/// 700-1000ms intra-stream gaps (Kokoro HTTP setup per sentence), which
/// cause UE audio buffer underruns (audible as static) and dual
/// animation_started events (ACE restarts mid-playback). See harness data
/// in renderer/tools/e2e_harness.py; baseline single-shot fits the 5s
/// budget already (~4s plea→first-audio).
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
        // Tentative parse for the intensity → emotion-strength mapping
        // (we still parse verdict again below for the final result).
        let preview = parse_verdict(&full);
        let parsed = parse_emotion(&full);
        let emotion_source = if parsed.is_some() { "llm" } else { "derived" };
        let emotions = parsed.unwrap_or_else(|| {
            derive_emotion(preview.as_ref().map(|v| v.guilty).unwrap_or(true))
        });
        let intensity = preview.as_ref().map(|v| v.intensity).unwrap_or(3);
        let overall = intensity_to_strength(intensity);
        let summary: Vec<String> = emotions.iter().map(|(k, v)| format!("{k}={v:.2}")).collect();
        info!(source = emotion_source, overall, "tts_emotion: {}", summary.join(", "));
        let _ = display_tx
            .send(Command::Display(DisplayEvent::TtsEmotion {
                emotions,
                overall_strength: overall,
                override_strength: 0.95,
            }))
            .await;

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
static EMOTION_RE: OnceLock<Regex> = OnceLock::new();
static EMOTION_PAIR_RE: OnceLock<Regex> = OnceLock::new();

/// Map INTENSITY 1..5 to FAudio2FaceEmotion.OverallEmotionStrength (0..1).
/// Plugin default is 0.6; we sit above that across the board so the face
/// reads expressively at conversational distance.
fn intensity_to_strength(intensity: u8) -> f32 {
    match intensity {
        0 | 1 => 0.65,
        2 => 0.80,
        3 => 0.90,
        4 => 0.95,
        _ => 1.00,
    }
}

/// Default emotion when the LLM didn't emit an EMOTION line.
fn derive_emotion(guilty: bool) -> BTreeMap<String, f32> {
    let mut m = BTreeMap::new();
    if guilty {
        m.insert("anger".into(), 0.7);
        m.insert("disgust".into(), 0.4);
    } else {
        m.insert("cheekiness".into(), 0.6);
        m.insert("amazement".into(), 0.3);
    }
    m
}

/// Parse an `EMOTION: a=0.5, b=0.7` line. Unknown keys ignored, values clamped
/// to 0..1. Returns None if no EMOTION line / no valid pairs found.
fn parse_emotion(text: &str) -> Option<BTreeMap<String, f32>> {
    let line_re = EMOTION_RE.get_or_init(|| Regex::new(r"(?im)^\s*EMOTION:\s*(.+)$").unwrap());
    let pair_re = EMOTION_PAIR_RE.get_or_init(|| Regex::new(r"([A-Za-z]+)\s*=\s*(-?[0-9]*\.?[0-9]+)").unwrap());
    let caps = line_re.captures(text)?;
    let body = caps.get(1)?.as_str();
    let mut map = BTreeMap::new();
    for c in pair_re.captures_iter(body) {
        let (Some(k), Some(v)) = (c.get(1), c.get(2)) else { continue };
        let name = k.as_str().to_lowercase();
        if !A2F_EMOTIONS.contains(&name.as_str()) {
            continue;
        }
        let Ok(value) = v.as_str().parse::<f32>() else { continue };
        map.insert(name, (value * LLM_EMOTION_SCALE).clamp(0.0, 1.0));
    }
    if map.is_empty() { None } else { Some(map) }
}

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

    #[test]
    fn parses_emotion_line() {
        let raw = "Pathetic.\nVERDICT: GUILTY\nINTENSITY: 4\nEMOTION: anger=0.5, disgust=0.3";
        let m = parse_emotion(raw).unwrap();
        // Parsed values are scaled by LLM_EMOTION_SCALE and clamped to 1.0.
        assert!((m["anger"]   - (0.5 * LLM_EMOTION_SCALE).min(1.0)).abs() < 1e-5);
        assert!((m["disgust"] - (0.3 * LLM_EMOTION_SCALE).min(1.0)).abs() < 1e-5);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn emotion_clamps_and_ignores_unknown() {
        let m = parse_emotion("EMOTION: anger=1.5, banana=0.9, joy=-0.2").unwrap();
        assert!((m["anger"] - 1.0).abs() < 1e-5);
        assert!((m["joy"] - 0.0).abs() < 1e-5);
        assert!(!m.contains_key("banana"));
    }

    #[test]
    fn missing_emotion_returns_none() {
        assert!(parse_emotion("VERDICT: GUILTY\nINTENSITY: 3").is_none());
    }

    #[test]
    fn strip_markers_drops_emotion_line() {
        use crate::inference::tts::strip_markers;
        let raw = "Body.\nVERDICT: GUILTY\nINTENSITY: 3\nEMOTION: anger=0.8";
        let s = strip_markers(raw);
        assert_eq!(s, "Body.");
    }
}
