//! Session-state tracking for the DRM backend.
//!
//! `libseat` reports `Enable`/`Disable` events when the user switches
//! VTs. While disabled, all DRM/page-flip operations have to be paused
//! and queued; while enabled, normal frame submission resumes. This
//! module owns the small state machine that translates the raw
//! seat-events into the activation flag the rest of the backend reads.
//!
//! The state machine itself is pure: it takes events in and returns the
//! new state. Tests drive it directly with synthetic events. The
//! integration with the real `LibSeatSession` lives in
//! `super::runtime::DrmRuntime` (hardware-only path).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// What happened on the seat channel. Mirrors `smithay::backend::session
/// ::Event` so the runtime layer can map them 1:1.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SeatTick {
    /// VT activated — drm ops resume.
    Activate,
    /// VT swapped away — drm ops must pause.
    Pause,
}

/// Pure state machine. Holds an atomic flag so the rendering thread can
/// read activation without synchronising with the event-dispatch
/// thread. Construct via [`SessionState::new_active`] (libseat reports
/// active immediately when we own the seat).
#[derive(Debug, Clone)]
pub struct SessionState {
    active: Arc<AtomicBool>,
}

impl SessionState {
    /// Build a session that starts in the active state. Most callers
    /// want this — `LibSeatSession::new` dispatches the initial
    /// `Enable` event during construction, so by the time we wrap it
    /// we're already on the active VT.
    pub fn new_active() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Build a session that starts paused — useful for tests that want
    /// to assert the first `Activate` flips the flag.
    pub fn new_paused() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Apply a seat-tick to the state. Returns the new active flag.
    pub fn apply(&self, tick: SeatTick) -> bool {
        match tick {
            SeatTick::Activate => {
                self.active.store(true, Ordering::Release);
                true
            }
            SeatTick::Pause => {
                self.active.store(false, Ordering::Release);
                false
            }
        }
    }

    /// Snapshot the active flag. Cheap (atomic load).
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// Clone the inner atomic so a background thread (e.g. the
    /// libinput drain) can poll activation without going through `&self`.
    pub fn shared_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_active_starts_active() {
        let s = SessionState::new_active();
        assert!(s.is_active());
    }

    #[test]
    fn new_paused_starts_paused() {
        let s = SessionState::new_paused();
        assert!(!s.is_active());
    }

    #[test]
    fn pause_then_activate_returns_to_active() {
        let s = SessionState::new_active();
        s.apply(SeatTick::Pause);
        assert!(!s.is_active());
        s.apply(SeatTick::Activate);
        assert!(s.is_active());
    }

    #[test]
    fn repeated_activate_stays_active() {
        let s = SessionState::new_active();
        s.apply(SeatTick::Activate);
        s.apply(SeatTick::Activate);
        assert!(s.is_active());
    }

    #[test]
    fn repeated_pause_stays_paused() {
        let s = SessionState::new_active();
        s.apply(SeatTick::Pause);
        s.apply(SeatTick::Pause);
        assert!(!s.is_active());
    }

    #[test]
    fn apply_returns_new_state() {
        let s = SessionState::new_active();
        assert!(!s.apply(SeatTick::Pause));
        assert!(s.apply(SeatTick::Activate));
    }

    #[test]
    fn shared_flag_reflects_apply() {
        // Simulate a render thread reading the same atomic the
        // dispatch thread writes.
        let s = SessionState::new_active();
        let flag = s.shared_flag();
        assert!(flag.load(Ordering::Acquire));
        s.apply(SeatTick::Pause);
        assert!(!flag.load(Ordering::Acquire));
    }
}
