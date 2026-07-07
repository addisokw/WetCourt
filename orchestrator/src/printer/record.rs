//! The data captured from one trial, plus the docket-alias generator.

use serde::{Deserialize, Serialize};

use crate::state_machine::states::CrossExam;

/// Everything the keepsake transcript needs about a single trial — and also the
/// exact shape persisted to the casebook (one JSON line per verdict). Assembled
/// at verdict time (M2) and handed to [`super::report::render`]. It owns its
/// strings so a finished record can outlive the trial's transient state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrialRecord {
    /// Monotonic case counter. Recovered on startup from the casebook (highest
    /// `case_no` on disk + 1). Drives both the printed "Case No." and the
    /// deterministic docket alias.
    pub case_no: u64,
    /// Wall-clock capture time in RFC 3339, e.g. "2026-06-28T14:09:33-05:00".
    /// Canonical/sortable for the log; the printed header derives a friendly
    /// form via [`Self::display_time`].
    pub ts: String,
    /// The absurd charge drawn against the defendant.
    pub charge: String,
    /// Verbatim STT plea. May be the [`NO_DEFENSE`] sentinel when the defendant
    /// said nothing intelligible.
    ///
    /// [`NO_DEFENSE`]: crate::state_machine::states::NO_DEFENSE
    pub plea: String,
    /// The judge's one follow-up and the defendant's reply, when cross-exam ran.
    pub cross: Option<CrossExam>,
    /// Presiding judge's display name (a persona's `display_name`).
    pub judge_name: String,
    pub guilty: bool,
    /// The judge's full deliberation text (markers already stripped upstream).
    pub deliberation: String,
    /// Short verdict tagline, e.g. "Justice, as ever, is wet."
    pub remarks: String,
    /// The 2–4 word deciding factor the judge named ("sincere apology"). `None`
    /// on fallback verdicts or when the model omitted it. `#[serde(default)]`
    /// keeps older casebook lines (written before this field existed) loadable.
    #[serde(default)]
    pub key_factor: Option<String>,
}

impl TrialRecord {
    /// The printed case number, e.g. `WCA-0042`.
    pub fn case_label(&self) -> String {
        format!("WCA-{:04}", self.case_no)
    }

    /// The defendant's generated nickname, e.g. `The Soggy Litigant`.
    pub fn docket_alias(&self) -> String {
        docket_alias(self.case_no)
    }

    /// "YYYY-MM-DD HH:MM" carved out of the RFC 3339 `ts` for the printed
    /// header. Falls back to the raw string if `ts` isn't the expected shape.
    pub fn display_time(&self) -> String {
        let t = &self.ts;
        if t.len() >= 16 && t.as_bytes()[10] == b'T' {
            format!("{} {}", &t[..10], &t[11..16])
        } else {
            t.clone()
        }
    }
}

/// A deterministic, PII-free nickname for the defendant, derived solely from the
/// case number — the same case always yields the same alias, so it needs no RNG
/// or clock and is stable across a reprint.
pub fn docket_alias(case_no: u64) -> String {
    // SplitMix64-style scramble so adjacent case numbers don't land on adjacent
    // words (otherwise #41/#42/#43 would march down the wordlist in lockstep).
    let mut z = case_no.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;

    let adj = ADJECTIVES[(z % ADJECTIVES.len() as u64) as usize];
    let noun = NOUNS[((z >> 20) % NOUNS.len() as u64) as usize];
    format!("The {adj} {noun}")
}

#[rustfmt::skip]
const ADJECTIVES: &[&str] = &[
    "Soggy", "Drenched", "Waterlogged", "Dripping", "Sodden", "Damp",
    "Saturated", "Sopping", "Briny", "Misty", "Sloshed", "Squelching",
    "Bedraggled", "Clammy", "Marshy", "Drizzled",
];

#[rustfmt::skip]
const NOUNS: &[&str] = &[
    "Litigant", "Defendant", "Scoundrel", "Miscreant", "Rapscallion",
    "Offender", "Culprit", "Suspect", "Accused", "Reprobate", "Rascal",
    "Perpetrator", "Wrongdoer", "Malefactor", "Brigand", "Knave",
];

#[cfg(test)]
impl TrialRecord {
    /// A canned guilty trial for layout iteration / tests.
    pub fn sample_guilty() -> Self {
        Self {
            case_no: 42,
            ts: "2026-06-28T14:09:33-05:00".into(),
            charge: "Operating a rubber duck at an unlicensed volume within 50ft \
                     of a municipal fountain"
                .into(),
            plea: "Your honor, the duck consented, I have the paperwork right here, \
                   and frankly the fountain started it."
                .into(),
            cross: Some(CrossExam {
                question: "And where, precisely, is this paperwork now?".into(),
                answer: "...it got wet.".into(),
            }),
            judge_name: "Judge Wettington".into(),
            guilty: true,
            deliberation: "The court has weighed the defense and found it, like the \
                           defendant, all wet. To blame a fountain for one's own \
                           acoustic crimes is the last refuge of the unprepared. \
                           The paperwork's convenient dissolution persuades no one."
                .into(),
            remarks: "Justice, as ever, is wet.".into(),
            key_factor: Some("blamed the fountain".into()),
        }
    }

    /// A canned acquittal — exercises the not-guilty branch (no photo slot).
    pub fn sample_acquitted() -> Self {
        Self {
            case_no: 7,
            ts: "2026-06-28T14:21:00-05:00".into(),
            charge: "Suspicion of being far too dry in a designated splash zone".into(),
            plea: "I was framed. I am, if anything, the wettest person here.".into(),
            cross: None,
            judge_name: "Judge Wettington".into(),
            guilty: false,
            deliberation: "A spirited and, the court notes, visibly damp defense. \
                           The accused carries themselves with a moisture befitting \
                           this venue. The charge does not hold water."
                .into(),
            remarks: "Acquitted. Do not let it happen again.".into(),
            key_factor: Some("disarming honesty".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_is_deterministic() {
        assert_eq!(docket_alias(42), docket_alias(42));
        assert_eq!(docket_alias(1000), docket_alias(1000));
    }

    #[test]
    fn alias_varies_across_adjacent_cases() {
        // The scramble should keep neighbouring case numbers from colliding.
        let a = docket_alias(41);
        let b = docket_alias(42);
        let c = docket_alias(43);
        assert!(a != b || b != c, "adjacent aliases all identical: {a} / {b} / {c}");
    }

    #[test]
    fn alias_is_well_formed() {
        let s = docket_alias(42);
        assert!(s.starts_with("The "), "got {s}");
        assert_eq!(s.split_whitespace().count(), 3, "expected 'The Adj Noun', got {s}");
    }

    #[test]
    fn display_time_carves_friendly_form() {
        assert_eq!(TrialRecord::sample_guilty().display_time(), "2026-06-28 14:09");
        // Malformed ts falls back to the raw string.
        let mut rec = TrialRecord::sample_guilty();
        rec.ts = "whenever".into();
        assert_eq!(rec.display_time(), "whenever");
    }

    #[test]
    fn case_label_is_zero_padded() {
        assert_eq!(TrialRecord::sample_guilty().case_label(), "WCA-0042");
        assert_eq!(TrialRecord::sample_acquitted().case_label(), "WCA-0007");
    }
}
