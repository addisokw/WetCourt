use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

/// One charge in the curated list. `enabled = false` retires a crime from the
/// draw pool without deleting it (operators can re-enable later).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Crime {
    pub id: u32,
    pub category: String,
    pub charge: String,
    #[serde(default = "d_true")]
    pub enabled: bool,
}

fn d_true() -> bool {
    true
}

impl Crime {
    pub fn validate(&self) -> Result<()> {
        let cat = self.category.trim();
        if cat.is_empty() || cat.chars().count() > 40 {
            bail!("category must be 1-40 chars");
        }
        let len = self.charge.trim().chars().count();
        if len < 10 {
            bail!("charge too short: {len} chars (min 10)");
        }
        if len > 300 {
            bail!("charge too long: {len} chars (max 300)");
        }
        Ok(())
    }
}

/// On-disk shape — matches the original brainstorm file so it can be loaded
/// unmodified. `exhibit`/`description` are carried through on save.
#[derive(Debug, Serialize, Deserialize)]
struct CrimesFile {
    #[serde(default)]
    exhibit: String,
    #[serde(default)]
    description: String,
    crimes: Vec<Crime>,
}

pub struct CrimeStore {
    path: PathBuf,
    header: (String, String), // (exhibit, description) preserved on save
    crimes: Vec<Crime>,
    /// Category filter for the draw (creator mode etc.). None = all categories.
    category_filter: Option<String>,
    /// Operator-queued charges; popped before any random draw.
    queue: VecDeque<String>,
    /// Recently drawn crime ids, newest last, to avoid repeats.
    recent: VecDeque<u32>,
    no_repeat_window: usize,
}

impl CrimeStore {
    pub fn load_from_file(path: impl AsRef<Path>, no_repeat_window: usize) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let file: CrimesFile =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let mut seen = std::collections::HashSet::new();
        for c in &file.crimes {
            c.validate()
                .with_context(|| format!("crime id {} in {}", c.id, path.display()))?;
            if !seen.insert(c.id) {
                bail!("duplicate crime id {} in {}", c.id, path.display());
            }
        }
        Ok(Self {
            path,
            header: (file.exhibit, file.description),
            crimes: file.crimes,
            category_filter: None,
            queue: VecDeque::new(),
            recent: VecDeque::new(),
            no_repeat_window,
        })
    }

    pub fn save(&self) -> Result<()> {
        let file = CrimesFile {
            exhibit: self.header.0.clone(),
            description: self.header.1.clone(),
            crimes: self.crimes.clone(),
        };
        let text = serde_json::to_string_pretty(&file).context("serializing crimes")?;
        // Write via temp file + rename so a crash mid-write can't truncate the
        // only copy of the curated list.
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming {} into place", tmp.display()))?;
        Ok(())
    }

    pub fn list(&self) -> &[Crime] {
        &self.crimes
    }

    pub fn get(&self, id: u32) -> Option<&Crime> {
        self.crimes.iter().find(|c| c.id == id)
    }

    pub fn categories(&self) -> Vec<String> {
        let mut cats: Vec<String> = self.crimes.iter().map(|c| c.category.clone()).collect();
        cats.sort();
        cats.dedup();
        cats
    }

    pub fn add(&mut self, category: String, charge: String) -> Result<&Crime> {
        let id = self.crimes.iter().map(|c| c.id).max().unwrap_or(0) + 1;
        let crime = Crime { id, category, charge, enabled: true };
        crime.validate()?;
        self.crimes.push(crime);
        self.save()?;
        Ok(self.crimes.last().unwrap())
    }

    pub fn update(&mut self, id: u32, mut crime: Crime) -> Result<&Crime> {
        crime.id = id; // path id wins; body can't move a crime onto another id
        crime.validate()?;
        let slot = self
            .crimes
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| anyhow!("unknown crime id {id}"))?;
        *slot = crime;
        self.save()?;
        Ok(self.crimes.iter().find(|c| c.id == id).unwrap())
    }

    pub fn remove(&mut self, id: u32) -> Result<()> {
        let before = self.crimes.len();
        self.crimes.retain(|c| c.id != id);
        if self.crimes.len() == before {
            bail!("unknown crime id {id}");
        }
        self.save()?;
        Ok(())
    }

    pub fn category_filter(&self) -> Option<&str> {
        self.category_filter.as_deref()
    }

    pub fn set_category_filter(&mut self, category: Option<String>) -> Result<()> {
        if let Some(cat) = &category {
            if !self.crimes.iter().any(|c| c.category == *cat) {
                bail!("no crimes in category '{cat}'");
            }
        }
        self.category_filter = category;
        Ok(())
    }

    pub fn queue(&self) -> impl Iterator<Item = &str> {
        self.queue.iter().map(|s| s.as_str())
    }

    pub fn queue_push(&mut self, charge: String) -> Result<()> {
        let len = charge.trim().chars().count();
        if !(10..=300).contains(&len) {
            bail!("queued charge must be 10-300 chars, got {len}");
        }
        self.queue.push_back(charge.trim().to_string());
        Ok(())
    }

    /// Pop the next operator-queued charge, if any. Used directly when
    /// `crimes.source = "llm"` so manual charges still take precedence over
    /// on-the-fly generation.
    pub fn queue_pop(&mut self) -> Option<String> {
        self.queue.pop_front()
    }

    pub fn queue_remove(&mut self, index: usize) -> Result<()> {
        if index >= self.queue.len() {
            bail!("queue index {index} out of range (len {})", self.queue.len());
        }
        self.queue.remove(index);
        Ok(())
    }

    /// Next charge for a trial: operator queue first, then a random enabled
    /// crime matching the category filter, avoiding the last
    /// `no_repeat_window` draws. Returns None only when the queue is empty
    /// AND no crime is eligible (caller falls back to canned charges).
    pub fn draw(&mut self) -> Option<String> {
        if let Some(queued) = self.queue_pop() {
            return Some(queued);
        }
        let eligible: Vec<&Crime> = self
            .crimes
            .iter()
            .filter(|c| c.enabled)
            .filter(|c| {
                self.category_filter
                    .as_ref()
                    .is_none_or(|cat| c.category == *cat)
            })
            .collect();
        if eligible.is_empty() {
            return None;
        }
        // Prefer crimes outside the no-repeat window; if the pool is smaller
        // than the window, fall back to the full eligible set.
        let fresh: Vec<&&Crime> = eligible
            .iter()
            .filter(|c| !self.recent.contains(&c.id))
            .collect();
        let pick = if fresh.is_empty() {
            *eligible.choose(&mut rand::thread_rng())?
        } else {
            **fresh.choose(&mut rand::thread_rng())?
        };
        self.recent.push_back(pick.id);
        while self.recent.len() > self.no_repeat_window {
            self.recent.pop_front();
        }
        Some(pick.charge.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(crimes: &[(u32, &str, &str)]) -> CrimeStore {
        let dir = tempdir();
        let path = dir.join("crimes.json");
        let file = CrimesFile {
            exhibit: "Wet Court".into(),
            description: "test".into(),
            crimes: crimes
                .iter()
                .map(|(id, cat, charge)| Crime {
                    id: *id,
                    category: (*cat).into(),
                    charge: (*charge).into(),
                    enabled: true,
                })
                .collect(),
        };
        fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();
        CrimeStore::load_from_file(&path, 3).unwrap()
    }

    const C1: &str = "The defendant stands accused of crime one.";
    const C2: &str = "The defendant stands accused of crime two.";
    const C3: &str = "The defendant stands accused of crime three.";

    #[test]
    fn load_accepts_original_brainstorm_shape() {
        // No `enabled` field on disk — must default to true.
        let dir = tempdir();
        let path = dir.join("crimes.json");
        fs::write(
            &path,
            r#"{"exhibit":"Wet Court","description":"d","crimes":[
                {"id":1,"category":"tech","charge":"The defendant stands accused of testing."}
            ]}"#,
        )
        .unwrap();
        let store = CrimeStore::load_from_file(&path, 5).unwrap();
        assert_eq!(store.list().len(), 1);
        assert!(store.list()[0].enabled);
    }

    #[test]
    fn load_rejects_duplicate_ids() {
        let dir = tempdir();
        let path = dir.join("crimes.json");
        fs::write(
            &path,
            format!(
                r#"{{"crimes":[{{"id":1,"category":"a","charge":"{C1}"}},{{"id":1,"category":"b","charge":"{C2}"}}]}}"#
            ),
        )
        .unwrap();
        assert!(CrimeStore::load_from_file(&path, 5).is_err());
    }

    #[test]
    fn draw_prefers_queue_then_filters() {
        let mut s = store_with(&[(1, "tech", C1), (2, "social", C2)]);
        s.queue_push("The defendant stands accused of a queued crime.".into())
            .unwrap();
        assert_eq!(
            s.draw().unwrap(),
            "The defendant stands accused of a queued crime."
        );
        s.set_category_filter(Some("social".into())).unwrap();
        assert_eq!(s.draw().unwrap(), C2);
    }

    #[test]
    fn draw_avoids_recent_until_pool_exhausted() {
        let mut s = store_with(&[(1, "a", C1), (2, "a", C2), (3, "a", C3)]);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..3 {
            seen.insert(s.draw().unwrap());
        }
        // window = 3 and pool = 3: first three draws must all differ
        assert_eq!(seen.len(), 3);
        // pool exhausted relative to window: still returns something
        assert!(s.draw().is_some());
    }

    #[test]
    fn draw_skips_disabled_and_empties_to_none() {
        let mut s = store_with(&[(1, "a", C1)]);
        let mut c = s.list()[0].clone();
        c.enabled = false;
        s.update(1, c).unwrap();
        assert!(s.draw().is_none());
    }

    #[test]
    fn filter_rejects_unknown_category() {
        let mut s = store_with(&[(1, "tech", C1)]);
        assert!(s.set_category_filter(Some("nope".into())).is_err());
        assert!(s.set_category_filter(Some("tech".into())).is_ok());
        assert!(s.set_category_filter(None).is_ok());
    }

    #[test]
    fn crud_assigns_ids_and_persists() {
        let mut s = store_with(&[(7, "a", C1)]);
        let id = s.add("b".into(), C2.into()).unwrap().id;
        assert_eq!(id, 8); // max + 1
        s.remove(7).unwrap();
        assert!(s.remove(7).is_err());

        // reload from disk: add/remove were persisted
        let path = s.path.clone();
        let reloaded = CrimeStore::load_from_file(&path, 3).unwrap();
        assert_eq!(reloaded.list().len(), 1);
        assert_eq!(reloaded.list()[0].id, 8);
    }

    #[test]
    fn update_keeps_path_id() {
        let mut s = store_with(&[(1, "a", C1), (2, "a", C2)]);
        let mut body = s.list()[0].clone();
        body.id = 2; // attempt to collide
        body.charge = C3.into();
        let updated = s.update(1, body).unwrap();
        assert_eq!(updated.id, 1);
        assert_eq!(s.list().iter().filter(|c| c.id == 2).count(), 1);
    }

    #[test]
    fn validate_bounds() {
        let mut c = Crime { id: 1, category: "a".into(), charge: C1.into(), enabled: true };
        assert!(c.validate().is_ok());
        c.charge = "too short".into();
        assert!(c.validate().is_err());
        c.charge = "x".repeat(301);
        assert!(c.validate().is_err());
        c.charge = C1.into();
        c.category = "".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn queue_validation_and_removal() {
        let mut s = store_with(&[(1, "a", C1)]);
        assert!(s.queue_push("short".into()).is_err());
        s.queue_push(C2.into()).unwrap();
        s.queue_push(C3.into()).unwrap();
        assert_eq!(s.queue().count(), 2);
        s.queue_remove(0).unwrap();
        assert_eq!(s.queue().next().unwrap(), C3);
        assert!(s.queue_remove(5).is_err());
    }

    // tiny tempdir helper, same idiom as personas tests
    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "wetcourt_crimes_{}_{}_{}",
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
