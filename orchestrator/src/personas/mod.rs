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
}

static ID_RE: OnceLock<Regex> = OnceLock::new();
fn id_re() -> &'static Regex {
    ID_RE.get_or_init(|| Regex::new(r"^[a-z0-9_]+$").unwrap())
}

impl Persona {
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
        Ok(())
    }
}

pub struct PersonaRegistry {
    dir: PathBuf,
    personas: BTreeMap<String, Persona>,
    active_id: String,
}

impl PersonaRegistry {
    pub fn load_from_dir(dir: impl AsRef<Path>, default_id: &str) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        if !dir.is_dir() {
            bail!("persona dir missing: {}", dir.display());
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
            personas,
            active_id: default_id.to_string(),
        })
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
        }
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
        p
    }
}
