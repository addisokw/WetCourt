use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use regex::Regex;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use crate::config::Config;
use crate::display::events::DisplayEvent;
use crate::fallbacks;
use crate::personas::PersonaRegistry;
use crate::state_machine::states::{CrossExam, Verdict};
use crate::state_machine::{Command, Event};

use super::client::LlmClient;
use super::tts::{strip_markers, synth_into_display};

pub async fn mock(
    cfg: Arc<Config>,
    _charge: String,
    _plea: String,
    _cross: Option<CrossExam>,
    event_tx: mpsc::Sender<Event>,
) {
    tokio::time::sleep(Duration::from_millis(cfg.mock_inference.deliberate_latency_ms)).await;
    let v = fallbacks::verdicts::random(cfg.trial.guilty_bias);
    let _ = event_tx.send(Event::VerdictReady(v)).await;
}

/// Assemble the verdict user message, folding in the cross-examination exchange
/// when one took place so the judge can weigh the answer.
fn build_user_msg(charge: &str, plea: &str, cross: &Option<CrossExam>) -> String {
    let mut msg = format!("CHARGE: {charge}\n\nPLEA: {plea}");
    if let Some(c) = cross {
        msg.push_str(&format!(
            "\n\nCROSS-EXAMINATION:\nYou asked: {}\nThe defendant answered: {}",
            c.question, c.answer
        ));
    }
    msg.push_str("\n\nRender your verdict.");
    msg
}

/// Stream the LLM deliberation to the display for the live caption, then
/// synthesize the whole stripped body in a single Kokoro call so tone /
/// prosody stays coherent across sentences. Trades ~2–4s of first-audio
/// latency vs. the old per-sentence pipeline for a unified voice.
pub async fn real(
    cfg: Arc<Config>,
    personas: Arc<RwLock<PersonaRegistry>>,
    charge: String,
    plea: String,
    cross: Option<CrossExam>,
    event_tx: mpsc::Sender<Event>,
    display_tx: mpsc::Sender<Command>,
) {
    // Snapshot the active persona once at trial start; mid-trial changes
    // don't apply by design.
    // The guilty_bias slider is injected into the prompt here (not baked into
    // the persona text) so it is the sole knob governing conviction rate.
    let (system_prompt, voice, guilty_bias) = {
        let reg = personas.read().await;
        let p = reg.active();
        (p.system_prompt_with_bias(), p.tts_voice.clone(), p.guilty_bias as f64)
    };

    let client = LlmClient::new(&cfg.inference);
    let user_msg = build_user_msg(&charge, &plea, &cross);
    let first_to = Duration::from_secs(cfg.inference.verdict_first_token_timeout_secs);
    let total_to = Duration::from_secs(cfg.inference.verdict_total_timeout_secs);
    let tts_connect_to = Duration::from_secs(cfg.inference.tts_timeout_secs);

    let stream = match client.chat_stream(&system_prompt, &user_msg, first_to, total_to).await {
        Ok(s) => s,
        Err(e) => {
            warn!("verdict stream failed to open: {e:#}; falling back");
            let v = fallbacks::verdicts::random(guilty_bias);
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
    info!(
        llm_raw_len = full.len(),
        speakable_len = speakable.len(),
        llm_first_120 = %truncate(&full, 120),
        "llm verdict stream complete"
    );

    let mut v = parse_verdict(&full).unwrap_or_else(|| {
        warn!("verdict text did not parse; using fallback. Raw: {full}");
        let mut fb = fallbacks::verdicts::random(guilty_bias);
        fb.pre_announced = true;
        fb
    });
    v.pre_announced = true;
    let guilty = v.guilty;
    let remarks = v.remarks.clone();
    let verdict_word: &str = if guilty { "Guilty." } else { "Not guilty." };

    // Move the state machine out of Deliberating ASAP — its 30s Tick timeout
    // would otherwise fire while we're still pacing audio playback. The
    // pre_announced flag tells the state machine to skip its own Verdict
    // broadcast and Speak command; we'll emit them ourselves at the right
    // theatrical moments below.
    let _ = event_tx.send(Event::VerdictReady(v)).await;

    // Single audio session wraps deliberation + preamble + (silent theater) +
    // verdict word. Only the final TtsEnd advances PronouncingVerdict, so
    // intermediate "session" boundaries from the synth stay invisible to the
    // state machine.
    let _ = display_tx
        .send(Command::Display(DisplayEvent::TtsAudio { format: "pcm_s16le_24000".into() }))
        .await;

    // 1) Speak the deliberation body.
    let t1 = Instant::now();
    info!(text = %truncate(&speakable, 120), "tts segment 1 (deliberation) start");
    let n1 = synth_body(&client, &speakable, &voice, &display_tx, tts_connect_to).await;
    info!(bytes = n1, "tts segment 1 (deliberation) bytes");
    play_through(t1, n1).await;

    // 2) Lead-in: "The court finds the defendant…"
    let preamble = "The court finds the defendant...";
    let t2 = Instant::now();
    info!(text = preamble, "tts segment 2 (preamble) start");
    let n2 = synth_body(&client, preamble, &voice, &display_tx, tts_connect_to).await;
    info!(bytes = n2, "tts segment 2 (preamble) bytes");
    play_through(t2, n2).await;

    // 3) Theater beat — pad + dim — covers the dramatic silence in the
    //    audio queue (no PCM bytes flow for 3s).
    const THEATER_BEAT: Duration = Duration::from_millis(3000);
    let _ = display_tx.send(Command::Display(DisplayEvent::TheaterStart)).await;
    tokio::time::sleep(THEATER_BEAT).await;
    let _ = display_tx.send(Command::Display(DisplayEvent::TheaterEnd)).await;

    // 4) Reveal: broadcast the Verdict display event NOW (face flips colour,
    //    case view shows GUILTY/NOT GUILTY) right as the verdict-word TTS
    //    starts playing.
    let _ = display_tx
        .send(Command::Display(DisplayEvent::Verdict {
            guilty,
            remarks,
        }))
        .await;

    let t3 = Instant::now();
    info!(text = verdict_word, "tts segment 3 (verdict word) start");
    let n3 = synth_body(&client, verdict_word, &voice, &display_tx, tts_connect_to).await;
    info!(bytes = n3, "tts segment 3 (verdict word) bytes");

    // Close the single audio session. Browser fires tts_finished once after
    // the queue drains (i.e. after the verdict word plays).
    let _ = display_tx.send(Command::Display(DisplayEvent::TtsEnd)).await;

    info!(pcm_bytes_total = n1 + n2 + n3, "verdict spoken (deliberation + preamble + word)");

    // Headless fallback: if no browser is listening, fire TtsFinished after
    // the verdict word would have finished playing.
    tokio::spawn(async move {
        play_through(t3, n3).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = event_tx.send(Event::TtsFinished).await;
    });
}

/// Synthesise PCM straight into `display_tx` without emitting `tts_audio` /
/// `tts_end` boundaries — the multi-segment verdict flow wraps everything in
/// one outer session. Returns total bytes pushed.
async fn synth_body(
    client: &LlmClient,
    text: &str,
    voice: &str,
    display_tx: &mpsc::Sender<Command>,
    connect_to: Duration,
) -> usize {
    if text.is_empty() {
        return 0;
    }
    match synth_into_display(client, text, voice, display_tx, connect_to).await {
        Ok(n) => n,
        Err(e) => { warn!("tts segment failed: {e:#}"); 0 }
    }
}

fn truncate(s: &str, max: usize) -> String {
    let single_line: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if single_line.chars().count() <= max {
        single_line
    } else {
        let head: String = single_line.chars().take(max).collect();
        format!("{head}…")
    }
}

/// Wait until a session that started at `start` would have finished playing
/// (24 kHz mono s16le → 48 000 bytes/s). If we already spent that long
/// synthesising it (Kokoro can be slower than realtime on cold start), no
/// extra sleep.
async fn play_through(start: Instant, bytes: usize) {
    let dur = Duration::from_secs_f64(bytes as f64 / 48_000.0);
    let elapsed = start.elapsed();
    if elapsed < dur {
        tokio::time::sleep(dur - elapsed).await;
    }
}

static VERDICT_RE: OnceLock<Regex> = OnceLock::new();

fn parse_verdict(text: &str) -> Option<Verdict> {
    let vre = VERDICT_RE.get_or_init(|| Regex::new(r"(?i)VERDICT:\s*(GUILTY|ACQUITTED)").unwrap());

    let m = vre.captures(text)?;
    let guilty = m.get(1)?.as_str().eq_ignore_ascii_case("GUILTY");

    let deliberation = strip_markers(text);

    Some(Verdict {
        guilty,
        deliberation,
        remarks: if guilty { "Justice, as ever, is wet.".into() } else { "Acquitted. Do not let it happen again.".into() },
        pre_announced: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_guilty() {
        let raw = "Your plea is feeble nonsense.\nVERDICT: GUILTY";
        let v = parse_verdict(raw).unwrap();
        assert!(v.guilty);
        assert!(v.deliberation.contains("feeble"));
        assert!(!v.deliberation.contains("VERDICT"));
    }

    #[test]
    fn parses_acquitted() {
        let raw = "Surprisingly clever. VERDICT: ACQUITTED";
        let v = parse_verdict(raw).unwrap();
        assert!(!v.guilty);
    }

    #[test]
    fn strips_stray_intensity_line() {
        let raw = "Pathetic.\nVERDICT: GUILTY\nINTENSITY: 4";
        let v = parse_verdict(raw).unwrap();
        assert!(v.guilty);
        assert!(!v.deliberation.contains("INTENSITY"));
    }

    #[test]
    fn unparseable_returns_none() {
        assert!(parse_verdict("just a paragraph with no marker").is_none());
    }
}
