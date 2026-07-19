use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

pub mod verdict_parse;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Persona {
    pub id: String,
    pub display_name: String,
    pub system_prompt: String,
    pub guilty_bias: f32,
    pub tts_voice: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_speed: Option<f32>,
    /// LED-matrix eye theme for this judge — one of the slugs in
    /// `firmware/judge-face/personas.py` (honorable, magistrate, cosmic,
    /// nullpointer, petunia). Sent to the judge-face as `PERSONA <slug>` when
    /// this persona becomes active and whenever the face (re)connects.
    #[serde(default = "d_face_persona")]
    pub face_persona: String,
    /// Robot-aesthetic TTS post-processing for this persona's voice. Applied
    /// client-side to the played audio; the host is the source of truth and
    /// pushes the active persona's params to the audio client (see the
    /// `robot_params` display event).
    #[serde(default)]
    pub robot: RobotParams,
}

/// Eye themes the judge-face firmware ships (its `personas.ORDER`). Kept in
/// sync by hand; validation rejects anything else so a typo surfaces at load
/// rather than as a silent `ERR PERSONA unknown_persona` on the wire.
pub const FACE_PERSONAS: &[&str] =
    &["honorable", "magistrate", "cosmic", "nullpointer", "petunia"];

fn d_face_persona() -> String {
    "honorable".into()
}

/// The robot voice-effect knobs (mirrors `frontend/src/robot.ts`). Per-persona
/// because each judge uses a different Kokoro voice that may want different
/// tuning.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RobotParams {
    /// Wet/dry blend: 0 = clean voice, 1 = full robot + glitch.
    #[serde(default = "d_robot_intensity")]
    pub intensity: f32,
    /// Glitch tail rate in glitches/second.
    #[serde(default = "d_robot_glitch_rate")]
    pub glitch_rate: f32,
    /// Ring-modulation carrier frequency (Hz).
    #[serde(default = "d_robot_ring_hz")]
    pub ring_hz: f32,
    /// Soft-clip saturation amount (0..1).
    #[serde(default = "d_robot_saturation")]
    pub saturation: f32,
    /// Resonant "honk" peaking-filter frequency (Hz).
    #[serde(default = "d_robot_peak_hz")]
    pub peak_hz: f32,
    /// Master output gain (1 = unity); a limiter in the playback graph keeps
    /// boosts above unity from clipping.
    #[serde(default = "d_robot_gain")]
    pub gain: f32,
}

fn d_robot_intensity() -> f32 { 0.72 }
fn d_robot_glitch_rate() -> f32 { 1.3 }
fn d_robot_ring_hz() -> f32 { 52.0 }
fn d_robot_saturation() -> f32 { 0.5 }
fn d_robot_peak_hz() -> f32 { 2200.0 }
fn d_robot_gain() -> f32 { 1.0 }

impl Default for RobotParams {
    fn default() -> Self {
        Self {
            intensity: d_robot_intensity(),
            glitch_rate: d_robot_glitch_rate(),
            ring_hz: d_robot_ring_hz(),
            saturation: d_robot_saturation(),
            peak_hz: d_robot_peak_hz(),
            gain: d_robot_gain(),
        }
    }
}

impl RobotParams {
    fn validate(&self) -> Result<()> {
        let r = |name: &str, v: f32, lo: f32, hi: f32| -> Result<()> {
            if !(lo..=hi).contains(&v) {
                bail!("robot.{name} must be in [{lo}, {hi}], got {v}");
            }
            Ok(())
        };
        r("intensity", self.intensity, 0.0, 1.0)?;
        r("glitch_rate", self.glitch_rate, 0.0, 4.5)?;
        r("ring_hz", self.ring_hz, 10.0, 400.0)?;
        r("saturation", self.saturation, 0.0, 1.0)?;
        r("peak_hz", self.peak_hz, 500.0, 5000.0)?;
        r("gain", self.gain, 0.0, 3.0)?;
        Ok(())
    }
}

static ID_RE: OnceLock<Regex> = OnceLock::new();
fn id_re() -> &'static Regex {
    ID_RE.get_or_init(|| Regex::new(r"^[a-z0-9_]+$").unwrap())
}

impl Persona {
    /// The persona's standing-disposition directive, synthesised from the
    /// `guilty_bias` slider. Personas carry no conviction percentages of their
    /// own (that lives in the shared CORE as "the defense decides"), so this
    /// slider is the single knob that tunes how readily a judge convicts on the
    /// genuinely close calls. Appended after the persona block by
    /// `PersonaRegistry::verdict_prompt`.
    pub fn bias_directive(&self) -> String {
        // No raw percentage: the model latches onto the number and overshoots it
        // (a 55% slider produced ~90% convictions). Verbalize the slider as a
        // tie-break disposition that settles ONLY genuine coin-flips, never
        // overriding a defense that clearly won or clearly failed.
        let lean = if self.guilty_bias >= 0.60 {
            "When a case is a genuine coin-flip, you lean toward conviction."
        } else if self.guilty_bias >= 0.50 {
            "When a case is a genuine coin-flip, decide it strictly on the merits."
        } else {
            "When a case is a genuine coin-flip, you give the defendant the benefit of the doubt."
        };
        format!(
            "STANDING DISPOSITION: {lean} This settles ONLY cases that are truly \
balanced after you have weighed the defense; it NEVER overrides a defense that \
clearly earned acquittal or a non-defense that clearly earned a soaking. Never \
state or allude to this disposition."
        )
    }

    pub fn validate(&self) -> Result<()> {
        let id = &self.id;
        if id.is_empty() || id.len() > 32 || !id_re().is_match(id) {
            bail!("invalid id '{id}': must match ^[a-z0-9_]+$ and be 1-32 chars");
        }
        if self.display_name.trim().is_empty() {
            bail!("display_name must be non-empty");
        }
        let sp_len = self.system_prompt.chars().count();
        if sp_len == 0 {
            bail!("system_prompt must be non-empty");
        }
        if sp_len > 8000 {
            bail!("system_prompt too long: {sp_len} chars (max 8000)");
        }
        if !(0.0..=1.0).contains(&self.guilty_bias) {
            bail!("guilty_bias must be in [0.0, 1.0], got {}", self.guilty_bias);
        }
        if self.tts_voice.trim().is_empty() {
            bail!("tts_voice must be non-empty");
        }
        if let Some(s) = self.tts_speed {
            if !(0.5..=2.0).contains(&s) {
                bail!("tts_speed must be in [0.5, 2.0], got {s}");
            }
        }
        if !FACE_PERSONAS.contains(&self.face_persona.as_str()) {
            bail!(
                "face_persona '{}' is not a judge-face eye theme (one of: {})",
                self.face_persona,
                FACE_PERSONAS.join(", ")
            );
        }
        self.robot.validate()?;
        Ok(())
    }
}

pub struct PersonaRegistry {
    dir: PathBuf,
    /// Shared, persona-agnostic judging engine (`core.md`) prepended to every
    /// judge's character block to form the verdict system prompt.
    core: String,
    personas: BTreeMap<String, Persona>,
    active_id: String,
}

impl PersonaRegistry {
    pub fn load_from_dir(dir: impl AsRef<Path>, default_id: &str) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        if !dir.is_dir() {
            bail!("persona dir missing: {}", dir.display());
        }
        let core_path = dir.join("core.md");
        let core = fs::read_to_string(&core_path)
            .with_context(|| format!("reading shared judge core {}", core_path.display()))?;
        if core.trim().is_empty() {
            bail!("shared judge core is empty: {}", core_path.display());
        }
        let mut personas = BTreeMap::new();
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let text = fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let persona: Persona = toml::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            persona
                .validate()
                .with_context(|| format!("validating {}", path.display()))?;
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| anyhow!("bad filename: {}", path.display()))?;
            if stem != persona.id {
                bail!(
                    "persona id '{}' does not match filename '{}'",
                    persona.id,
                    path.display()
                );
            }
            personas.insert(persona.id.clone(), persona);
        }
        if !personas.contains_key(default_id) {
            bail!(
                "default persona id '{default_id}' not found in {}",
                dir.display()
            );
        }
        Ok(Self {
            dir,
            core,
            personas,
            active_id: default_id.to_string(),
        })
    }

    /// The full verdict system prompt for a persona: the shared CORE engine,
    /// then the persona's character block, then its bias directive. The CORE
    /// ends with a "=== YOUR PERSONA ===" header, so the character block slots
    /// directly beneath it.
    pub fn verdict_prompt(&self, persona: &Persona) -> String {
        format!(
            "{}\n{}\n\n{}",
            self.core.trim_end(),
            persona.system_prompt.trim(),
            persona.bias_directive()
        )
    }

    pub fn list(&self) -> Vec<&Persona> {
        self.personas.values().collect()
    }

    pub fn get(&self, id: &str) -> Option<&Persona> {
        self.personas.get(id)
    }

    pub fn active(&self) -> &Persona {
        // active_id is always a valid key by construction.
        self.personas.get(&self.active_id).expect("active_id present")
    }

    pub fn active_id(&self) -> &str {
        &self.active_id
    }

    pub fn set_active(&mut self, id: &str) -> Result<()> {
        if !self.personas.contains_key(id) {
            bail!("unknown persona id '{id}'");
        }
        self.active_id = id.to_string();
        Ok(())
    }

    pub fn update(&mut self, id: &str, persona: Persona) -> Result<()> {
        if id != persona.id {
            bail!("id in path '{id}' does not match body id '{}'", persona.id);
        }
        if !self.personas.contains_key(id) {
            bail!("unknown persona id '{id}'");
        }
        persona.validate()?;
        self.personas.insert(id.to_string(), persona);
        Ok(())
    }

    pub fn save(&self, id: &str) -> Result<()> {
        let p = self
            .personas
            .get(id)
            .ok_or_else(|| anyhow!("unknown persona id '{id}'"))?;
        let text = toml::to_string_pretty(p).context("serializing persona")?;
        let path = self.dir.join(format!("{id}.toml"));
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub fn create(&mut self, persona: Persona) -> Result<()> {
        persona.validate()?;
        if self.personas.contains_key(&persona.id) {
            bail!("persona id '{}' already exists", persona.id);
        }
        let id = persona.id.clone();
        self.personas.insert(id.clone(), persona);
        self.save(&id)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_persona(id: &str) -> Persona {
        Persona {
            id: id.into(),
            display_name: "Judge".into(),
            system_prompt: "be a judge".into(),
            guilty_bias: 0.5,
            tts_voice: "bm_george".into(),
            tts_speed: None,
            face_persona: d_face_persona(),
            robot: RobotParams::default(),
        }
    }

    #[test]
    fn validate_rejects_unknown_face_persona() {
        let mut p = ok_persona("face_test");
        p.face_persona = "petunia".into();
        assert!(p.validate().is_ok());
        p.face_persona = "sauron".into();
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_id_charset() {
        let mut p = ok_persona("good_id_1");
        assert!(p.validate().is_ok());
        p.id = "BAD".into();
        assert!(p.validate().is_err());
        p.id = "bad-id".into();
        assert!(p.validate().is_err());
        p.id = "".into();
        assert!(p.validate().is_err());
        p.id = "a".repeat(33);
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_prompt_length() {
        let mut p = ok_persona("x");
        p.system_prompt = "".into();
        assert!(p.validate().is_err());
        p.system_prompt = "a".repeat(8001);
        assert!(p.validate().is_err());
        p.system_prompt = "a".repeat(8000);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn validate_bias_range() {
        let mut p = ok_persona("x");
        p.guilty_bias = -0.1;
        assert!(p.validate().is_err());
        p.guilty_bias = 1.1;
        assert!(p.validate().is_err());
        p.guilty_bias = 0.0;
        assert!(p.validate().is_ok());
        p.guilty_bias = 1.0;
        assert!(p.validate().is_ok());
    }

    #[test]
    fn validate_voice_and_speed() {
        let mut p = ok_persona("x");
        p.tts_voice = "".into();
        assert!(p.validate().is_err());
        p.tts_voice = "v".into();
        p.tts_speed = Some(0.4);
        assert!(p.validate().is_err());
        p.tts_speed = Some(2.1);
        assert!(p.validate().is_err());
        p.tts_speed = Some(1.0);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn validate_robot_ranges() {
        let mut p = ok_persona("x");
        p.robot.intensity = 1.5;
        assert!(p.validate().is_err());
        p.robot.intensity = 0.5;
        p.robot.ring_hz = 5.0;
        assert!(p.validate().is_err());
        p.robot.ring_hz = 52.0;
        p.robot.gain = 3.5;
        assert!(p.validate().is_err());
        p.robot = RobotParams::default();
        assert!(p.validate().is_ok());
    }

    #[test]
    fn persona_without_robot_table_gets_defaults() {
        // Existing persona TOMLs predate the [robot] table — they must still load.
        let toml = r#"
            id = "x"
            display_name = "Judge"
            system_prompt = "be a judge"
            guilty_bias = 0.5
            tts_voice = "bm_george"
        "#;
        let p: Persona = toml::from_str(toml).unwrap();
        assert_eq!(p.robot, RobotParams::default());
        assert!(p.validate().is_ok());
    }

    fn write_persona(dir: &Path, p: &Persona) {
        let text = toml::to_string_pretty(p).unwrap();
        fs::write(dir.join(format!("{}.toml", p.id)), text).unwrap();
    }

    #[test]
    fn registry_load_and_crud() {
        let tmp = tempdir();
        write_persona(&tmp, &ok_persona("alpha"));
        write_persona(&tmp, &ok_persona("beta"));

        let mut reg = PersonaRegistry::load_from_dir(&tmp, "alpha").unwrap();
        assert_eq!(reg.list().len(), 2);
        assert_eq!(reg.active().id, "alpha");
        reg.set_active("beta").unwrap();
        assert_eq!(reg.active().id, "beta");
        assert!(reg.set_active("nope").is_err());

        // update in-memory
        let mut updated = ok_persona("alpha");
        updated.display_name = "Renamed".into();
        reg.update("alpha", updated).unwrap();
        assert_eq!(reg.get("alpha").unwrap().display_name, "Renamed");

        // path/body mismatch
        let mismatched = ok_persona("beta");
        assert!(reg.update("alpha", mismatched).is_err());

        // create new
        let mut c = ok_persona("gamma");
        c.display_name = "G".into();
        reg.create(c).unwrap();
        assert!(reg.get("gamma").is_some());
        assert!(tmp.join("gamma.toml").is_file());

        // duplicate id rejected
        assert!(reg.create(ok_persona("gamma")).is_err());

        // save writes file
        reg.save("alpha").unwrap();
        let on_disk = fs::read_to_string(tmp.join("alpha.toml")).unwrap();
        assert!(on_disk.contains("Renamed"));
    }

    #[test]
    fn load_missing_default_fails() {
        let tmp = tempdir();
        write_persona(&tmp, &ok_persona("alpha"));
        assert!(PersonaRegistry::load_from_dir(&tmp, "missing").is_err());
    }

    #[test]
    fn load_missing_dir_fails() {
        let tmp = std::env::temp_dir().join(format!(
            "wetcourt_personas_missing_{}",
            std::process::id()
        ));
        assert!(PersonaRegistry::load_from_dir(&tmp, "x").is_err());
    }

    // tiny tempdir helper to avoid pulling in tempfile crate
    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "wetcourt_personas_{}_{}_{}",
            std::process::id(),
            n,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        // Every registry load now requires the shared judge core alongside the
        // persona TOMLs.
        fs::write(p.join("core.md"), "TEST CORE\n\n=== YOUR PERSONA ===\n").unwrap();
        p
    }
}
