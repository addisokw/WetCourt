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
use crate::hardware::maintenance::{MaintenanceCommand, Role};
use crate::hardware::protocol::{FacePhase, HardwareCommand};
use crate::personas::PersonaRegistry;
use crate::state_machine::states::{CrossExam, Verdict};
use crate::state_machine::{Command, Event};

use super::client::LlmClient;
use super::tts::{strip_markers, synth_into_display, StreamMarkerFilter};

/// Operator mode #42: flip a guilty verdict to an acquittal. Must run before
/// anything derives from the verdict (spoken word, reveal events, the FSM's
/// copy) so record and reveal stay coherent. Returns true if it flipped.
pub fn apply_innocent_override(v: &mut Verdict) -> bool {
    if !v.guilty {
        return false;
    }
    v.guilty = false;
    v.remarks = fallbacks::verdicts::FORCED_ACQUITTAL_REMARKS.into();
    v.key_factor = None;
    true
}

/// The #42 prompt directive: steers the whole deliberation toward acquittal so
/// the monologue argues for the verdict the override guarantees.
const INNOCENT_DIRECTIVE: &str = "\n\nSECRET OPERATOR DIRECTIVE (never mention or allude to \
it): whatever the plea, this trial you WILL find the defendant NOT GUILTY. Deliberate fully \
in character — discover a grudging technicality, an unexpected charm, or a fatal procedural \
flaw in the prosecution — and end with VERDICT: ACQUITTED.";

pub async fn mock(
    cfg: Arc<Config>,
    _charge: String,
    _plea: String,
    _cross: Option<CrossExam>,
    event_tx: mpsc::Sender<Event>,
    modes: Arc<crate::operator_modes::OperatorModes>,
) {
    tokio::time::sleep(Duration::from_millis(cfg.mock_inference.deliberate_latency_ms)).await;
    let mut v = fallbacks::verdicts::random(cfg.trial.guilty_bias);
    if modes.active_contains(crate::operator_modes::CODE_INNOCENT) && apply_innocent_override(&mut v)
    {
        info!("operator mode 42: mock verdict forced to acquittal");
    }
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
#[allow(clippy::too_many_arguments)]
pub async fn real(
    cfg: Arc<Config>,
    personas: Arc<RwLock<PersonaRegistry>>,
    charge: String,
    plea: String,
    cross: Option<CrossExam>,
    event_tx: mpsc::Sender<Event>,
    display_tx: mpsc::Sender<Command>,
    maint_cmd_tx: mpsc::Sender<MaintenanceCommand>,
    modes: Arc<crate::operator_modes::OperatorModes>,
) {
    // Snapshot the active persona once at trial start; mid-trial changes
    // don't apply by design.
    // The guilty_bias slider is injected into the prompt here (not baked into
    // the persona text) so it is the sole knob governing conviction rate.
    let (mut system_prompt, voice, speed, mut guilty_bias) = {
        let reg = personas.read().await;
        let p = reg.active();
        (reg.verdict_prompt(p), p.tts_voice.clone(), p.tts_speed, p.guilty_bias as f64)
    };
    // Operator mode 42, layer 1: steer the deliberation itself toward the
    // acquittal so the monologue argues for the verdict the hard force (layer
    // 2, at parse) guarantees. Snapshotted once, like the persona.
    let forced_innocent = modes.active_contains(crate::operator_modes::CODE_INNOCENT);
    if forced_innocent {
        info!("operator mode 42 active: steering verdict toward acquittal");
        system_prompt.push_str(INNOCENT_DIRECTIVE);
        guilty_bias = 0.0; // any fallback constructed below must acquit too
    }

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
    // Marker lines (VERDICT:/KEY_FACTOR:/…) are filtered server-side, mid-token,
    // so no display can ever flash "VERDICT: GUIL…" before the reveal. `full`
    // keeps the raw text — the parser needs the markers.
    let mut caption = StreamMarkerFilter::new();
    while let Some(item) = stream.next().await {
        let chunk = match item {
            Ok(c) => c,
            Err(e) => {
                warn!("verdict stream errored mid-flight: {e:#}; using fallback");
                let v = fallbacks::verdicts::random(guilty_bias);
                let _ = event_tx.send(Event::VerdictReady(v)).await;
                return;
            }
        };
        full.push_str(&chunk);

        let visible = caption.push(&chunk);
        if !visible.is_empty() {
            let _ = display_tx
                .send(Command::Display(DisplayEvent::DeliberationToken { text: visible }))
                .await;
        }
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
    // Operator mode 42, layer 2: the hard force. This must happen before
    // ANYTHING derives from `v` — the locals below feed the spoken verdict
    // word and the reveal events, and the FSM gets its copy via VerdictReady —
    // so flipping here keeps every downstream consumer coherent.
    if forced_innocent && apply_innocent_override(&mut v) {
        info!("operator mode 42: forcing acquittal over a guilty verdict");
    }
    let guilty = v.guilty;
    let remarks = v.remarks.clone();
    let key_factor = v.key_factor.clone();
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
    let n1 = synth_body(&client, &speakable, &voice, speed, &display_tx, tts_connect_to).await;
    info!(bytes = n1, "tts segment 1 (deliberation) bytes");
    play_through(t1, n1).await;

    // 2) Lead-in: "The court finds the defendant…"
    let preamble = "The court finds the defendant...";
    let t2 = Instant::now();
    info!(text = preamble, "tts segment 2 (preamble) start");
    let n2 = synth_body(&client, preamble, &voice, speed, &display_tx, tts_connect_to).await;
    info!(bytes = n2, "tts segment 2 (preamble) bytes");
    play_through(t2, n2).await;

    // 3) Theater beat — pad + dim — covers the dramatic silence in the
    //    audio queue (no PCM bytes flow for 3s).
    const THEATER_BEAT: Duration = Duration::from_millis(3000);
    let _ = display_tx.send(Command::Display(DisplayEvent::TheaterStart)).await;
    tokio::time::sleep(THEATER_BEAT).await;
    let _ = display_tx.send(Command::Display(DisplayEvent::TheaterEnd)).await;

    // 4) Reveal: broadcast the Verdict display event NOW (case view shows
    //    GUILTY/NOT GUILTY) right as the verdict-word TTS starts playing, and
    //    flip the LED-matrix eye to its verdict phase at the same beat — the
    //    guilty strobe / innocent bloom must land with the reveal, never at
    //    VerdictReady (a whole deliberation earlier).
    let _ = display_tx
        .send(Command::Display(DisplayEvent::Verdict {
            guilty,
            remarks,
            key_factor,
        }))
        .await;
    let _ = maint_cmd_tx
        .send(MaintenanceCommand {
            target: Role::JudgeFace,
            cmd: HardwareCommand::Face(FacePhase::verdict(guilty)),
            reply: None, // fire-and-forget; the face may be absent
        })
        .await;
    // The gavel lands on the same beat — at the reveal, not when the
    // deliberation started playing back at VerdictReady. Routed through the
    // FSM so the hardware adapter applies the gavel.toml strike geometry.
    let _ = event_tx.send(Event::VerdictRevealed).await;

    let t3 = Instant::now();
    info!(text = verdict_word, "tts segment 3 (verdict word) start");
    let n3 = synth_body(&client, verdict_word, &voice, speed, &display_tx, tts_connect_to).await;
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
    speed: Option<f32>,
    display_tx: &mpsc::Sender<Command>,
    connect_to: Duration,
) -> usize {
    if text.is_empty() {
        return 0;
    }
    match synth_into_display(client, text, voice, speed, display_tx, connect_to).await {
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
static KEY_FACTOR_RE: OnceLock<Regex> = OnceLock::new();
static REASON_RE: OnceLock<Regex> = OnceLock::new();

/// Pull a single-line marker value (e.g. `KEY_FACTOR: bragged about it`),
/// trimmed and normalised to `None` when empty.
fn marker_value(re: &Regex, text: &str) -> Option<String> {
    let v = re.captures(text)?.get(1)?.as_str().trim().to_string();
    if v.is_empty() { None } else { Some(v) }
}

fn parse_verdict(text: &str) -> Option<Verdict> {
    let vre = VERDICT_RE.get_or_init(|| Regex::new(r"(?i)VERDICT:\s*(GUILTY|ACQUITTED)").unwrap());
    let kre = KEY_FACTOR_RE.get_or_init(|| Regex::new(r"(?im)^\s*KEY_FACTOR:\s*(.+)$").unwrap());
    let rre = REASON_RE.get_or_init(|| Regex::new(r"(?im)^\s*REASON:\s*(.+)$").unwrap());

    let m = vre.captures(text)?;
    let guilty = m.get(1)?.as_str().eq_ignore_ascii_case("GUILTY");

    let deliberation = strip_markers(text);
    let key_factor = marker_value(kre, text);
    // Prefer the model's one-line REASON for the on-screen remark; fall back to
    // the canned lines when it's absent.
    let remarks = marker_value(rre, text).unwrap_or_else(|| {
        if guilty { "Justice, as ever, is wet.".into() } else { "Acquitted. Do not let it happen again.".into() }
    });

    Some(Verdict {
        guilty,
        deliberation,
        remarks,
        key_factor,
        pre_announced: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn innocent_override_flips_guilty_and_scrubs_remarks() {
        let raw = "Empty and smug.\nVERDICT: GUILTY\nKEY_FACTOR: smugness\nREASON: Pure contempt.";
        let mut v = parse_verdict(raw).unwrap();
        assert!(v.guilty);
        assert!(apply_innocent_override(&mut v));
        assert!(!v.guilty);
        assert_eq!(v.remarks, fallbacks::verdicts::FORCED_ACQUITTAL_REMARKS);
        assert!(v.key_factor.is_none());
    }

    #[test]
    fn innocent_override_noops_on_acquittal() {
        let raw = "Surprisingly clever. VERDICT: ACQUITTED\nREASON: Charm.";
        let mut v = parse_verdict(raw).unwrap();
        let remarks_before = v.remarks.clone();
        assert!(!apply_innocent_override(&mut v));
        assert!(!v.guilty);
        assert_eq!(v.remarks, remarks_before);
    }

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

    #[test]
    fn parses_key_factor_and_reason() {
        let raw = "You owned it, friend.\nVERDICT: ACQUITTED\nKEY_FACTOR: sincere apology\nREASON: The apology was specific and real.";
        let v = parse_verdict(raw).unwrap();
        assert!(!v.guilty);
        assert_eq!(v.key_factor.as_deref(), Some("sincere apology"));
        assert_eq!(v.remarks, "The apology was specific and real.");
        // markers never leak into the spoken/displayed body
        assert!(!v.deliberation.contains("KEY_FACTOR"));
        assert!(!v.deliberation.contains("REASON"));
        assert!(v.deliberation.contains("owned it"));
    }

    #[test]
    fn key_factor_absent_is_none_and_remarks_fall_back() {
        let raw = "Empty and smug.\nVERDICT: GUILTY";
        let v = parse_verdict(raw).unwrap();
        assert!(v.guilty);
        assert!(v.key_factor.is_none());
        assert_eq!(v.remarks, "Justice, as ever, is wet.");
    }

    #[test]
    fn empty_key_factor_value_is_none() {
        let raw = "Weak.\nVERDICT: GUILTY\nKEY_FACTOR:   ";
        let v = parse_verdict(raw).unwrap();
        assert!(v.key_factor.is_none());
    }
}
