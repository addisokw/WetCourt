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

/// The gavel's strike geometry. The host sends all seven values on every `GAVEL`
/// so the firmware stays stateless: three servo positions (pulse-width µs) for
/// the rap plus the per-move dwell (ms) that lets each move physically arrive
/// before the next, and how many raps to deliver. The sequence is
/// `rest` → `raise`, then (`strike` → `raise`) × `strikes`, then → `rest`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GavelCal {
    /// Idle position (servo µs).
    pub rest: i32,
    /// Wind-up position (servo µs).
    pub raise: i32,
    /// Strike position — where the head bangs the block (servo µs).
    pub strike: i32,
    /// Dwell after each wind-up (ms).
    pub raise_dwell_ms: u32,
    /// Dwell after each strike — the bang (ms).
    pub strike_dwell_ms: u32,
    /// Dwell on the return to rest before acking (ms).
    pub settle_dwell_ms: u32,
    /// Number of raps in one strike sequence (≥ 1).
    #[serde(default = "default_strikes")]
    pub strikes: u32,
}

/// Backfills `strikes` for older `gavel.toml` files written before the field
/// existed — a single rap, the historical behaviour.
fn default_strikes() -> u32 {
    1
}

impl GavelCal {
    /// Plausible servo pulse-width window (µs). The firmware clamps to its own
    /// hard range too; this just rejects nonsense in the console.
    const US_MIN: i32 = 400;
    const US_MAX: i32 = 2600;
    /// No single dwell should exceed this (ms) — a slow servo is ~1s of travel.
    const DWELL_MAX_MS: u32 = 5000;
    /// Cap on raps per sequence — each rap blocks the firmware loop for its
    /// dwells, so keep the whole synchronous swing bounded.
    const MAX_STRIKES: u32 = 10;

    fn validate(&self) -> Result<()> {
        for (name, us) in [("rest", self.rest), ("raise", self.raise), ("strike", self.strike)] {
            if us < Self::US_MIN || us > Self::US_MAX {
                bail!(
                    "gavel.{name} ({us}) must be within [{}, {}] µs",
                    Self::US_MIN,
                    Self::US_MAX
                );
            }
        }
        for (name, ms) in [
            ("raise_dwell_ms", self.raise_dwell_ms),
            ("strike_dwell_ms", self.strike_dwell_ms),
            ("settle_dwell_ms", self.settle_dwell_ms),
        ] {
            if ms > Self::DWELL_MAX_MS {
                bail!("gavel.{name} ({ms}) must be ≤ {} ms", Self::DWELL_MAX_MS);
            }
        }
        if self.strikes < 1 || self.strikes > Self::MAX_STRIKES {
            bail!(
                "gavel.strikes ({}) must be within [1, {}]",
                self.strikes,
                Self::MAX_STRIKES
            );
        }
        Ok(())
    }
}

/// The vision targeting servo's tuning — the "firmware" here is the vision
/// process, which holds these live in memory (seeded from its CLI flags) and
/// loses them on restart. The host owns the saved copy, same as servo
/// calibration: the console tunes live, then deliberately saves; the
/// orchestrator re-seeds the vision process whenever it (re)appears.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisionCal {
    /// Degrees of pan per pixel of boresight error. Sign is hardware-dependent
    /// (negative flips the servo direction). f64 so console-typed decimals
    /// (0.05) round-trip through the toml without float noise.
    pub gain_pan: f64,
    /// Degrees of tilt per pixel of error.
    pub gain_tilt: f64,
    /// Pixel error within which the target counts as LOCKED.
    pub tolerance: u32,
    /// The camera pixel the gun actually points at ([x, y]); `None` = frame
    /// center. Calibrated by clicking the feed where the stream lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boresight: Option<[u32; 2]>,
    /// Body part the trial's Acquire cue targets ("chest" | "head" — never
    /// "none": a guilty verdict must always have something to lock onto).
    #[serde(default = "default_target_part")]
    pub target_part: String,
    /// Auto-fire dwell (ms) — how long a lock must hold before the tuning
    /// tool fires. The enabled flag is deliberately NOT persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autofire_dwell_ms: Option<u64>,
    /// Vision-failure fallback: a fixed turret aim (logical [pan°, tilt°],
    /// calibrated to just above the defendant microphone). With vision down at
    /// Acquire the gun parks here; with no fresh lock at Freeze it fires here
    /// instead of holding the shot. `None` = no fallback (fail-safe hold).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_aim: Option<[f32; 2]>,
}

fn default_target_part() -> String {
    "head".into()
}

impl VisionCal {
    /// |gain| ceiling (deg/px): at 640 px wide even 0.5 deg/px is a wild slew;
    /// 1.0 rejects nonsense while leaving generous tuning room either sign.
    const GAIN_MAX: f64 = 1.0;
    const TOLERANCE_MAX: u32 = 200;
    /// Matches `autofire::MAX_DWELL_MS`.
    const DWELL_MAX_MS: u64 = 60_000;
    /// Sanity bound for a boresight coordinate (any plausible camera mode).
    const BORESIGHT_MAX: u32 = 8192;
    /// Fallback aim window (logical degrees; per-role calibration clamps
    /// tighter at send time).
    const FALLBACK_DEG_MAX: f32 = 90.0;

    fn validate(&self) -> Result<()> {
        for (name, g) in [("gain_pan", self.gain_pan), ("gain_tilt", self.gain_tilt)] {
            if !g.is_finite() || g.abs() > Self::GAIN_MAX {
                bail!("vision.{name} ({g}) must be finite and within ±{}", Self::GAIN_MAX);
            }
        }
        if self.tolerance < 1 || self.tolerance > Self::TOLERANCE_MAX {
            bail!(
                "vision.tolerance ({}) must be within [1, {}]",
                self.tolerance,
                Self::TOLERANCE_MAX
            );
        }
        if let Some([x, y]) = self.boresight {
            if x > Self::BORESIGHT_MAX || y > Self::BORESIGHT_MAX {
                bail!("vision.boresight ({x}, {y}) out of range (max {})", Self::BORESIGHT_MAX);
            }
        }
        if !matches!(self.target_part.as_str(), "chest" | "head") {
            bail!(
                "vision.target_part ('{}') must be \"chest\" or \"head\"",
                self.target_part
            );
        }
        if let Some(ms) = self.autofire_dwell_ms {
            if ms > Self::DWELL_MAX_MS {
                bail!("vision.autofire_dwell_ms ({ms}) must be ≤ {}", Self::DWELL_MAX_MS);
            }
        }
        if let Some([p, t]) = self.fallback_aim {
            for (name, d) in [("pan", p), ("tilt", t)] {
                if !d.is_finite() || d.abs() > Self::FALLBACK_DEG_MAX {
                    bail!(
                        "vision.fallback_aim {name} ({d}) must be finite and within ±{}°",
                        Self::FALLBACK_DEG_MAX
                    );
                }
            }
        }
        Ok(())
    }
}

/// All calibration for one device role. Axes are optional (the gavel has none),
/// `fire_ms` is the squirt board's relay-open duration (ms) — the single
/// test-fire time set from the console, `gavel` is the gavel's strike geometry,
/// `vision` is the targeting servo's tuning (role "vision").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Calibration {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pan: Option<ServoCal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilt: Option<ServoCal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fire_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gavel: Option<GavelCal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<VisionCal>,
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
        if let Some(ms) = self.fire_ms {
            // The firmware hard-caps the relay pulse at 1000 ms; reject anything
            // above that so a saved value can't silently clamp.
            if ms == 0 || ms > 1000 {
                bail!("fire_ms ({ms}) must be within [1, 1000]");
            }
        }
        if let Some(g) = &self.gavel {
            g.validate()?;
        }
        if let Some(v) = &self.vision {
            v.validate()?;
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
            fire_ms: None, // turret aims; the squirt board owns fire_ms
            gavel: None,
            vision: None,
        }
    }

    fn gavel_cal() -> GavelCal {
        GavelCal {
            rest: 1500,
            raise: 2000,
            strike: 1100,
            raise_dwell_ms: 180,
            strike_dwell_ms: 120,
            settle_dwell_ms: 160,
            strikes: 3,
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
            fire_ms: None,
            gavel: Some(gavel_cal()),
            vision: None,
        };
        assert!(gavel.aim_to_raw(0.0, 0.0).is_err());
    }

    #[test]
    fn gavel_cal_roundtrips_through_toml() {
        let cal = Calibration {
            role: "gavel".into(),
            pan: None,
            tilt: None,
            fire_ms: None,
            gavel: Some(gavel_cal()),
            vision: None,
        };
        let text = toml::to_string_pretty(&cal).unwrap();
        let back: Calibration = toml::from_str(&text).unwrap();
        assert_eq!(back, cal);
        assert!(back.validate().is_ok());
    }

    #[test]
    fn gavel_cal_rejects_out_of_range_position() {
        let mut g = gavel_cal();
        g.strike = 50; // below the µs window
        assert!(g.validate().is_err());
    }

    #[test]
    fn gavel_cal_rejects_absurd_dwell() {
        let mut g = gavel_cal();
        g.raise_dwell_ms = 60_000;
        assert!(g.validate().is_err());
    }

    #[test]
    fn gavel_cal_rejects_bad_strikes() {
        let mut g = gavel_cal();
        g.strikes = 0;
        assert!(g.validate().is_err());
        g.strikes = 99;
        assert!(g.validate().is_err());
    }

    #[test]
    fn gavel_cal_backfills_missing_strikes() {
        // A pre-`strikes` gavel.toml deserialises to a single rap.
        let text = "\
role = \"gavel\"
[gavel]
rest = 1500
raise = 2000
strike = 1100
raise_dwell_ms = 180
strike_dwell_ms = 120
settle_dwell_ms = 160
";
        let cal: Calibration = toml::from_str(text).unwrap();
        assert_eq!(cal.gavel.unwrap().strikes, 1);
    }

    fn vision_cal() -> VisionCal {
        VisionCal {
            gain_pan: 0.025,
            gain_tilt: -0.03,
            tolerance: 12,
            boresight: Some([320, 260]),
            target_part: "head".into(),
            autofire_dwell_ms: Some(2000),
            fallback_aim: Some([-4.5, 12.0]),
        }
    }

    #[test]
    fn vision_cal_roundtrips_through_toml() {
        let cal = Calibration {
            role: "vision".into(),
            pan: None,
            tilt: None,
            fire_ms: None,
            gavel: None,
            vision: Some(vision_cal()),
        };
        let text = toml::to_string_pretty(&cal).unwrap();
        let back: Calibration = toml::from_str(&text).unwrap();
        assert_eq!(back, cal);
        assert!(back.validate().is_ok());
    }

    #[test]
    fn vision_cal_rejects_nonsense() {
        let mut v = vision_cal();
        v.gain_pan = f64::NAN;
        assert!(v.validate().is_err());
        v = vision_cal();
        v.gain_tilt = 5.0; // beyond the ±1 deg/px ceiling
        assert!(v.validate().is_err());
        v = vision_cal();
        v.tolerance = 0;
        assert!(v.validate().is_err());
        v = vision_cal();
        v.target_part = "none".into(); // trials must always have a real target
        assert!(v.validate().is_err());
        v = vision_cal();
        v.autofire_dwell_ms = Some(120_000);
        assert!(v.validate().is_err());
        v = vision_cal();
        v.fallback_aim = Some([f32::INFINITY, 0.0]);
        assert!(v.validate().is_err());
        v = vision_cal();
        v.fallback_aim = Some([0.0, 120.0]); // beyond ±90°
        assert!(v.validate().is_err());
        assert!(vision_cal().validate().is_ok());
    }

    #[test]
    fn vision_cal_backfills_target_part() {
        let text = "\
role = \"vision\"
[vision]
gain_pan = 0.025
gain_tilt = 0.025
tolerance = 12
";
        let cal: Calibration = toml::from_str(text).unwrap();
        let v = cal.vision.unwrap();
        assert_eq!(v.target_part, "head");
        assert!(v.boresight.is_none());
        assert!(v.autofire_dwell_ms.is_none());
    }

    #[test]
    fn shipped_vision_toml_parses() {
        // Keep the seed calibration/vision.toml loadable — the registry refuses
        // to start on an invalid file.
        let text = include_str!("../../calibration/vision.toml");
        let cal: Calibration = toml::from_str(text).unwrap();
        assert_eq!(cal.role, "vision");
        assert!(cal.validate().is_ok());
        assert!(cal.vision.is_some());
    }

    #[test]
    fn validate_rejects_bad_axis() {
        let mut cal = turret();
        cal.pan.as_mut().unwrap().min = 3000; // min >= max
        assert!(cal.validate().is_err());
    }

    #[test]
    fn validate_rejects_bad_fire_ms() {
        let mut cal = turret();
        cal.fire_ms = Some(0);
        assert!(cal.validate().is_err());
        cal.fire_ms = Some(1001); // above the firmware cap
        assert!(cal.validate().is_err());
        cal.fire_ms = Some(150);
        assert!(cal.validate().is_ok());
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
        updated.fire_ms = Some(99);
        reg.update("turret", updated).unwrap();
        assert_eq!(reg.get("turret").unwrap().fire_ms, Some(99));

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
