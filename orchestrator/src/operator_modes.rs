//! Secret operator modes, armed from the booth phone's keypad (see the
//! `counsel` crate's operator console) or `POST /operator/modes/arm`.
//!
//! Lifecycle: codes are **armed** while the court is idle, **latch** into the
//! next trial on the Idle→GeneratingCharge edge (armed → active, armed
//! cleared), and the active set is spent when that trial returns to Idle. An
//! e-stop clears everything. The case monitor shows the sets as a discreet
//! bare-number indicator, so the operator can verify what's armed before
//! pressing the button.
//!
//! The registry is deliberately tiny and explicit: one entry per code, all
//! consumers gate on `active_contains(CODE_*)`. Adding a mode = one registry
//! line plus whatever behavior it gates.

use std::collections::BTreeSet;
use std::sync::Mutex;

/// Mode 42: the defendant is found NOT GUILTY no matter what. Steers the
/// verdict prompt toward acquittal and hard-forces the parsed verdict.
pub const CODE_INNOCENT: u16 = 42;

/// Reserved action code (not a mode): dialing `#0#` on the phone clears every
/// armed mode. Handled by the arm endpoint before the registry lookup, so it
/// is never armable and never shown as an active indicator.
pub const CODE_DISARM: u16 = 0;

/// Reserved action code (not a mode): dialing `#99#` aborts the current trial
/// back to Idle (the e-stop path — cancels speech, resets hardware). Works
/// mid-trial, unlike arming; a no-op when already idle.
pub const CODE_RESET: u16 = 99;

/// Reserved action code (not a mode): dialing `#88#` toggles the cross-exam
/// lawyer-call integration on/off (the same `lawyer_enabled` flag the operator
/// console exposes). Off = no automatic "your lawyer is calling" ring during
/// cross-examination.
pub const CODE_LAWYER_TOGGLE: u16 = 88;

pub struct ModeDef {
    pub code: u16,
    pub slug: &'static str,
    pub description: &'static str,
}

pub static REGISTRY: &[ModeDef] = &[ModeDef {
    code: CODE_INNOCENT,
    slug: "innocent",
    description: "defendant is found NOT GUILTY no matter what",
}];

pub fn lookup(code: u16) -> Option<&'static ModeDef> {
    REGISTRY.iter().find(|m| m.code == code)
}

#[derive(Default)]
struct Sets {
    armed: BTreeSet<u16>,
    active: BTreeSet<u16>,
}

/// Shared armed/active mode state. One mutex over both sets so the latch is
/// atomic (a snapshot can never observe a code in both or neither mid-latch).
#[derive(Default)]
pub struct OperatorModes {
    inner: Mutex<Sets>,
}

impl OperatorModes {
    /// Arm a code for the next trial. `Err(())` for codes not in the registry;
    /// arming an already-armed code is idempotent.
    pub fn arm(&self, code: u16) -> Result<(), ()> {
        if lookup(code).is_none() {
            return Err(());
        }
        self.inner.lock().unwrap().armed.insert(code);
        Ok(())
    }

    /// Operator changed their mind before the trial. Returns true if anything
    /// was armed.
    pub fn clear_armed(&self) -> bool {
        let mut s = self.inner.lock().unwrap();
        let changed = !s.armed.is_empty();
        s.armed.clear();
        changed
    }

    /// Trial-start edge: armed becomes active, armed clears — each arm-set
    /// governs exactly the one session it was armed for. Returns true if
    /// either set changed.
    pub fn latch(&self) -> bool {
        let mut s = self.inner.lock().unwrap();
        let changed = !s.armed.is_empty() || !s.active.is_empty();
        s.active = std::mem::take(&mut s.armed);
        changed
    }

    /// Entry to Idle: the applied set is spent. Returns true if anything was
    /// active.
    pub fn clear_active(&self) -> bool {
        let mut s = self.inner.lock().unwrap();
        let changed = !s.active.is_empty();
        s.active.clear();
        changed
    }

    /// E-stop: everything off. Returns true if anything was set.
    pub fn clear_all(&self) -> bool {
        let mut s = self.inner.lock().unwrap();
        let changed = !s.armed.is_empty() || !s.active.is_empty();
        s.armed.clear();
        s.active.clear();
        changed
    }

    pub fn active_contains(&self, code: u16) -> bool {
        self.inner.lock().unwrap().active.contains(&code)
    }

    /// (armed, active) as sorted vecs, for the display event and snapshot.
    pub fn snapshot(&self) -> (Vec<u16>, Vec<u16>) {
        let s = self.inner.lock().unwrap();
        (s.armed.iter().copied().collect(), s.active.iter().copied().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_validates_against_registry() {
        let m = OperatorModes::default();
        assert!(m.arm(CODE_INNOCENT).is_ok());
        assert!(m.arm(99).is_err());
        assert_eq!(m.snapshot(), (vec![CODE_INNOCENT], vec![]));
    }

    #[test]
    fn double_arm_is_idempotent() {
        let m = OperatorModes::default();
        m.arm(CODE_INNOCENT).unwrap();
        m.arm(CODE_INNOCENT).unwrap();
        assert_eq!(m.snapshot(), (vec![CODE_INNOCENT], vec![]));
    }

    #[test]
    fn latch_moves_armed_to_active_and_spends_on_idle() {
        let m = OperatorModes::default();
        m.arm(CODE_INNOCENT).unwrap();
        assert!(m.latch());
        assert_eq!(m.snapshot(), (vec![], vec![CODE_INNOCENT]));
        assert!(m.active_contains(CODE_INNOCENT));

        // Trial over: the applied set is spent.
        assert!(m.clear_active());
        assert_eq!(m.snapshot(), (vec![], vec![]));

        // A second latch with nothing armed changes nothing.
        assert!(!m.latch());
        assert!(!m.clear_active());
    }

    #[test]
    fn relatch_without_rearming_starts_clean() {
        let m = OperatorModes::default();
        m.arm(CODE_INNOCENT).unwrap();
        m.latch();
        // Next trial starts without a re-arm: active must drop to empty.
        assert!(m.latch());
        assert_eq!(m.snapshot(), (vec![], vec![]));
    }

    #[test]
    fn estop_clears_everything() {
        let m = OperatorModes::default();
        m.arm(CODE_INNOCENT).unwrap();
        m.latch();
        m.arm(CODE_INNOCENT).unwrap(); // armed again mid-trial-ish
        assert!(m.clear_all());
        assert_eq!(m.snapshot(), (vec![], vec![]));
        assert!(!m.clear_all());
    }

    #[test]
    fn clear_armed_leaves_active() {
        let m = OperatorModes::default();
        m.arm(CODE_INNOCENT).unwrap();
        m.latch();
        assert!(!m.clear_armed()); // nothing armed post-latch
        assert!(m.active_contains(CODE_INNOCENT));
    }
}
