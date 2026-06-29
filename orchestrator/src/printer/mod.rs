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
pub mod record;
pub mod report;
pub mod service;

pub use casebook::Casebook;
pub use record::TrialRecord;
pub use report::{render, ReportOpts};
