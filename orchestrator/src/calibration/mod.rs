//! Per-device, host-side calibration for the maintenance console.
//!
//! The console works in *logical degrees*; the orchestrator transforms those to
//! *raw servo units* with this calibration before sending `AIM`, so the
//! firmware stays "dumb". Calibration is persisted per-role like personas: one
//! `<role>.toml` in a `calibration/` dir resolved next to the config file, held
//! in memory behind an `Arc<RwLock<CalibrationRegistry>>`, with REST CRUD +
//! save-to-disk mirroring `PersonaRegistry`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Calibration for one pan/tilt axis. The logical degree range
/// `[limit_min_deg, limit_max_deg]` (relative to 0° = `center`) maps linearly
/// into the raw range `[min, max]`; `invert` flips the direction; the result is
/// clamped to `[min, max]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServoCal {
    /// Raw unit lower bound (mechanical limit).
    pub min: i32,
    /// Raw unit upper bound (mechanical limit).
    pub max: i32,
    /// Raw unit at logical 0° (home / neutral).
    pub center: i32,
    /// Reverse the sense of positive degrees.
    pub invert: bool,
    /// Smallest logical angle the console may command.
    pub limit_min_deg: f32,
    /// Largest logical angle the console may command.
    pub limit_max_deg: f32,
}

impl ServoCal {
    fn validate(&self, axis: &str) -> Result<()> {
        if self.min >= self.max {
            bail!("{axis}: min ({}) must be < max ({})", self.min, self.max);
        }
        if self.center < self.min || self.center > self.max {
            bail!("{axis}: center ({}) must be within [min, max]", self.center);
        }
        if !(self.limit_min_deg < self.limit_max_deg) {
            bail!(
                "{axis}: limit_min_deg ({}) must be < limit_max_deg ({})",
                self.limit_min_deg,
                self.limit_max_deg
            );
        }
        Ok(())
    }

    /// Map a logical angle (degrees, 0 = center) to a raw servo unit.
    fn to_raw(&self, deg: f32) -> i32 {
        let d = deg.clamp(self.limit_min_deg, self.limit_max_deg);
        let span_deg = self.limit_max_deg - self.limit_min_deg;
        let units_per_deg = (self.max - self.min) as f32 / span_deg;
        let signed = if self.invert { -d } else { d };
        let raw = self.center as f32 + signed * units_per_deg;
        (raw.round() as i32).clamp(self.min, self.max)
    }
}

/// All calibration for one device role. Axes are optional (the gavel has none),
/// `fire_presets_ms` is the turret's quick-fire button durations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Calibration {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pan: Option<ServoCal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilt: Option<ServoCal>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fire_presets_ms: Vec<u32>,
}

static ROLE_RE: OnceLock<Regex> = OnceLock::new();
fn role_re() -> &'static Regex {
    ROLE_RE.get_or_init(|| Regex::new(r"^[a-z0-9_]+$").unwrap())
}

impl Calibration {
    pub fn validate(&self) -> Result<()> {
        let role = &self.role;
        if role.is_empty() || role.len() > 32 || !role_re().is_match(role) {
            bail!("invalid role '{role}': must match ^[a-z0-9_]+$ and be 1-32 chars");
        }
        if let Some(p) = &self.pan {
            p.validate("pan")?;
        }
        if let Some(t) = &self.tilt {
            t.validate("tilt")?;
        }
        if self.fire_presets_ms.iter().any(|&ms| ms == 0) {
            bail!("fire_presets_ms must all be > 0");
        }
        Ok(())
    }

    /// Transform a logical pan/tilt aim (degrees) into raw servo units. Errors
    /// if this device has no pan/tilt axes calibrated.
    pub fn aim_to_raw(&self, pan_deg: f32, tilt_deg: f32) -> Result<(i32, i32)> {
        let pan = self
            .pan
            .as_ref()
            .ok_or_else(|| anyhow!("role '{}' has no pan axis", self.role))?;
        let tilt = self
            .tilt
            .as_ref()
            .ok_or_else(|| anyhow!("role '{}' has no tilt axis", self.role))?;
        Ok((pan.to_raw(pan_deg), tilt.to_raw(tilt_deg)))
    }
}

/// In-memory registry of per-role calibration, loaded from `<dir>/<role>.toml`.
/// Mirrors `PersonaRegistry` but has no "active" concept — keyed purely by role.
pub struct CalibrationRegistry {
    dir: PathBuf,
    by_role: BTreeMap<String, Calibration>,
}

impl CalibrationRegistry {
    pub fn load_from_dir(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        if !dir.is_dir() {
            bail!("calibration dir missing: {}", dir.display());
        }
        let mut by_role = BTreeMap::new();
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let text =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            let cal: Calibration =
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
            cal.validate()
                .with_context(|| format!("validating {}", path.display()))?;
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| anyhow!("bad filename: {}", path.display()))?;
            if stem != cal.role {
                bail!(
                    "calibration role '{}' does not match filename '{}'",
                    cal.role,
                    path.display()
                );
            }
            by_role.insert(cal.role.clone(), cal);
        }
        Ok(Self { dir, by_role })
    }

    pub fn list(&self) -> Vec<&Calibration> {
        self.by_role.values().collect()
    }

    pub fn get(&self, role: &str) -> Option<&Calibration> {
        self.by_role.get(role)
    }

    pub fn update(&mut self, role: &str, cal: Calibration) -> Result<()> {
        if role != cal.role {
            bail!("role in path '{role}' does not match body role '{}'", cal.role);
        }
        if !self.by_role.contains_key(role) {
            bail!("unknown calibration role '{role}'");
        }
        cal.validate()?;
        self.by_role.insert(role.to_string(), cal);
        Ok(())
    }

    pub fn save(&self, role: &str) -> Result<()> {
        let cal = self
            .by_role
            .get(role)
            .ok_or_else(|| anyhow!("unknown calibration role '{role}'"))?;
        let text = toml::to_string_pretty(cal).context("serializing calibration")?;
        let path = self.dir.join(format!("{role}.toml"));
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pan_cal() -> ServoCal {
        ServoCal {
            min: 1000,
            max: 2000,
            center: 1500,
            invert: false,
            limit_min_deg: -90.0,
            limit_max_deg: 90.0,
        }
    }

    fn turret() -> Calibration {
        Calibration {
            role: "turret".into(),
            pan: Some(pan_cal()),
            tilt: Some(pan_cal()),
            fire_presets_ms: vec![60, 150, 280],
        }
    }

    #[test]
    fn aim_center_is_servo_center() {
        let (p, t) = turret().aim_to_raw(0.0, 0.0).unwrap();
        assert_eq!((p, t), (1500, 1500));
    }

    #[test]
    fn aim_extremes_hit_servo_bounds() {
        let cal = turret();
        let (p, _) = cal.aim_to_raw(90.0, 0.0).unwrap();
        assert_eq!(p, 2000);
        let (p, _) = cal.aim_to_raw(-90.0, 0.0).unwrap();
        assert_eq!(p, 1000);
    }

    #[test]
    fn aim_clamps_out_of_range_degrees() {
        let cal = turret();
        let (p, _) = cal.aim_to_raw(180.0, 0.0).unwrap();
        assert_eq!(p, 2000); // clamped to limit_max_deg → max
    }

    #[test]
    fn aim_invert_flips_direction() {
        let mut cal = turret();
        cal.pan.as_mut().unwrap().invert = true;
        let (p, _) = cal.aim_to_raw(90.0, 0.0).unwrap();
        assert_eq!(p, 1000); // inverted: +90° now maps to min
    }

    #[test]
    fn aim_without_axes_errors() {
        let gavel = Calibration {
            role: "gavel".into(),
            pan: None,
            tilt: None,
            fire_presets_ms: vec![],
        };
        assert!(gavel.aim_to_raw(0.0, 0.0).is_err());
    }

    #[test]
    fn validate_rejects_bad_axis() {
        let mut cal = turret();
        cal.pan.as_mut().unwrap().min = 3000; // min >= max
        assert!(cal.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_preset() {
        let mut cal = turret();
        cal.fire_presets_ms = vec![0];
        assert!(cal.validate().is_err());
    }

    #[test]
    fn registry_load_update_save() {
        let tmp = tempdir();
        let cal = turret();
        fs::write(
            tmp.join("turret.toml"),
            toml::to_string_pretty(&cal).unwrap(),
        )
        .unwrap();

        let mut reg = CalibrationRegistry::load_from_dir(&tmp).unwrap();
        assert_eq!(reg.list().len(), 1);
        assert!(reg.get("turret").is_some());

        let mut updated = turret();
        updated.fire_presets_ms = vec![99];
        reg.update("turret", updated).unwrap();
        assert_eq!(reg.get("turret").unwrap().fire_presets_ms, vec![99]);

        // path/body mismatch rejected
        assert!(reg.update("gavel", turret()).is_err());

        reg.save("turret").unwrap();
        let on_disk = fs::read_to_string(tmp.join("turret.toml")).unwrap();
        assert!(on_disk.contains("99"));
    }

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "wetcourt_calibration_{}_{}_{}",
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
