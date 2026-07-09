//! LiteLLM client — a deliberate copy-trim of the orchestrator's
//! `src/inference/client.rs` (transcribe / speech / stale-keep-alive retry /
//! error-body surfacing), plus a multi-turn `chat` the booth doesn't need.
//! Kept separate so the lawyer line degrades independently of the trial loop.

use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::{multipart, Client};
use serde::Serialize;
use serde_json::{json, Value};

use crate::config::InferenceConfig;

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: &'static str,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system", content: content.into() }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user", content: content.into() }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant", content: content.into() }
    }
}

pub enum Backend {
    Real(LlmClient),
    /// Offline dev: canned transcript/replies, tone TTS.
    Mock { turn: std::sync::atomic::AtomicUsize },
}

impl Backend {
    pub fn from_config(cfg: &InferenceConfig) -> Self {
        if cfg.mode == "mock" {
            tracing::warn!("inference mode = mock — canned lawyer, tone TTS");
            Backend::Mock { turn: Default::default() }
        } else {
            Backend::Real(LlmClient::new(cfg))
        }
    }
}

#[derive(Clone)]
pub struct LlmClient {
    http: Client,
    base_url: String,
    api_key: Option<String>,
    chat_model: String,
    enable_thinking: bool,
    stt_model: String,
    tts_model: String,
}

impl LlmClient {
    pub fn new(cfg: &InferenceConfig) -> Self {
        let http = Client::builder()
            .pool_idle_timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest client");
        Self {
            http,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            api_key: cfg.api_key.clone(),
            chat_model: cfg.chat_model.clone(),
            enable_thinking: cfg.enable_thinking,
            stt_model: cfg.stt_model.clone(),
            tts_model: cfg.tts_model.clone(),
        }
    }

    /// Multipart upload to /v1/audio/transcriptions. Returns transcribed text.
    pub async fn transcribe(&self, wav: Vec<u8>, timeout: Duration) -> Result<String> {
        let part = multipart::Part::bytes(wav)
            .file_name("call.wav")
            .mime_str("application/octet-stream")?;
        let form = multipart::Form::new()
            .text("model", self.stt_model.clone())
            .part("file", part);
        let req = self
            .build(reqwest::Method::POST, "/audio/transcriptions")
            .multipart(form);
        let resp: Value = tokio::time::timeout(timeout, async {
            let r = req.send().await?;
            let r = ensure_ok(r).await?;
            anyhow::Ok(r.json::<Value>().await?)
        })
        .await
        .map_err(|_| anyhow!("transcribe timeout after {timeout:?}"))??;
        Ok(resp["text"].as_str().unwrap_or("").trim().to_string())
    }

    /// Non-streaming multi-turn completion; returns assistant text.
    pub async fn chat(
        &self,
        messages: &[ChatMessage],
        max_tokens: u32,
        timeout: Duration,
    ) -> Result<String> {
        let body = json!({
            "model": self.chat_model,
            "messages": messages,
            "chat_template_kwargs": { "enable_thinking": self.enable_thinking },
            "temperature": 0.9,
            "max_tokens": max_tokens,
        });
        let resp: Value = tokio::time::timeout(timeout, async {
            let r = self
                .build(reqwest::Method::POST, "/chat/completions")
                .json(&body)
                .send()
                .await?;
            let r = ensure_ok(r).await?;
            anyhow::Ok(r.json::<Value>().await?)
        })
        .await
        .map_err(|_| anyhow!("chat timeout after {timeout:?}"))??;
        let content = resp["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow!("missing choices[0].message.content"))?;
        Ok(content.trim().to_string())
    }

    /// Streaming /v1/audio/speech: raw PCM s16le @ 24 kHz mono from Kokoro.
    /// Retries once on a stale pooled keep-alive connection.
    pub async fn synth_pcm_stream(
        &self,
        text: &str,
        voice: &str,
        speed: Option<f32>,
        connect_timeout: Duration,
    ) -> Result<impl Stream<Item = Result<Bytes>>> {
        let mut body = json!({
            "model": self.tts_model,
            "voice": voice,
            "input": text,
            "response_format": "pcm",
        });
        if let Some(s) = speed {
            body["speed"] = json!(s);
        }
        let send_once = || async {
            let req = self
                .build(reqwest::Method::POST, "/audio/speech")
                .json(&body);
            tokio::time::timeout(connect_timeout, req.send())
                .await
                .map_err(|_| anyhow!("tts connect timeout"))?
                .map_err(anyhow::Error::from)
        };
        let resp = match send_once().await {
            Ok(r) => r,
            Err(e) if is_stale_connection(&e) => {
                tracing::debug!("tts: stale keep-alive, retrying once: {e:#}");
                send_once().await?
            }
            Err(e) => return Err(e),
        };
        let resp = ensure_ok(resp).await?;
        Ok(resp.bytes_stream().map(|r| r.map_err(|e| anyhow!(e))))
    }

    fn build(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut req = self
            .http
            .request(method, format!("{}/v1{}", self.base_url, path));
        if let Some(k) = &self.api_key {
            req = req.bearer_auth(k);
        }
        req
    }
}

/// Non-2xx → error carrying the response body; LiteLLM puts the actual
/// reason there and `error_for_status` would discard it.
async fn ensure_ok(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    let snippet: String = body.trim().chars().take(1000).collect();
    if snippet.is_empty() {
        bail!("inference API returned HTTP {status}");
    }
    bail!("inference API returned HTTP {status}: {snippet}")
}

/// Pooled keep-alive closed by the peer before our request hit the wire —
/// safe to retry the first send.
fn is_stale_connection(err: &anyhow::Error) -> bool {
    let mut src: Option<&dyn std::error::Error> = Some(err.as_ref());
    while let Some(e) = src {
        let s = e.to_string().to_lowercase();
        if s.contains("connection closed before message completed")
            || s.contains("sendrequest")
            || s.contains("broken pipe")
            || s.contains("connection reset")
        {
            return true;
        }
        src = e.source();
    }
    false
}

// ---- Mock backend helpers ----

pub const MOCK_REPLIES: &[&str] = &[
    "Ah yes, I have your file right here. Well, a file. It's a menu, but the principle stands.",
    "My advice is to plead guilty to a crime you did not commit. Judges love the honesty.",
    "I once got a client off with time served. The time was forty years, but still, served.",
];

/// 24 kHz s16le PCM tone, for exercising the TTS path offline.
pub fn mock_tts_pcm(seconds: f32) -> Vec<u8> {
    let n = (24000.0 * seconds) as usize;
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let s = (8000.0
            * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 24000.0).sin())
            as i16;
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}
