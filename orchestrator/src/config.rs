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
    #[serde(default)]
    pub cross_examination: CrossExamConfig,
    pub display: DisplayConfig,
    pub logging: LoggingConfig,
    #[serde(default = "d_default_persona_id")]
    pub default_persona_id: String,
    #[serde(default)]
    pub crimes: CrimesConfig,
    #[serde(default)]
    pub printer: PrinterConfig,
    #[serde(default)]
    pub vision: VisionConfig,
    #[serde(default)]
    pub capture: CaptureConfig,
    #[serde(default)]
    pub lawyer: LawyerConfig,
}

/// The call-your-lawyer phone service (`counsel`). The orchestrator
/// reverse-proxies its status/ring-out at `/lawyer/*` (console stays
/// same-origin) and serves it a read-only trial snapshot at `/trial/state`.
#[derive(Debug, Deserialize, Clone)]
pub struct LawyerConfig {
    #[serde(default = "d_lawyer_base_url")]
    pub base_url: String,
}

impl Default for LawyerConfig {
    fn default() -> Self {
        Self { base_url: d_lawyer_base_url() }
    }
}

fn d_lawyer_base_url() -> String {
    "http://localhost:8092".into()
}

#[derive(Debug, Deserialize, Clone)]
pub struct VisionConfig {
    /// Base URL of the turret vision process (serves /feed and /state). The
    /// orchestrator reverse-proxies these at /vision/* so the console stays
    /// same-origin (works through the tunnel for remote operators). Dev: the
    /// booth PC's localhost; prod: the vision container on the Spark.
    #[serde(default = "d_vision_base_url")]
    pub base_url: String,
    /// Drive the turret during trials: on deliberation, arm targeting so the gun
    /// visibly acquires a lock on the defendant (suspense) before the verdict;
    /// on guilty it freezes on the target and fires; every trial starts and ends
    /// with the gun at idle. Off = the FSM never touches targeting (the gun stays
    /// static and a guilty verdict fires straight, ungated — the older behaviour).
    #[serde(default = "d_trial_targeting")]
    pub trial_targeting: bool,
}

impl Default for VisionConfig {
    fn default() -> Self {
        Self { base_url: d_vision_base_url(), trial_targeting: d_trial_targeting() }
    }
}

fn d_vision_base_url() -> String {
    "http://localhost:8091".into()
}

fn d_trial_targeting() -> bool {
    true
}

/// The guilty "moment of justice" capture: on a guilty verdict the orchestrator
/// grabs a short burst of un-annotated frames from the vision service (`/clean`)
/// around the blast, saves them under `dir/<case>/`, and dithers one onto the
/// keepsake receipt. Off = the receipt keeps its placeholder still.
#[derive(Debug, Deserialize, Clone)]
pub struct CaptureConfig {
    #[serde(default = "d_capture_enabled")]
    pub enabled: bool,
    /// Base directory for saved bursts; one `<case_label>/` subdir per trial.
    #[serde(default = "d_capture_dir")]
    pub dir: String,
    /// Delay after entering the sentence before the first grab (ms) — lets the
    /// FIRE go out and the water reach the defendant.
    #[serde(default = "d_capture_delay_ms")]
    pub fire_delay_ms: u64,
    /// How many frames to grab.
    #[serde(default = "d_capture_frames")]
    pub frames: u32,
    /// Spacing between grabs (ms). frames × interval spans the burst window.
    #[serde(default = "d_capture_interval_ms")]
    pub interval_ms: u64,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            enabled: d_capture_enabled(),
            dir: d_capture_dir(),
            fire_delay_ms: d_capture_delay_ms(),
            frames: d_capture_frames(),
            interval_ms: d_capture_interval_ms(),
        }
    }
}

fn d_capture_enabled() -> bool { true }
fn d_capture_dir() -> String { "captures".into() }
fn d_capture_delay_ms() -> u64 { 250 }
fn d_capture_frames() -> u32 { 8 }
fn d_capture_interval_ms() -> u64 { 110 }

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

/// Thermal-printer keepsake output. The casebook trial log
/// (`[logging] transcripts_jsonl`) is written regardless of `mode`; this only
/// governs the physical receipt.
#[derive(Debug, Deserialize, Clone)]
pub struct PrinterConfig {
    /// "off" (no receipt), "mock" (render + log the byte count, no USB), or
    /// "real" (render + send to the USB printer).
    #[serde(default = "d_printer_mode")]
    pub mode: String,
    /// ESC/POS dot width — 576 for a standard 80mm head (512 on some clones).
    #[serde(default = "d_printer_width")]
    pub width_dots: u32,
    /// Footer QR target. Editable on-site (the booth may point at a day page).
    #[serde(default = "d_printer_qr")]
    pub qr_url: String,
    /// "Find us here" footer line. Editable on-site as the booth moves.
    #[serde(default = "d_printer_loc")]
    pub booth_location: String,
}

impl Default for PrinterConfig {
    fn default() -> Self {
        Self {
            mode: d_printer_mode(),
            width_dots: d_printer_width(),
            qr_url: d_printer_qr(),
            booth_location: d_printer_loc(),
        }
    }
}

fn d_printer_mode() -> String { "mock".into() }
fn d_printer_width() -> u32 { 576 }
fn d_printer_qr() -> String { "https://wetcourt.lol".into() }
fn d_printer_loc() -> String { "Find the Wet Court near you".into() }

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
pub struct CrossExamConfig {
    /// Startup default for the operator-toggleable cross-examination feature.
    pub enabled: bool,
    /// Seconds the defendant gets to answer the judge's follow-up question.
    pub answer_window_secs: u64,
    /// Cap on the question-generation LLM call (also the speak/await watchdog);
    /// on expiry the trial skips cross-exam and proceeds straight to verdict.
    pub question_timeout_secs: u64,
}

impl Default for CrossExamConfig {
    fn default() -> Self {
        Self { enabled: true, answer_window_secs: 10, question_timeout_secs: 12 }
    }
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
