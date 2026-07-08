//! Vision fire gate for trial firing.
//!
//! Vision computes a per-frame `fire_ok` (the aim is locked on the selected
//! target — see `vision/vision.py`) and streams it to the orchestrator on the
//! aim POST. This gate stores the latest verdict and, when vision targeting is
//! **armed**, lets a trial `FIRE` reach the squirt only on a *fresh* `fire_ok`.
//! The freshness check is what makes "no target / no person / vision crashed"
//! fail safe: vision only posts while it is actively tracking, so a stale
//! timestamp means we have no current evidence it is locked on a target.
//!
//! (The eye-exclusion safety zone that once also fed `fire_ok` was retired when
//! the softer nozzle made the stream safe; the lock + arm gating here remains.)
//!
//! When not armed the gate is transparent — the operator owns the aim manually
//! (legacy behaviour), so trial fire passes through unchanged.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// A `fire_ok` update older than this is treated as no evidence at all → no fire
/// while armed. Vision posts aim at ~15 Hz (~66 ms), so 300 ms tolerates a few
/// dropped frames without ever firing on a stale verdict.
pub const FIRE_OK_STALE_MS: u64 = 300;

/// Process-monotonic milliseconds. Used to timestamp `fire_ok` updates and to
/// age them at the gate — a single clock so the staleness check is consistent
/// between the writer (the aim handler) and the reader (the fire gate).
pub fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Shared vision fire-gate state: the latest `fire_ok` from vision and when it landed.
pub struct VisionFireGate {
    fire_ok: AtomicBool,
    updated_ms: AtomicU64,
    stale_ms: u64,
}

impl VisionFireGate {
    pub fn new(stale_ms: u64) -> Self {
        Self {
            fire_ok: AtomicBool::new(false),
            // 0 reads as "ancient" until the first real update lands, so the gate
            // is closed (when armed) before vision has said anything.
            updated_ms: AtomicU64::new(0),
            stale_ms,
        }
    }

    /// Record the latest safety verdict streamed from vision. Called on every
    /// aim POST regardless of arm state, so the gate has fresh data the instant
    /// the operator arms.
    pub fn record(&self, fire_ok: bool) {
        // Order matters for the reader: stamp the time only after the value, so a
        // racing `fire_allowed` can never see a fresh timestamp paired with a
        // stale `true`. (Both relaxed loads, but value-before-time is the safe
        // pairing for a fail-safe gate.)
        self.fire_ok.store(fire_ok, Ordering::Relaxed);
        self.updated_ms.store(now_ms(), Ordering::Relaxed);
    }

    /// Whether a trial `FIRE` may reach the squirt right now. Transparent when
    /// disarmed; otherwise requires a fresh `fire_ok`.
    pub fn fire_allowed(&self, armed: bool) -> bool {
        let age = now_ms().saturating_sub(self.updated_ms.load(Ordering::Relaxed));
        decide(
            armed,
            self.fire_ok.load(Ordering::Relaxed),
            age,
            self.stale_ms,
        )
    }
}

/// Pure decision: disarmed always fires (operator owns aim); armed fires only on
/// a fresh, true `fire_ok`. Fail-safe — unknown/stale ⇒ no fire when armed.
fn decide(armed: bool, fire_ok: bool, age_ms: u64, stale_ms: u64) -> bool {
    !armed || (fire_ok && age_ms <= stale_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disarmed_always_fires() {
        // Operator owns the aim manually — fire regardless of fire_ok/staleness.
        assert!(decide(false, false, 9999, 300));
        assert!(decide(false, true, 0, 300));
    }

    #[test]
    fn armed_requires_fresh_fire_ok() {
        assert!(decide(true, true, 0, 300)); // fresh + ok → fire
        assert!(decide(true, true, 300, 300)); // exactly at the edge still counts
    }

    #[test]
    fn armed_holds_when_not_ok() {
        assert!(!decide(true, false, 0, 300)); // fresh but not ok → hold
    }

    #[test]
    fn armed_holds_when_stale() {
        assert!(!decide(true, true, 301, 300)); // ok but stale → hold
        assert!(!decide(true, false, 301, 300)); // stale and not ok → hold
    }

    #[test]
    fn fresh_record_clears_gate_when_armed() {
        let gate = VisionFireGate::new(FIRE_OK_STALE_MS);
        assert!(!gate.fire_allowed(true)); // nothing recorded yet → closed
        gate.record(true);
        assert!(gate.fire_allowed(true)); // fresh ok → open
        gate.record(false);
        assert!(!gate.fire_allowed(true)); // fresh not-ok → closed
    }

    #[test]
    fn disarmed_is_transparent_even_with_no_data() {
        let gate = VisionFireGate::new(FIRE_OK_STALE_MS);
        assert!(gate.fire_allowed(false)); // never recorded, but disarmed → fire
    }
}
