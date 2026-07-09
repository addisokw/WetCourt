//! The lawyer on the line: VAD-endpointed turns of STT → chat → TTS until
//! hangup. No barge-in by design — the mixer keeps talking and inbound
//! frames just drop while we speak; DTMF and hangup are still honored.

use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::audio::vad::{EnergyVad, Vad};
use crate::audio::wav;
use crate::call::{context, ivr};
use crate::http::Shared;
use crate::inference::{Backend, ChatMessage, MOCK_REPLIES};
use crate::rtp::{g711, resample::Decimator, MixerHandle, RtpEvent, RtpSession};

pub async fn run(
    shared: &Shared,
    session: RtpSession,
    token: CancellationToken,
    opening_note: Option<String>,
) -> Result<()> {
    let RtpSession { mixer, mut events, recorder } = session;
    let cfg = &shared.cfg.audio;
    let persona = &shared.persona;
    let note = |kind: &str, detail: String| {
        if let Some(rec) = &recorder {
            rec.note(kind, detail);
        }
    };

    let mut vad = EnergyVad::new(cfg);
    let mut fallbacks = 0usize;

    // Inbound calls get the intake-menu gag (doubles as cover while the
    // trial context is fetched), then a brief hold — music broken by the
    // office voice announcing their absurd queue position. Ring-out calls
    // skip straight to the opener.
    let (snapshot, ivr_outcome) = if opening_note.is_none() {
        let r = tokio::join!(
            context::fetch(&shared.cfg.trial_context),
            ivr::menu(shared, &mixer, &mut events, &token)
        );
        ivr::hold_gag(shared, &mixer, &token).await;
        r
    } else {
        (context::fetch(&shared.cfg.trial_context).await, ivr::IvrOutcome::Skipped)
    };

    note(
        "case_file",
        snapshot
            .as_ref()
            .and_then(|s| s.charge.clone())
            .unwrap_or_else(|| "unavailable".into()),
    );
    let system = format!(
        "{}{}",
        persona.system_prompt,
        context::case_file_block(&snapshot)
    );
    let mut history = vec![ChatMessage::system(system)];
    if let Some(ivr_note) = ivr_outcome.history_note() {
        note("ivr", ivr_note.clone());
        history.push(ChatMessage::user(ivr_note));
    }

    // Opening line. Ring-out calls get a reason-seeded opener; inbound gets
    // the office greeting.
    let opening = match &opening_note {
        Some(reason) => {
            history.push(ChatMessage::user(format!(
                "(The lawyer is calling the client. Reason for the call: {reason}. Open the call.)"
            )));
            match chat_reply(shared, &history).await {
                Ok(r) => r,
                Err(_) => persona.greeting.clone(),
            }
        }
        None => persona.greeting.clone(),
    };
    note("lawyer", opening.clone());
    speak(shared, &mixer, &opening).await.ok();
    history.push(ChatMessage::assistant(opening));
    flush_stale(&mut events, &mut vad);

    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(cfg.max_call_secs);
    let silence_limit = cfg.silence_reprompt_secs * 1000;
    let mut reprompted = false;

    loop {
        tokio::select! {
            _ = token.cancelled() => break,
            _ = tokio::time::sleep_until(deadline) => {
                tracing::info!("max_call_secs reached — signing off");
                note("lawyer", persona.signoff.clone());
                speak(shared, &mixer, &persona.signoff).await.ok();
                break;
            }
            ev = events.recv() => match ev {
                None => break,
                Some(RtpEvent::Dtmf(digit)) => {
                    tracing::info!(%digit, "mid-call DTMF");
                }
                Some(RtpEvent::Audio(frame)) => {
                    if let Some(utterance) = vad.push(&frame) {
                        take_turn(
                            shared,
                            &mixer,
                            &mut history,
                            utterance,
                            &mut fallbacks,
                            recorder.as_ref(),
                        )
                        .await;
                        flush_stale(&mut events, &mut vad);
                        reprompted = false;
                    } else if vad.idle_ms() >= silence_limit {
                        if reprompted {
                            tracing::info!("client stayed silent — signing off");
                            note("lawyer", persona.signoff.clone());
                            speak(shared, &mixer, &persona.signoff).await.ok();
                            break;
                        }
                        note("lawyer", persona.reprompt.clone());
                        speak(shared, &mixer, &persona.reprompt).await.ok();
                        history.push(ChatMessage::assistant(persona.reprompt.clone()));
                        flush_stale(&mut events, &mut vad);
                        reprompted = true;
                    }
                }
            }
        }
    }
    Ok(())
}

/// One conversational turn. Inference failures become in-character fallback
/// lines — the phone never goes dead silent on an error.
async fn take_turn(
    shared: &Shared,
    mixer: &MixerHandle,
    history: &mut Vec<ChatMessage>,
    utterance: Vec<i16>,
    fallbacks: &mut usize,
    recorder: Option<&std::sync::Arc<crate::recorder::CallRecorder>>,
) {
    let icfg = &shared.cfg.inference;
    mixer.set_cover(shared.cover.thinking.clone());

    let reply = async {
        let text = match &shared.backend {
            Backend::Real(client) => {
                let wav = wav::wrap_pcm_8k(&utterance);
                client
                    .transcribe(wav, Duration::from_secs(icfg.stt_timeout_secs))
                    .await?
            }
            Backend::Mock { .. } => "I have been unjustly accused.".to_string(),
        };
        if text.is_empty() {
            anyhow::bail!("empty transcript");
        }
        tracing::info!(client = %text, "transcript");
        if let Some(rec) = recorder {
            rec.note("caller", text.clone());
        }
        history.push(ChatMessage::user(text));
        chat_reply(shared, history).await
    }
    .await;

    let line = match reply {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("turn failed, using fallback: {e:#}");
            *fallbacks += 1;
            shared.persona.fallback(*fallbacks).to_string()
        }
    };
    tracing::info!(lawyer = %line, "reply");
    if let Some(rec) = recorder {
        rec.note("lawyer", line.clone());
    }
    history.push(ChatMessage::assistant(line.clone()));
    if let Err(e) = speak(shared, mixer, &line).await {
        tracing::warn!("tts failed: {e:#}");
        mixer.set_cover(None);
    }
}

async fn chat_reply(shared: &Shared, history: &[ChatMessage]) -> Result<String> {
    let icfg = &shared.cfg.inference;
    match &shared.backend {
        Backend::Real(client) => {
            client
                .chat(history, 200, Duration::from_secs(icfg.chat_timeout_secs))
                .await
        }
        Backend::Mock { turn } => {
            let n = turn.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(MOCK_REPLIES[n % MOCK_REPLIES.len()].to_string())
        }
    }
}

/// Synthesize `text` and play it out, blocking until the line drains.
/// Clears any cover loop first so speech starts clean.
pub async fn speak(shared: &Shared, mixer: &MixerHandle, text: &str) -> Result<()> {
    mixer.set_cover(None);
    synth_to_queue(
        shared,
        mixer,
        text,
        &shared.persona.tts_voice,
        shared.persona.tts_speed,
    )
    .await?;
    mixer.wait_drained().await;
    Ok(())
}

/// Synthesize into the speech queue without touching the cover loop or
/// waiting for drain — speech preempts any cover the moment bytes land, so
/// callers can keep hold music running while synthesis is in flight.
pub(super) async fn synth_to_queue(
    shared: &Shared,
    mixer: &MixerHandle,
    text: &str,
    voice: &str,
    speed: Option<f32>,
) -> Result<()> {
    let icfg = &shared.cfg.inference;
    match &shared.backend {
        Backend::Real(client) => {
            let stream = client
                .synth_pcm_stream(
                    text,
                    voice,
                    speed,
                    Duration::from_secs(icfg.tts_timeout_secs),
                )
                .await
                .context("starting tts stream")?;
            tokio::pin!(stream);

            let mut decimator = Decimator::new();
            let mut leftover: Option<u8> = None;
            let total_timeout = Duration::from_secs(icfg.tts_timeout_secs * 4);
            let started = tokio::time::Instant::now();

            while let Some(chunk) = tokio::time::timeout(total_timeout, stream.next())
                .await
                .map_err(|_| anyhow::anyhow!("tts stream stalled"))?
            {
                if started.elapsed() > total_timeout {
                    anyhow::bail!("tts stream exceeded total timeout");
                }
                let chunk = chunk?;
                queue_pcm24(mixer, &mut decimator, &mut leftover, &chunk);
            }
        }
        Backend::Mock { .. } => {
            let pcm = crate::inference::mock_tts_pcm(1.2);
            let mut decimator = Decimator::new();
            let mut leftover: Option<u8> = None;
            queue_pcm24(mixer, &mut decimator, &mut leftover, &pcm);
        }
    }
    Ok(())
}

/// 24 kHz s16le bytes → 8 kHz µ-law into the mixer, carrying an odd byte
/// across chunk seams.
fn queue_pcm24(
    mixer: &MixerHandle,
    decimator: &mut Decimator,
    leftover: &mut Option<u8>,
    chunk: &[u8],
) {
    let mut bytes: Vec<u8> = Vec::with_capacity(chunk.len() + 1);
    if let Some(b) = leftover.take() {
        bytes.push(b);
    }
    bytes.extend_from_slice(chunk);
    if bytes.len() % 2 == 1 {
        *leftover = bytes.pop();
    }
    let samples: Vec<i16> = bytes
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    if samples.is_empty() {
        return;
    }
    let down = decimator.process(&samples);
    if !down.is_empty() {
        mixer.queue_speech(&g711::encode(&down));
    }
}

/// Drop frames that queued up while we were busy speaking, and reset the
/// endpointer so half-heard audio doesn't leak into the next turn.
fn flush_stale(events: &mut tokio::sync::mpsc::Receiver<RtpEvent>, vad: &mut EnergyVad) {
    while events.try_recv().is_ok() {}
    vad.reset();
}

