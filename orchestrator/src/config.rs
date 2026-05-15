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
    pub squirt_intensity: SquirtIntensity,
    pub trial: TrialConfig,
    pub display: DisplayConfig,
    pub logging: LoggingConfig,
}

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
pub struct SquirtIntensity {
    pub level_1: u32,
    pub level_2: u32,
    pub level_3: u32,
    pub level_4: u32,
    pub level_5: u32,
}

impl SquirtIntensity {
    pub fn duration_ms(&self, intensity: u8) -> u32 {
        match intensity.clamp(1, 5) {
            1 => self.level_1,
            2 => self.level_2,
            3 => self.level_3,
            4 => self.level_4,
            _ => self.level_5,
        }
    }
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
