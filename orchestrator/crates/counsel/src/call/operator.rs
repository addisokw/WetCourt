//! The phone-keypad operator console.
//!
//! With the court idle, the operator lifts the handset (which normally starts
//! the Dewey gag call) and presses `#`. The line falls silent, and digits
//! keyed until a `#`/inter-digit-timeout commit form a mode code (`#42`) that
//! is POSTed to the orchestrator's `/operator/modes/arm`. The orchestrator is
//! the authority: it validates the code against its registry, enforces
//! idle-only arming, and drives the case monitor's discreet confirmation.
//! counsel just transmits and plays accept/reject tones on the handset.
//!
//! If the court is mid-trial (the orchestrator answers 409), the console
//! bows out and hands the call back to the lawyer flow — a defendant who
//! fat-fingers `#` during a consult loses nothing.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::http::Shared;
use crate::rtp::{g711, MixerHandle, RtpEvent};

// ---- Code grammar (pure, unit-tested) ----

#[derive(Debug, PartialEq)]
pub enum KeyResult {
    /// Digits accumulating; nothing to do yet.
    Pending,
    /// A complete code was keyed.
    Commit(u16),
    /// `*` with no pending digits: clear every armed mode.
    ClearAll,
    /// `*` with pending digits: scrap the entry, keep listening.
    CancelEntry,
    /// Nonsense press (`#` with nothing pending, letters, overlong code).
    Invalid,
}

/// Accumulates keypad digits into mode codes. `#` commits, `*` cancels (or
/// clears all armed modes when nothing is pending), the inter-digit timeout
/// commits from the console loop.
#[derive(Default)]
pub struct CodeCollector {
    digits: String,
    max_len: usize,
}

impl CodeCollector {
    pub fn new(max_len: usize) -> Self {
        Self { digits: String::new(), max_len: max_len.max(1) }
    }

    pub fn has_pending(&self) -> bool {
        !self.digits.is_empty()
    }

    pub fn on_key(&mut self, c: char) -> KeyResult {
        match c {
            '0'..='9' => {
                if self.digits.len() >= self.max_len {
                    self.digits.clear();
                    return KeyResult::Invalid;
                }
                self.digits.push(c);
                KeyResult::Pending
            }
            '#' => self.commit_pending(KeyResult::Invalid),
            '*' => {
                if self.digits.is_empty() {
                    KeyResult::ClearAll
                } else {
                    self.digits.clear();
                    KeyResult::CancelEntry
                }
            }
            _ => {
                self.digits.clear();
                KeyResult::Invalid
            }
        }
    }

    /// Inter-digit timeout: commit whatever is pending, or report the entry
    /// cancelled if the operator just went quiet with nothing keyed.
    pub fn on_timeout(&mut self) -> KeyResult {
        self.commit_pending(KeyResult::CancelEntry)
    }

    fn commit_pending(&mut self, if_empty: KeyResult) -> KeyResult {
        if self.digits.is_empty() {
            return if_empty;
        }
        let code = self.digits.parse::<u16>();
        self.digits.clear();
        match code {
            Ok(c) => KeyResult::Commit(c),
            Err(_) => KeyResult::Invalid, // unreachable for ≤4 digits, but total
        }
    }
}

// ---- Handset feedback tones (8 kHz sine → µ-law, straight into the mixer) ----

fn tone_ulaw(freq_hz: f32, ms: u64, amplitude: f32) -> Vec<u8> {
    let n = (8 * ms) as usize; // 8 samples per ms at 8 kHz
    let samples: Vec<i16> = (0..n)
        .map(|i| {
            let t = i as f32 / 8000.0;
            (amplitude * (2.0 * std::f32::consts::PI * freq_hz * t).sin()) as i16
        })
        .collect();
    g711::encode(&samples)
}

const SILENCE_GAP_MS: u64 = 60;

fn silence_ulaw(ms: u64) -> Vec<u8> {
    vec![g711::ULAW_SILENCE; (8 * ms) as usize]
}

/// Console-ready chirp: one mid tone.
fn play_ready(mixer: &MixerHandle) {
    mixer.queue_speech(&tone_ulaw(880.0, 120, 9000.0));
}

/// Accepted: rising two-note.
fn play_accept(mixer: &MixerHandle) {
    let mut pcm = tone_ulaw(660.0, 110, 9000.0);
    pcm.extend(silence_ulaw(SILENCE_GAP_MS));
    pcm.extend(tone_ulaw(990.0, 150, 9000.0));
    mixer.queue_speech(&pcm);
}

/// Rejected: low double buzz.
fn play_reject(mixer: &MixerHandle) {
    let mut pcm = tone_ulaw(220.0, 160, 9000.0);
    pcm.extend(silence_ulaw(SILENCE_GAP_MS));
    pcm.extend(tone_ulaw(220.0, 160, 9000.0));
    mixer.queue_speech(&pcm);
}

/// Short blip for a scrapped/invalid entry.
fn play_blip(mixer: &MixerHandle) {
    mixer.queue_speech(&tone_ulaw(330.0, 80, 7000.0));
}

// ---- Orchestrator client ----

#[derive(Debug, PartialEq)]
pub enum ArmResult {
    Armed,
    /// 409 — the court is mid-trial; arming is idle-only.
    NotIdle,
    /// 404 — code not in the registry.
    Unknown,
    Unreachable,
}

async fn arm_code(base_url: &str, code: u16) -> ArmResult {
    let url = format!("{}/operator/modes/arm", base_url.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .post(&url)
        .timeout(Duration::from_secs(2))
        .json(&serde_json::json!({ "code": code }))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => ArmResult::Armed,
        Ok(r) if r.status() == reqwest::StatusCode::CONFLICT => ArmResult::NotIdle,
        Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => ArmResult::Unknown,
        Ok(r) => {
            tracing::warn!(status = %r.status(), "operator arm: unexpected status");
            ArmResult::Unreachable
        }
        Err(e) => {
            tracing::warn!("operator arm: orchestrator unreachable: {e:#}");
            ArmResult::Unreachable
        }
    }
}

async fn clear_armed(base_url: &str) -> bool {
    let url = format!("{}/operator/modes/clear", base_url.trim_end_matches('/'));
    matches!(
        reqwest::Client::new()
            .post(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await,
        Ok(r) if r.status().is_success()
    )
}

// ---- The console loop ----

#[derive(Debug, PartialEq)]
pub enum ConsoleOutcome {
    /// Hand the call back to the lawyer flow (mid-trial `#`, or orchestrator
    /// unreachable before anything was armed).
    Resume,
    /// Hangup / cancellation — the caller tears the call down.
    Ended,
}

/// Run the operator console until hangup (or bow out with `Resume`). Entered
/// after a `#` press; silences the line and collects codes.
pub async fn console(
    shared: &Shared,
    mixer: &MixerHandle,
    events: &mut mpsc::Receiver<RtpEvent>,
    token: &CancellationToken,
) -> ConsoleOutcome {
    let cfg = &shared.cfg.operator;
    let base = &shared.cfg.trial_context.orchestrator_base_url;
    tracing::info!("operator console opened ('#' pressed)");

    // Dewey, the IVR prompt, and any hold music stop mid-word; the operator
    // gets a clean quiet line with a ready chirp.
    mixer.clear_speech();
    mixer.set_cover(None);
    play_ready(mixer);

    let mut collector = CodeCollector::new(cfg.max_code_len);
    let inter_digit = Duration::from_millis(cfg.inter_digit_ms.max(250));
    // True once any code was accepted this call — after that, a transient
    // orchestrator failure keeps us in the console instead of resurrecting
    // Dewey mid-session.
    let mut latched = false;

    loop {
        let key = tokio::select! {
            _ = token.cancelled() => return ConsoleOutcome::Ended,
            ev = events.recv() => match ev {
                None => return ConsoleOutcome::Ended,
                Some(RtpEvent::Dtmf(c)) => collector.on_key(c),
                Some(RtpEvent::Audio(_)) => continue,
            },
            _ = tokio::time::sleep(inter_digit), if collector.has_pending() => {
                collector.on_timeout()
            }
        };

        tracing::debug!(?key, pending = collector.has_pending(), "operator console key");
        match key {
            KeyResult::Pending => {}
            KeyResult::Commit(code) => match arm_code(base, code).await {
                ArmResult::Armed => {
                    tracing::info!(code, "operator mode armed via phone");
                    latched = true;
                    play_accept(mixer);
                }
                ArmResult::NotIdle => {
                    tracing::info!(code, "operator arm rejected: court not idle");
                    play_reject(mixer);
                    if !latched {
                        // Almost certainly the defendant fat-fingering '#'
                        // mid-consult — give them their lawyer back.
                        return ConsoleOutcome::Resume;
                    }
                }
                ArmResult::Unknown => {
                    tracing::warn!(code, "operator arm rejected: unknown code");
                    play_reject(mixer);
                }
                ArmResult::Unreachable => {
                    play_reject(mixer);
                    if !latched {
                        return ConsoleOutcome::Resume;
                    }
                }
            },
            KeyResult::ClearAll => {
                if clear_armed(base).await {
                    tracing::info!("operator modes cleared via phone");
                    play_accept(mixer);
                } else {
                    play_reject(mixer);
                }
            }
            KeyResult::CancelEntry | KeyResult::Invalid => play_blip(mixer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::post, Json, Router};
    use serde_json::Value;

    fn collect(keys: &str) -> Vec<KeyResult> {
        let mut c = CodeCollector::new(4);
        keys.chars().map(|k| c.on_key(k)).collect()
    }

    #[test]
    fn pound_terminated_code_commits() {
        assert_eq!(collect("42#").pop(), Some(KeyResult::Commit(42)));
    }

    #[test]
    fn timeout_commits_pending_digits() {
        let mut c = CodeCollector::new(4);
        c.on_key('4');
        c.on_key('2');
        assert_eq!(c.on_timeout(), KeyResult::Commit(42));
        assert!(!c.has_pending());
    }

    #[test]
    fn timeout_with_nothing_pending_is_cancel() {
        let mut c = CodeCollector::new(4);
        assert_eq!(c.on_timeout(), KeyResult::CancelEntry);
    }

    #[test]
    fn star_cancels_pending_entry() {
        let mut c = CodeCollector::new(4);
        c.on_key('9');
        assert_eq!(c.on_key('*'), KeyResult::CancelEntry);
        assert_eq!(c.on_key('*'), KeyResult::ClearAll);
    }

    #[test]
    fn bare_pound_is_invalid() {
        assert_eq!(collect("#").pop(), Some(KeyResult::Invalid));
    }

    #[test]
    fn overlong_code_is_invalid_and_resets() {
        let mut c = CodeCollector::new(4);
        for k in "1234".chars() {
            assert_eq!(c.on_key(k), KeyResult::Pending);
        }
        assert_eq!(c.on_key('5'), KeyResult::Invalid);
        assert!(!c.has_pending());
    }

    #[test]
    fn letters_are_invalid() {
        let mut c = CodeCollector::new(4);
        c.on_key('4');
        assert_eq!(c.on_key('A'), KeyResult::Invalid);
        assert!(!c.has_pending());
    }

    #[test]
    fn tone_length_matches_duration() {
        assert_eq!(tone_ulaw(440.0, 100, 8000.0).len(), 800); // 100 ms @ 8 kHz
    }

    /// arm_code end-to-end against a real HTTP server: status codes map to the
    /// right ArmResult variants.
    #[tokio::test]
    async fn arm_code_maps_statuses() {
        use axum::http::StatusCode;
        let app = Router::new().route(
            "/operator/modes/arm",
            post(|Json(v): Json<Value>| async move {
                match v["code"].as_u64() {
                    Some(42) => StatusCode::OK,
                    Some(9) => StatusCode::CONFLICT,
                    _ => StatusCode::NOT_FOUND,
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base = format!("http://{addr}");

        assert_eq!(arm_code(&base, 42).await, ArmResult::Armed);
        assert_eq!(arm_code(&base, 9).await, ArmResult::NotIdle);
        assert_eq!(arm_code(&base, 7).await, ArmResult::Unknown);
        assert_eq!(arm_code("http://127.0.0.1:1", 42).await, ArmResult::Unreachable);
    }
}
