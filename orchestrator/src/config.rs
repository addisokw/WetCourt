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
    /// Startup default for the trial integration (operator-toggleable live via
    /// `/operator/lawyer_integration`): off-hook pauses the plea/answer clock,
    /// and the phone rings when a cross-examination answer window opens. Off =
    /// the phone is a standalone prop (the force-ring button still works).
    #[serde(default = "d_lawyer_trial_integration")]
    pub trial_integration: bool,
}

fn d_lawyer_trial_integration() -> bool {
    true
}

impl Default for LawyerConfig {
    fn default() -> Self {
        Self { base_url: d_lawyer_base_url(), trial_integration: d_lawyer_trial_integration() }
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
    /// "off" (no receipt), "mock" (render + log the byte count, no I/O), or
    /// "real" (render + send to the printer).
    #[serde(default = "d_printer_mode")]
    pub mode: String,
    /// How to reach the printer in `real` mode: "usb" (direct libusb) or
    /// "net" (raw TCP / JetDirect to `net_addr`).
    #[serde(default = "d_printer_transport")]
    pub transport: String,
    /// LAN printer `host[:port]` for `transport = "net"`; the port defaults
    /// to 9100.
    #[serde(default)]
    pub net_addr: String,
    /// ESC/POS dot width — 576 for a standard 80mm head (512 on some clones).
    #[serde(default = "d_printer_width")]
    pub width_dots: u32,
    /// Footer QR target. Editable on-site (the booth may point at a day page).
    #[serde(default = "d_printer_qr")]
    pub qr_url: String,
    /// "Find us here" footer line. Editable on-site as the booth moves.
    #[serde(default = "d_printer_loc")]
    pub booth_location: String,
    /// Distance from the print head to the cutter blade, in 203-dpi dots. Sets
    /// the unprintable dead zone at the top of size-bounded custom prints and
    /// the minimum trailing feed before their closing cut. Calibrate by
    /// printing a fixed-length strip and measuring top-edge-to-first-line.
    #[serde(default = "d_head_to_cutter")]
    pub head_to_cutter_dots: u32,
    /// The printer's vertical motion unit (what ESC J feeds and ESC 3 line
    /// spacing count in), as units per inch. The booth POS-80 uses the Epson
    /// default 1/360" — a strip commanded as 400 "dots" measured 28.2mm, i.e.
    /// 400/360". A printer whose feeds are true 203-dpi dots would be 203.
    #[serde(default = "d_feed_units")]
    pub feed_units_per_inch: u32,
    /// Paper the cutter mechanism advances on its own before a partial cut, in
    /// 203-dpi dots — subtracted from the closing fill of size-bounded prints
    /// so cut-to-cut lands on target. Calibrate by printing bounded strips at
    /// two lengths: the constant excess shared by both is this advance.
    #[serde(default)]
    pub cut_advance_dots: u32,
    /// Default gamma applied to every rasterized image (custom-print blocks
    /// and the keepsake capture photo) before dithering. `<1.0` brightens
    /// mid-tones; thermal heads run dark, so ~0.7–0.8 is typical. Per-printer:
    /// calibrate with a tone-ladder strip.
    #[serde(default = "d_image_gamma")]
    pub image_gamma: f32,
    /// Default brightness offset in luma units (−128..128, `+` = lighter),
    /// applied with `image_gamma`. Per-printer.
    #[serde(default)]
    pub image_brightness: f32,
    /// Default contrast multiplier around mid-gray (`1.0` = none, `<1`
    /// flattens — lifts shadows and tames highlights, useful against thermal
    /// dot gain; `>1` punchier). Per-printer.
    #[serde(default = "d_image_contrast")]
    pub image_contrast: f32,
    /// Default dither for rasterized images: "fs" | "atkinson" | "bayer" |
    /// "none". Atkinson prints sparser/lighter — often better against thermal
    /// dot gain. Per-printer.
    #[serde(default = "d_image_dither")]
    pub image_dither: String,
    /// Set when the printer is physically mounted upside down: every print is
    /// rotated 180° (ESC { per-line rotation + reversed content order +
    /// software-rotated rasters) so the emerging receipt reads correctly.
    #[serde(default)]
    pub upside_down: bool,
    /// How many copies of each trial keepsake to print — the booth runs 2 (one
    /// to hang on the backdrop, one for the defendant). Each copy is a separate
    /// cut strip. Clamped to at least 1 at print time; only affects the trial
    /// keepsake, not operator custom prints (those print exactly what's asked).
    #[serde(default = "d_keepsake_copies")]
    pub keepsake_copies: u32,
    /// How often to append a "bad lawyer" coupon to the keepsake:
    /// "off" | "rare" (~1/6) | "sometimes" (~1/3) | "always". Runtime-switchable
    /// (edit + `--restart`); unknown values are treated as "off".
    #[serde(default = "d_coupon_frequency")]
    pub coupon_frequency: String,
}

impl Default for PrinterConfig {
    fn default() -> Self {
        Self {
            mode: d_printer_mode(),
            transport: d_printer_transport(),
            net_addr: String::new(),
            width_dots: d_printer_width(),
            qr_url: d_printer_qr(),
            booth_location: d_printer_loc(),
            head_to_cutter_dots: d_head_to_cutter(),
            feed_units_per_inch: d_feed_units(),
            cut_advance_dots: 0,
            image_gamma: d_image_gamma(),
            image_brightness: 0.0,
            image_contrast: d_image_contrast(),
            image_dither: d_image_dither(),
            upside_down: false,
            keepsake_copies: d_keepsake_copies(),
            coupon_frequency: d_coupon_frequency(),
        }
    }
}

fn d_coupon_frequency() -> String { "off".into() }
fn d_printer_mode() -> String { "mock".into() }
fn d_printer_transport() -> String { "usb".into() }
fn d_printer_width() -> u32 { 576 }
// Measured on the booth POS-80: 17.0mm from the cut edge to the first printed
// line (~136 dots @ 203dpi). The crate's CUTTER_CLEARANCE_DOTS (110) was the
// pre-calibration guess.
fn d_head_to_cutter() -> u32 { 136 }
// Measured: vertical commands move in 1/360" Epson-default units, not dots.
fn d_feed_units() -> u32 { 360 }
fn d_image_gamma() -> f32 { 1.0 }
fn d_image_contrast() -> f32 { 1.0 }
fn d_image_dither() -> String { "fs".into() }
fn d_keepsake_copies() -> u32 { 1 }
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
    pub charge_timeout_secs: u64,
    pub verdict_first_token_timeout_secs: u64,
    pub verdict_total_timeout_secs: u64,
    pub stt_timeout_secs: u64,
    pub tts_timeout_secs: u64,
    pub enable_thinking: bool,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Sampling temperature for the VERDICT deliberation only (charge/cross keep
    /// the client's 0.9). Verdicts ran at 0.9, which made the same good defense
    /// win or lose by luck run-to-run; a lower value makes "the defense decides"
    /// reliable. serde default keeps older config files parsing.
    #[serde(default = "d_verdict_temperature")]
    pub verdict_temperature: f64,
}

fn d_verdict_temperature() -> f64 {
    0.5
}

#[derive(Debug, Deserialize, Clone)]
pub struct HardwareConfig {
    pub driver: String,         // "mock" | "tcp" | "serial"
    pub ack_timeout_ms: u64,
    /// Where to listen for the MCU's TCP connection when driver = "tcp".
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,
    /// UDP port for the discovery beacon (`WETCOURT <spec> <tcp_port>`,
    /// broadcast every ~2 s so firmware with no `ORCH_HOST` configured can
    /// find this host). Only the TCP driver beacons. 0 disables.
    #[serde(default = "default_beacon_port")]
    pub beacon_port: u16,
}

fn default_bind_addr() -> String {
    "0.0.0.0:8090".into()
}

fn default_beacon_port() -> u16 {
    8091
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
    /// History anchoring: inject this many of the most recent verdicts (as
    /// verdict + key_factor only — never any defendant text) into each new
    /// deliberation as a "tonight's bar" calibration reference. 0 = off. The
    /// block is explicitly verdict-neutral; it steadies the bar, never a quota.
    #[serde(default)]
    pub history_anchor_count: usize,
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
