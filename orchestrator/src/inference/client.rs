use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures_util::{Stream, StreamExt};
use reqwest::Client;
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
        }
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
            "temperature": 0.9,
            "max_tokens": 4096,
        });

        let req = self.build(reqwest::Method::POST, "/chat/completions").json(&body);
        let resp = tokio::time::timeout(Duration::from_secs(10), req.send())
            .await
            .map_err(|_| anyhow!("chat_stream connect timeout"))??
            .error_for_status()?;

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
            .await?
            .error_for_status()?;
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
