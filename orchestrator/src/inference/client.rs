use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::{multipart, Client};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

use crate::config::InferenceConfig;

#[derive(Clone)]
pub struct LlmClient {
    http: Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
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
            model: cfg.chat_model.clone(),
            enable_thinking: cfg.enable_thinking,
            stt_model: cfg.stt_model.clone(),
            tts_model: cfg.tts_model.clone(),
        }
    }

    /// Multipart upload to /v1/audio/transcriptions. Returns transcribed text.
    pub async fn transcribe(&self, audio: Bytes, filename: &str, timeout: Duration) -> Result<String> {
        let part = multipart::Part::bytes(audio.to_vec())
            .file_name(filename.to_string())
            .mime_str("application/octet-stream")?;
        let form = multipart::Form::new()
            .text("model", self.stt_model.clone())
            .part("file", part);
        let req = self.build(reqwest::Method::POST, "/audio/transcriptions").multipart(form);
        let resp: Value = tokio::time::timeout(timeout, async {
            let r = req.send().await?.error_for_status()?;
            anyhow::Ok(r.json::<Value>().await?)
        })
        .await
        .map_err(|_| anyhow!("transcribe timeout after {:?}", timeout))??;
        Ok(resp["text"].as_str().unwrap_or("").trim().to_string())
    }

    /// Streaming /v1/audio/speech. Yields raw PCM s16le @ 24kHz bytes as they
    /// arrive from Kokoro. Caller wraps each chunk into a frontend binary frame.
    ///
    /// Retries once on connection-closed errors — Kokoro/LiteLLM closes idle
    /// keep-alive sockets after a few seconds, but reqwest may hand back a
    /// pooled connection that's already half-closed. The first send fails
    /// with "connection closed before message completed"; the retry lands on
    /// a fresh connection.
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
        // Per-persona speaking rate (Kokoro supports 0.5–2.0; omit = 1.0).
        if let Some(sp) = speed {
            body["speed"] = json!(sp);
        }
        let send_once = || async {
            let req = self.build(reqwest::Method::POST, "/audio/speech").json(&body);
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
        let resp = resp.error_for_status()?;
        let stream = resp.bytes_stream().map(|r| r.map_err(|e| anyhow!(e)));
        Ok(stream)
    }

    fn url(&self, path: &str) -> String {
        format!("{}/v1{}", self.base_url, path)
    }

    /// Non-streaming structured completion. Returns the parsed `T` extracted
    /// from `choices[0].message.content` as JSON.
    pub async fn chat_structured<T: DeserializeOwned>(
        &self,
        system: &str,
        user: &str,
        schema: Value,
        timeout: Duration,
    ) -> Result<T> {
        let body = json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
            "response_format": { "type": "json_schema", "json_schema": {"name": "out", "schema": schema} },
            "chat_template_kwargs": { "enable_thinking": self.enable_thinking },
            "temperature": 0.9,
            "max_tokens": 1024,
        });
        let resp = tokio::time::timeout(timeout, self.post_json("/chat/completions", &body))
            .await
            .map_err(|_| anyhow!("chat_structured timeout after {:?}", timeout))??;
        let content = resp["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow!("missing choices[0].message.content"))?;
        serde_json::from_str(content)
            .with_context(|| format!("parsing structured content: {content}"))
    }

    /// Streaming completion. The returned stream yields delta `content` strings
    /// as they arrive. Caller is responsible for accumulating and parsing.
    /// `first_token_timeout` aborts if no `content` delta is seen in time;
    /// `total_timeout` caps the whole stream.
    pub async fn chat_stream(
        &self,
        system: &str,
        user: &str,
        temperature: f64,
        first_token_timeout: Duration,
        total_timeout: Duration,
    ) -> Result<impl Stream<Item = Result<String>>> {
        let body = json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
            "stream": true,
            "chat_template_kwargs": { "enable_thinking": self.enable_thinking },
            "temperature": temperature,
            "max_tokens": 4096,
        });

        let req = self.build(reqwest::Method::POST, "/chat/completions").json(&body);
        let resp = tokio::time::timeout(Duration::from_secs(10), req.send())
            .await
            .map_err(|_| anyhow!("chat_stream connect timeout"))??;
        let resp = ensure_ok(resp).await?;

        let started = tokio::time::Instant::now();
        let mut bytes = resp.bytes_stream();
        let mut buf = String::new();
        let mut first_token_seen = false;

        let stream = async_stream::try_stream! {
            loop {
                if started.elapsed() > total_timeout {
                    Err(anyhow!("chat_stream total timeout after {:?}", total_timeout))?;
                }
                let next_to = if first_token_seen { total_timeout } else { first_token_timeout };
                let elapsed = started.elapsed();
                let remaining = next_to.checked_sub(elapsed).unwrap_or(Duration::from_millis(1));

                let chunk = match tokio::time::timeout(remaining, bytes.next()).await {
                    Err(_) if !first_token_seen => Err(anyhow!("first-token timeout"))?,
                    Err(_) => Err(anyhow!("read timeout"))?,
                    Ok(None) => break,
                    Ok(Some(Err(e))) => Err(e)?,
                    Ok(Some(Ok(b))) => b,
                };

                buf.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(idx) = buf.find("\n\n") {
                    let event: String = buf.drain(..idx + 2).collect();
                    for line in event.lines() {
                        let Some(payload) = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:")) else { continue };
                        let payload = payload.trim();
                        if payload.is_empty() || payload == "[DONE]" { continue; }
                        let v: Value = match serde_json::from_str(payload) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        if let Some(content) = v["choices"][0]["delta"]["content"].as_str() {
                            if !content.is_empty() {
                                first_token_seen = true;
                                yield content.to_string();
                            }
                        }
                    }
                }
            }
        };
        Ok(stream)
    }

    async fn post_json<T: Serialize>(&self, path: &str, body: &T) -> Result<Value> {
        let resp = self.build(reqwest::Method::POST, path)
            .json(body)
            .send()
            .await?;
        let resp = ensure_ok(resp).await?;
        Ok(resp.json().await?)
    }

    fn build(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut req = self.http.request(method, self.url(path));
        if let Some(k) = &self.api_key {
            req = req.bearer_auth(k);
        }
        req
    }
}

/// Turn a non-2xx response into an error that includes the response body — the
/// inference gateway (LiteLLM/vLLM) puts the actual reason there (bad param,
/// context-length overflow, unknown model), which `error_for_status` discards.
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

/// Match the reqwest/hyper error patterns that indicate the pooled keep-alive
/// connection was closed by the peer before our request reached it. These are
/// safe to retry idempotently on the first send (no bytes of the body were
/// committed to the wire from the server's perspective).
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
