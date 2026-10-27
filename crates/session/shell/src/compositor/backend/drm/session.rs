//! Session-state tracking for the DRM backend.
//!
//! `libseat` reports `Enable`/`Disable` events when the user switches
//! VTs. While disabled, all DRM/page-flip operations have to be paused
//! and queued; while enabled, normal frame submission resumes. This
//! module owns the small state machine that translates the raw
//! seat-events into the activation flag the rest of the backend reads.
//!
//! The state machine itself is pure: it takes events in and returns the
//! new state plus the [`SessionAction`]s that state change implies. Tests
//! drive it directly with synthetic events. The integration with
//! `LibSeatSession` lives in `super::runtime::DrmRuntime` (hardware-only
//! path).
//!
//! # Why a full sequence, not just the flag
//!
//! VT switch and suspend hit the same failure shape: page flips queued
//! against a device the kernel no longer grants master on, or panels that
//! power back on without a re-modeset and stay dark. Both are fixed by
//! the same ordered sequence rather than being open-coded twice: give up
//! the hardware on the way out, take it back and re-establish
//! *everything* on the way in. Nothing about a resumed device can be
//! assumed to have survived — not the mode, not the framebuffer binding,
//! not the cursor plane, not the panel's power state.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What happened on the seat channel. Mirrors `smithay::backend::session
/// ::Event` so the runtime layer can map them 1:1.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SeatTick {
    /// VT activated — drm ops resume.
    Activate,
    /// VT swapped away — drm ops must pause.
    Pause,
}

/// One step of the pause/resume sequence. Ordered: the runtime performs
/// them in the order the state machine returns them, and the order is the
/// contract — dropping master before stopping scanout would queue a flip
/// against a device we no longer own, and repainting before the modeset
/// would blit into a framebuffer no CRTC is scanning.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SessionAction {
    /// Stop presenting. Every subsequent frame is dropped until the
    /// session is active again.
    PauseScanout,
    /// Give up DRM master so the incoming session can take it.
    DropMaster,
    /// Take DRM master back.
    AcquireMaster,
    /// Re-run modeset on every live output and re-bind its cursor plane.
    /// A device that has been through a VT switch or a suspend retains
    /// none of that.
    ResetOutputs,
    /// Put the panels back into the power state the session had before it
    /// was paused — a session that blanked for idle must not come back lit.
    RestoreDpms,
    /// Resume presenting.
    ResumeScanout,
    /// Discard any damage tracking and repaint everything: the scanout
    /// buffers hold whatever the other session left in them.
    FullDamage,
}

/// The sequence to run when the session gains the hardware. Shared by the
/// VT-switch path and the resume-from-sleep path, because the state the
/// device comes back in is the same either way.
pub const ACTIVATE_SEQUENCE: [SessionAction; 5] = [
    SessionAction::AcquireMaster,
    SessionAction::ResetOutputs,
    SessionAction::RestoreDpms,
    SessionAction::ResumeScanout,
    SessionAction::FullDamage,
];

/// The sequence to run when the session loses the hardware.
pub const PAUSE_SEQUENCE: [SessionAction; 2] =
    [SessionAction::PauseScanout, SessionAction::DropMaster];

/// Pure state machine. Holds atomic flags so the rendering thread can
/// read activation without synchronising with the event-dispatch
/// thread. Construct via [`SessionState::new_active`] (libseat reports
/// active immediately when we own the seat).
#[derive(Debug, Clone)]
pub struct SessionState {
    active: Arc<AtomicBool>,
    /// Whether the panels should be lit. Tracked across a pause so
    /// [`SessionAction::RestoreDpms`] can put back what the session had
    /// rather than unconditionally lighting the screen — a machine that
    /// VT-switched while idle-blanked must not wake up on the way back.
    dpms_on: Arc<AtomicBool>,
}

impl SessionState {
    /// Build a session that starts in the active state. Most callers
    /// want this — `LibSeatSession::new` dispatches the initial
    /// `Enable` event during construction, so by the time we wrap it
    /// we're already on the active VT.
    pub fn new_active() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(true)),
            dpms_on: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Build a session that starts paused — useful for tests that want
    /// to assert the first `Activate` flips the flag.
    pub fn new_paused() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(false)),
            dpms_on: Arc::new(AtomicBool::new(true)),
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

    /// Apply a seat-tick and report the work it implies.
    ///
    /// A tick that does not change the state returns an empty list: a
    /// redundant `Activate` (libseat re-announces on some paths) must not
    /// re-modeset a screen that is already correct, because a modeset is a
    /// visible flicker.
    pub fn apply_with_actions(&self, tick: SeatTick) -> Vec<SessionAction> {
        let was_active = self.is_active();
        self.apply(tick);
        match (was_active, tick) {
            (false, SeatTick::Activate) => ACTIVATE_SEQUENCE.to_vec(),
            (true, SeatTick::Pause) => PAUSE_SEQUENCE.to_vec(),
            _ => Vec::new(),
        }
    }

    /// The sequence for coming back from system suspend.
    ///
    /// Unconditional, unlike [`Self::apply_with_actions`]: a suspend does
    /// not pause the seat, so the session was never marked inactive, yet
    /// the DRM device came back needing every one of these steps. Gating
    /// this on a state change is exactly how "lid close, lid open, black
    /// screen" happens.
    pub fn resume_actions(&self) -> Vec<SessionAction> {
        self.active.store(true, Ordering::Release);
        ACTIVATE_SEQUENCE.to_vec()
    }

    /// Snapshot the active flag. Cheap (atomic load).
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// Record the power state the session wants its panels in, so a
    /// later [`SessionAction::RestoreDpms`] restores that rather than
    /// guessing.
    pub fn set_dpms(&self, on: bool) {
        self.dpms_on.store(on, Ordering::Release);
    }

    /// The power state to restore on activation.
    pub fn dpms_on(&self) -> bool {
        self.dpms_on.load(Ordering::Acquire)
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
    fn session_state_flag_tracks_pause_and_activate() {
        {
            let s = SessionState::new_active();
            assert!(s.is_active());
        }
        {
            let s = SessionState::new_paused();
            assert!(!s.is_active());
        }
        {
            let s = SessionState::new_active();
            s.apply(SeatTick::Pause);
            assert!(!s.is_active());
            s.apply(SeatTick::Activate);
            assert!(s.is_active());
        }
        {
            let s = SessionState::new_active();
            s.apply(SeatTick::Activate);
            s.apply(SeatTick::Activate);
            assert!(s.is_active());
        }
        {
            let s = SessionState::new_active();
            s.apply(SeatTick::Pause);
            s.apply(SeatTick::Pause);
            assert!(!s.is_active());
        }
        {
            let s = SessionState::new_active();
            assert!(!s.apply(SeatTick::Pause));
            assert!(s.apply(SeatTick::Activate));
        }
        {
            // Simulate a render thread reading the same atomic the
            // dispatch thread writes.
            let s = SessionState::new_active();
            let flag = s.shared_flag();
            assert!(flag.load(Ordering::Acquire));
            s.apply(SeatTick::Pause);
            assert!(!flag.load(Ordering::Acquire));
        }
    }

    #[test]
    fn vt_switch_plans_stop_scanout_first_and_repaint_last() {
        {
            // Order is the contract: dropping master first would leave the
            // presenter queuing flips against a device it no longer owns, and
            // the kernel answers those with EACCES rather than a frame.
            let s = SessionState::new_active();
            let actions = s.apply_with_actions(SeatTick::Pause);
            assert_eq!(
                actions,
                vec![SessionAction::PauseScanout, SessionAction::DropMaster]
            );
            assert!(!s.is_active());
        }
        {
            // The screen after a VT switch has no mode set, no framebuffer
            // bound, and holds whatever the other session drew. Each step is
            // load-bearing and the order is what makes them work.
            let s = SessionState::new_active();
            s.apply_with_actions(SeatTick::Pause);
            let actions = s.apply_with_actions(SeatTick::Activate);
            assert_eq!(
                actions,
                vec![
                    SessionAction::AcquireMaster,
                    SessionAction::ResetOutputs,
                    SessionAction::RestoreDpms,
                    SessionAction::ResumeScanout,
                    SessionAction::FullDamage,
                ]
            );
            assert!(s.is_active());
        }
        {
            let s = SessionState::new_paused();
            let actions = s.apply_with_actions(SeatTick::Activate);
            let master = actions
                .iter()
                .position(|a| *a == SessionAction::AcquireMaster)
                .expect("activation must acquire master");
            let modeset = actions
                .iter()
                .position(|a| *a == SessionAction::ResetOutputs)
                .expect("activation must re-modeset");
            assert!(
                master < modeset,
                "set_crtc without DRM master returns EACCES: {actions:?}"
            );
        }
        {
            // Painting before the modeset writes into a buffer no CRTC is
            // scanning out, which looks exactly like the bug it is meant to
            // fix — a black screen after resume.
            let s = SessionState::new_paused();
            let actions = s.apply_with_actions(SeatTick::Activate);
            assert_eq!(actions.last(), Some(&SessionAction::FullDamage));
        }
        {
            // libseat re-announces activation on some paths. Re-modesetting
            // there is a visible flicker for nothing.
            let s = SessionState::new_active();
            assert!(s.apply_with_actions(SeatTick::Activate).is_empty());
            s.apply_with_actions(SeatTick::Pause);
            assert!(s.apply_with_actions(SeatTick::Pause).is_empty());
        }
    }

    #[test]
    fn resume_runs_the_vt_activate_sequence_and_keeps_the_blank_state() {
        {
            // The lid-close bug: a suspend never pauses the seat, so the
            // session is still marked active on the way back — and a state
            // machine that only acts on transitions does nothing, leaving the
            // panel dark.
            let s = SessionState::new_active();
            assert!(s.is_active(), "suspend does not pause the seat");
            assert_eq!(s.resume_actions(), ACTIVATE_SEQUENCE.to_vec());
        }
        {
            // Two entry points, one recovery. Any divergence would mean one of
            // them is missing a step nobody notices until the hardware differs.
            let vt = SessionState::new_paused();
            let vt_actions = vt.apply_with_actions(SeatTick::Activate);
            let sleep = SessionState::new_active();
            assert_eq!(vt_actions, sleep.resume_actions());
        }
        {
            // Restore must put back what the session had, not light the panel:
            // a VT switch while the screen was idle-blanked must not wake it.
            let s = SessionState::new_active();
            s.set_dpms(false);
            s.apply_with_actions(SeatTick::Pause);
            let actions = s.apply_with_actions(SeatTick::Activate);
            assert!(actions.contains(&SessionAction::RestoreDpms));
            assert!(
                !s.dpms_on(),
                "the remembered power state must survive the pause"
            );
        }
        {
            let s = SessionState::new_active();
            s.set_dpms(true);
            s.apply_with_actions(SeatTick::Pause);
            s.apply_with_actions(SeatTick::Activate);
            assert!(s.dpms_on());
        }
        {
            // A resume that arrives while the seat still reads paused (the
            // ordering is not guaranteed) must not leave the compositor
            // refusing to paint.
            let s = SessionState::new_paused();
            s.resume_actions();
            assert!(s.is_active());
        }
    }
}
