//! The casebook: an append-only JSONL record of every completed trial — one
//! line per verdict, each line a serialized [`TrialRecord`]. This is the
//! `[logging] transcripts_jsonl` file finally made real.
//!
//! It doubles as the source of truth for the case counter: the next case number
//! is recovered by scanning the highest `case_no` already on disk, so it
//! survives restarts with no separate counter file to drift out of sync.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::record::TrialRecord;

/// A handle to the on-disk casebook at `path`. Cheap to clone-by-reopen; the
/// file is opened per write (append) so concurrent readers and external `tail`
/// see a consistent stream.
pub struct Casebook {
    path: PathBuf,
}

impl Casebook {
    pub fn open(path: impl AsRef<Path>) -> Self {
        Self { path: path.as_ref().to_path_buf() }
    }

    /// Append one completed trial as a single JSON line. The file (and any
    /// missing parent dirs) is created on first write.
    pub fn record(&self, rec: &TrialRecord) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating casebook dir {}", parent.display()))?;
            }
        }
        let line = serde_json::to_string(rec).context("serializing trial record")?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening casebook {}", self.path.display()))?;
        writeln!(f, "{line}")
            .with_context(|| format!("appending to {}", self.path.display()))?;
        Ok(())
    }

    /// The next case number = 1 + the highest `case_no` already on disk. A
    /// missing or empty log starts at 1.
    pub fn next_case_no(&self) -> u64 {
        self.highest_case_no().map_or(1, |h| h + 1)
    }

    /// Scan every parseable line for the maximum `case_no`. Considering all
    /// lines (not just the last) makes recovery robust to a torn final line —
    /// e.g. a crash mid-write — without needing the whole `TrialRecord` to
    /// deserialize, since only the `case_no` field is consulted.
    fn highest_case_no(&self) -> Option<u64> {
        let f = std::fs::File::open(&self.path).ok()?;
        let mut max: Option<u64> = None;
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(n) = v.get("case_no").and_then(|x| x.as_u64()) {
                    max = Some(max.map_or(n, |m| m.max(n)));
                }
            }
        }
        max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "wetcourt_casebook_{}_{}.jsonl",
            std::process::id(),
            tag
        ))
    }

    #[test]
    fn counter_starts_at_one_when_missing() {
        let p = temp_path("missing");
        let _ = std::fs::remove_file(&p);
        assert_eq!(Casebook::open(&p).next_case_no(), 1);
    }

    #[test]
    fn append_then_recover_counter_and_lines() {
        let p = temp_path("recover");
        let _ = std::fs::remove_file(&p);
        let cb = Casebook::open(&p);

        let mut g = TrialRecord::sample_guilty();
        g.case_no = 1;
        cb.record(&g).unwrap();
        let mut a = TrialRecord::sample_acquitted();
        a.case_no = 2;
        cb.record(&a).unwrap();

        // Counter advances past the highest recorded case.
        assert_eq!(cb.next_case_no(), 3);

        // Exactly two JSON lines, and they round-trip back to TrialRecords.
        let txt = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = txt.lines().collect();
        assert_eq!(lines.len(), 2);
        let back: TrialRecord = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(back.case_no, 1);
        assert!(back.guilty);

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn recovery_ignores_a_torn_final_line() {
        let p = temp_path("torn");
        std::fs::write(&p, "{\"case_no\":5,\"x\":1}\n{ half-written cra").unwrap();
        assert_eq!(Casebook::open(&p).next_case_no(), 6);
        let _ = std::fs::remove_file(&p);
    }
}
