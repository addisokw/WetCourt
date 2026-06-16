use anyhow::{Context, Result};
use figment::{providers::{Env, Format, Toml}, Figment};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub inference: InferenceConfig,
    pub hardware: HardwareConfig,
    pub mock_hw: MockHwConfig,
    #[serde(default)]
    pub mock_inference: MockInferenceConfig,
    pub squirt: SquirtConfig,
    pub trial: TrialConfig,
    pub display: DisplayConfig,
    pub logging: LoggingConfig,
    #[serde(default = "d_default_persona_id")]
    pub default_persona_id: String,
    #[serde(default)]
    pub crimes: CrimesConfig,
}

fn d_default_persona_id() -> String { "wettington".into() }

#[derive(Debug, Deserialize, Clone)]
pub struct CrimesConfig {
    /// Crimes file, relative to the config file's directory.
    #[serde(default = "d_crimes_file")]
    pub file: String,
    /// "list" (default) draws charges from the curated file; "llm" restores
    /// the legacy on-the-fly generation. Operator-queued charges take
    /// precedence either way.
    #[serde(default = "d_crimes_source")]
    pub source: String,
    /// How many recent draws to avoid repeating.
    #[serde(default = "d_no_repeat_window")]
    pub no_repeat_window: usize,
}

impl Default for CrimesConfig {
    fn default() -> Self {
        Self {
            file: d_crimes_file(),
            source: d_crimes_source(),
            no_repeat_window: d_no_repeat_window(),
        }
    }
}

fn d_crimes_file() -> String { "crimes/wet_court_crimes.json".into() }
fn d_crimes_source() -> String { "list".into() }
fn d_no_repeat_window() -> usize { 15 }

#[derive(Debug, Deserialize, Clone)]
pub struct InferenceConfig {
    /// "real" hits LiteLLM at `base_url`. "mock" uses the canned latency-aware
    /// stand-ins from `mock_inference` and never opens a socket — useful for
    /// offline dev. Default: "real".
    #[serde(default = "d_mode")]
    pub mode: String,
    pub base_url: String,
    pub chat_model: String,
    pub stt_model: String,
    pub tts_model: String,
    pub tts_voice: String,
    pub charge_timeout_secs: u64,
    pub verdict_first_token_timeout_secs: u64,
    pub verdict_total_timeout_secs: u64,
    pub stt_timeout_secs: u64,
    pub tts_timeout_secs: u64,
    pub enable_thinking: bool,
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct HardwareConfig {
    pub driver: String,         // "mock" | "tcp" | "serial"
    pub serial_port: String,
    pub baud: u32,
    pub ack_timeout_ms: u64,
    /// Where to listen for the MCU's TCP connection when driver = "tcp".
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,
}

fn default_bind_addr() -> String {
    "0.0.0.0:8090".into()
}

#[derive(Debug, Deserialize, Clone)]
pub struct MockHwConfig {
    pub ack_latency_ms: u64,
    pub fail_rate: f64,
    pub simulate_estop_after_secs: u64,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct MockInferenceConfig {
    #[serde(default = "d_charge")]
    pub charge_latency_ms: u64,
    #[serde(default = "d_transcribe")]
    pub transcribe_latency_ms: u64,
    #[serde(default = "d_deliberate")]
    pub deliberate_latency_ms: u64,
    #[serde(default = "d_tts")]
    pub tts_latency_ms: u64,
}
fn d_mode() -> String { "real".into() }
fn d_charge() -> u64 { 800 }
fn d_transcribe() -> u64 { 400 }
fn d_deliberate() -> u64 { 1200 }
fn d_tts() -> u64 { 200 }

#[derive(Debug, Deserialize, Clone)]
pub struct SquirtConfig {
    /// The squirt gun is binary — every guilty verdict fires for this fixed
    /// duration (ms). There is no per-verdict intensity.
    pub duration_ms: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TrialConfig {
    pub plea_window_secs: u64,
    pub charge_display_secs: u64,
    pub cooldown_secs: u64,
    pub guilty_bias: f64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DisplayConfig {
    pub listen_addr: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LoggingConfig {
    pub level: String,
    #[allow(dead_code)]
    pub log_file: String,
    #[allow(dead_code)]
    pub transcripts_jsonl: String,
}

pub fn load(path: &Path) -> Result<Config> {
    Figment::new()
        .merge(Toml::file(path))
        .merge(Env::prefixed("BOOTH__").split("__"))
        .extract()
        .with_context(|| format!("loading config from {}", path.display()))
}
