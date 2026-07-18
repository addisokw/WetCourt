use anyhow::{Context, Result};
use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize, Clone, Default)]
pub struct Config {
    #[serde(default)]
    pub sip: SipConfig,
    #[serde(default)]
    pub rtp: RtpConfig,
    #[serde(default)]
    pub audio: AudioConfig,
    #[serde(default)]
    pub inference: InferenceConfig,
    #[serde(default)]
    pub persona: PersonaConfig,
    #[serde(default)]
    pub trial_context: TrialContextConfig,
    #[serde(default)]
    pub control: ControlConfig,
    #[serde(default)]
    pub recording: RecordingConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RecordingConfig {
    /// Record every call: stereo WAV (caller left, lawyer right) + a JSON
    /// event sidecar, written on hangup.
    #[serde(default = "d_recording_enabled")]
    pub enabled: bool,
    /// Output dir, relative to the config file's directory.
    #[serde(default = "d_recording_dir")]
    pub dir: String,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self { enabled: d_recording_enabled(), dir: d_recording_dir() }
    }
}

fn d_recording_enabled() -> bool {
    true
}
fn d_recording_dir() -> String {
    "recordings".into()
}

#[derive(Debug, Deserialize, Clone)]
pub struct SipConfig {
    /// UDP bind for SIP signaling.
    #[serde(default = "d_sip_bind")]
    pub bind: String,
    /// IP to advertise in Contact/SDP. Empty = auto-detect the outbound LAN
    /// address (UDP-connect trick). Set explicitly on multi-homed hosts.
    #[serde(default)]
    pub advertise_ip: String,
    /// SIP username the ATA registers as (its "SIP User ID"). Registrations
    /// for other users are accepted too, but ring-out dials this one.
    #[serde(default = "d_sip_user")]
    pub ata_user: String,
    /// The number/user the lawyer answers on — what the ATA's Offhook
    /// Auto-Dial sends. Any INVITE is answered in v1; this is documentation.
    #[serde(default = "d_lawyer_user")]
    pub lawyer_user: String,
}

impl Default for SipConfig {
    fn default() -> Self {
        Self {
            bind: d_sip_bind(),
            advertise_ip: String::new(),
            ata_user: d_sip_user(),
            lawyer_user: d_lawyer_user(),
        }
    }
}

fn d_sip_bind() -> String {
    "0.0.0.0:5060".into()
}
fn d_sip_user() -> String {
    "defendant".into()
}
fn d_lawyer_user() -> String {
    "1".into()
}

#[derive(Debug, Deserialize, Clone)]
pub struct RtpConfig {
    /// First UDP port tried for RTP media sessions.
    #[serde(default = "d_rtp_port_min")]
    pub port_min: u16,
    /// Last UDP port tried (inclusive).
    #[serde(default = "d_rtp_port_max")]
    pub port_max: u16,
}

impl Default for RtpConfig {
    fn default() -> Self {
        Self { port_min: d_rtp_port_min(), port_max: d_rtp_port_max() }
    }
}

fn d_rtp_port_min() -> u16 {
    40000
}
fn d_rtp_port_max() -> u16 {
    40099
}

#[derive(Debug, Deserialize, Clone)]
pub struct AudioConfig {
    /// RMS threshold (i16 scale) above which a 20 ms frame counts as speech.
    /// Tune on the real handset; `debug_rms = true` logs per-frame values.
    #[serde(default = "d_vad_rms_threshold")]
    pub vad_rms_threshold: f32,
    /// Consecutive speech frames to open an utterance.
    #[serde(default = "d_vad_start_frames")]
    pub vad_start_frames: u32,
    /// Trailing silence that closes an utterance (ms).
    #[serde(default = "d_vad_hangover_ms")]
    pub vad_hangover_ms: u64,
    /// Audio kept from before the trigger frame (ms).
    #[serde(default = "d_vad_preroll_ms")]
    pub vad_preroll_ms: u64,
    /// Utterances shorter than this are discarded as noise (ms).
    #[serde(default = "d_min_utterance_ms")]
    pub min_utterance_ms: u64,
    /// Hard cap; the utterance is force-closed at this length (ms).
    #[serde(default = "d_max_utterance_ms")]
    pub max_utterance_ms: u64,
    /// Listening silence before the lawyer re-prompts (secs).
    #[serde(default = "d_silence_reprompt_secs")]
    pub silence_reprompt_secs: u64,
    /// Whole-call cap; the lawyer signs off and hangs up (secs).
    #[serde(default = "d_max_call_secs")]
    pub max_call_secs: u64,
    /// Exchange cap: after this many client-utterance → lawyer-reply turns,
    /// something urgent befalls the lawyer (a persona `hangup_lines` bit) and
    /// he hangs up. Keeps calls snappy independent of the wall clock.
    #[serde(default = "d_max_exchanges")]
    pub max_exchanges: usize,
    /// Log per-frame RMS while listening (VAD calibration on real hardware).
    #[serde(default)]
    pub debug_rms: bool,
    /// Answer calls with a raw media echo instead of the lawyer — the M1
    /// media-path diagnostic, kept for bring-up.
    #[serde(default)]
    pub echo_test: bool,
    /// F5: also POST the lawyer's spoken audio (phone-band 8 kHz) to the
    /// orchestrator so it can play over the booth's primary speaker. Requires the
    /// orchestrator's `[lawyer] speaker_playback` too. Ships OFF.
    #[serde(default)]
    pub speaker_playback: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            vad_rms_threshold: d_vad_rms_threshold(),
            vad_start_frames: d_vad_start_frames(),
            vad_hangover_ms: d_vad_hangover_ms(),
            vad_preroll_ms: d_vad_preroll_ms(),
            min_utterance_ms: d_min_utterance_ms(),
            max_utterance_ms: d_max_utterance_ms(),
            silence_reprompt_secs: d_silence_reprompt_secs(),
            max_call_secs: d_max_call_secs(),
            max_exchanges: d_max_exchanges(),
            debug_rms: false,
            echo_test: false,
            speaker_playback: false,
        }
    }
}

fn d_vad_rms_threshold() -> f32 {
    700.0
}
fn d_vad_start_frames() -> u32 {
    3
}
fn d_vad_hangover_ms() -> u64 {
    700
}
fn d_vad_preroll_ms() -> u64 {
    300
}
fn d_min_utterance_ms() -> u64 {
    300
}
fn d_max_utterance_ms() -> u64 {
    15000
}
fn d_silence_reprompt_secs() -> u64 {
    12
}
fn d_max_call_secs() -> u64 {
    300
}
fn d_max_exchanges() -> usize {
    5
}

#[derive(Debug, Deserialize, Clone)]
pub struct InferenceConfig {
    /// "real" hits LiteLLM; "mock" plays canned turns for offline dev.
    #[serde(default = "d_inference_mode")]
    pub mode: String,
    #[serde(default = "d_base_url")]
    pub base_url: String,
    #[serde(default = "d_chat_model")]
    pub chat_model: String,
    #[serde(default = "d_stt_model")]
    pub stt_model: String,
    #[serde(default = "d_tts_model")]
    pub tts_model: String,
    #[serde(default = "d_stt_timeout_secs")]
    pub stt_timeout_secs: u64,
    #[serde(default = "d_chat_timeout_secs")]
    pub chat_timeout_secs: u64,
    #[serde(default = "d_tts_timeout_secs")]
    pub tts_timeout_secs: u64,
    #[serde(default)]
    pub enable_thinking: bool,
    /// Bearer for LiteLLM; aliased from LITELLM_MASTER_KEY by main.
    #[serde(default)]
    pub api_key: Option<String>,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            mode: d_inference_mode(),
            base_url: d_base_url(),
            chat_model: d_chat_model(),
            stt_model: d_stt_model(),
            tts_model: d_tts_model(),
            stt_timeout_secs: d_stt_timeout_secs(),
            chat_timeout_secs: d_chat_timeout_secs(),
            tts_timeout_secs: d_tts_timeout_secs(),
            enable_thinking: false,
            api_key: None,
        }
    }
}

fn d_inference_mode() -> String {
    "real".into()
}
fn d_base_url() -> String {
    "http://localhost:4000".into()
}
fn d_chat_model() -> String {
    "qwen3.6-35b-a3b".into()
}
fn d_stt_model() -> String {
    "whisper-1".into()
}
fn d_tts_model() -> String {
    "kokoro-tts".into()
}
fn d_stt_timeout_secs() -> u64 {
    15
}
fn d_chat_timeout_secs() -> u64 {
    20
}
fn d_tts_timeout_secs() -> u64 {
    15
}

#[derive(Debug, Deserialize, Clone)]
pub struct PersonaConfig {
    /// Lawyer persona TOML, relative to the config file's directory.
    #[serde(default = "d_persona_file")]
    pub file: String,
    /// Latency-cover / IVR asset dir, relative to the config file's directory.
    #[serde(default = "d_assets_dir")]
    pub assets_dir: String,
}

impl Default for PersonaConfig {
    fn default() -> Self {
        Self { file: d_persona_file(), assets_dir: d_assets_dir() }
    }
}

fn d_persona_file() -> String {
    "personas/lawyer.toml".into()
}
fn d_assets_dir() -> String {
    "assets".into()
}

#[derive(Debug, Deserialize, Clone)]
pub struct TrialContextConfig {
    /// Pull the live charge from the orchestrator at call start.
    #[serde(default = "d_trial_ctx_enabled")]
    pub enabled: bool,
    #[serde(default = "d_orchestrator_base_url")]
    pub orchestrator_base_url: String,
    #[serde(default = "d_trial_ctx_timeout_ms")]
    pub timeout_ms: u64,
}

impl Default for TrialContextConfig {
    fn default() -> Self {
        Self {
            enabled: d_trial_ctx_enabled(),
            orchestrator_base_url: d_orchestrator_base_url(),
            timeout_ms: d_trial_ctx_timeout_ms(),
        }
    }
}

fn d_trial_ctx_enabled() -> bool {
    true
}
fn d_orchestrator_base_url() -> String {
    "http://localhost:8080".into()
}
fn d_trial_ctx_timeout_ms() -> u64 {
    2000
}

#[derive(Debug, Deserialize, Clone)]
pub struct ControlConfig {
    /// HTTP control plane (health/status/call).
    #[serde(default = "d_control_bind")]
    pub bind: String,
}

impl Default for ControlConfig {
    fn default() -> Self {
        Self { bind: d_control_bind() }
    }
}

fn d_control_bind() -> String {
    "0.0.0.0:8092".into()
}

#[derive(Debug, Deserialize, Clone)]
pub struct LoggingConfig {
    #[serde(default = "d_log_level")]
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self { level: d_log_level() }
    }
}

fn d_log_level() -> String {
    "counsel=debug,info".into()
}

pub fn load(path: &Path) -> Result<Config> {
    Figment::new()
        .merge(Toml::file(path))
        .merge(Env::prefixed("COUNSEL__").split("__"))
        .extract()
        .with_context(|| format!("loading config from {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_load_from_empty() {
        let cfg: Config = Figment::new()
            .merge(Toml::string(""))
            .extract()
            .expect("empty config loads via defaults");
        assert_eq!(cfg.sip.bind, "0.0.0.0:5060");
        assert_eq!(cfg.control.bind, "0.0.0.0:8092");
        assert_eq!(cfg.inference.mode, "real");
        assert_eq!(cfg.rtp.port_min, 40000);
        assert!(cfg.trial_context.enabled);
    }

    #[test]
    fn partial_section_keeps_other_defaults() {
        let cfg: Config = Figment::new()
            .merge(Toml::string(
                r#"
                [sip]
                advertise_ip = "192.168.50.10"
                [audio]
                vad_rms_threshold = 900.0
                "#,
            ))
            .extract()
            .expect("partial config loads");
        assert_eq!(cfg.sip.advertise_ip, "192.168.50.10");
        assert_eq!(cfg.sip.bind, "0.0.0.0:5060");
        assert_eq!(cfg.audio.vad_rms_threshold, 900.0);
        assert_eq!(cfg.audio.vad_hangover_ms, 700);
    }
}
