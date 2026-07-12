//! Thermal-printer keepsake transcript — a physical record of a trial, handed
//! to the defendant on their way out.
//!
//! Milestones:
//! - **M1 (this):** the report renderer ([`report::render`]) + docket-alias
//!   generator, exercised by unit tests that dump ESC/POS to temp files and can
//!   print to a connected USB unit via `WETCOURT_PRINT_USB=1`. No live trial or
//!   printer required to develop the layout.
//! - **M2:** assemble a [`TrialRecord`] from real state-machine data (persisted
//!   case counter, judge name, wall-clock stamp), read `[printer]` config, and
//!   print at trial end behind a mock/real toggle.
//! - **M3:** fill the reserved "moment of justice" photo slot from the vision
//!   service's firing-still.

pub mod casebook;
pub mod custom;
pub mod record;
pub mod report;
pub mod service;
pub mod templates;

pub use casebook::Casebook;
pub use record::TrialRecord;
pub use report::{render, ReportOpts};

/// Fold the smart punctuation that LLM/STT (and operator) text is full of down
/// to the ASCII the printer can render — otherwise `Builder::text` would stamp
/// each curly quote / em-dash as a literal `?`.
pub(crate) fn asciify(s: &str) -> String {
    let s = s.replace('\u{2026}', "..."); // ellipsis
    s.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{2032}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{2033}' => '"',
            '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
            '\u{00A0}' => ' ',
            c if c.is_ascii() => c,
            _ => ' ', // anything else we can't print: a space beats a '?'
        })
        .collect()
}
