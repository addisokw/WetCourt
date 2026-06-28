//! The curated crimes list lives in the shared `crimes-core` crate so the
//! standalone `crimes-editor` binary can reuse the exact same store, validation
//! and persistence. Re-exported here so the orchestrator keeps using
//! `crate::crimes::{Crime, CrimeStore}` unchanged.
pub use crimes_core::*;
