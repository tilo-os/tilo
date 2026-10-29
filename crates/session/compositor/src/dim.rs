//! The idle dim that precedes a blank ([`crate::idle::SessionTransition::Dimmed`]).
//!
//! With a backlight the panel is lowered to [`DIM_FRACTION`] of whatever the
//! person had and put back exactly on input. Desktops and most VMs have no
//! backlight to lower, so there the scene is darkened by an overlay instead
//! ([`ScreenDimmer::overlay_alpha`]); either way the person sees the same
//! warning. The session supplies the backlight ([`Backlight`]); the
//! compositor knows no brightness device.

use crate::idle::SessionManager;

/// The dimmed brightness as a fraction of the brightness before dimming.
pub const DIM_FRACTION: f32 = 0.3;

/// Opacity of the black overlay that stands in for a backlight: leaving `DIM_FRACTION` of the
/// light through looks like the same dim a backlight would give.
pub const OVERLAY_ALPHA: f32 = 1.0 - DIM_FRACTION;

/// The display's backlight, as far as dimming needs it. Fractions are `0.0..=1.0`.
pub trait Backlight {
    /// The current brightness, or `None` when there is no controllable backlight.
    fn brightness(&self) -> Option<f32>;
    /// Set the brightness. Returns whether the write took effect.
    fn set_brightness(&self, fraction: f32) -> bool;
    /// Whether the panel's backlight is powered (`bl_power` 0), as the
    /// kernel holds it; `None` when it says nothing. A wake that leaves it
    /// blanked has not lit the panel.
    fn powered(&self) -> Option<bool> {
        None
    }
    /// The backlight as the kernel holds it, for the record.
    fn describe(&self) -> Option<String> {
        None
    }
}

/// No controllable backlight: the overlay dims.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoBacklight;

impl Backlight for NoBacklight {
    fn brightness(&self) -> Option<f32> {
        None
    }

    fn set_brightness(&self, _fraction: f32) -> bool {
        false
    }
}

impl Backlight for Box<dyn Backlight> {
    fn brightness(&self) -> Option<f32> {
        (**self).brightness()
    }

    fn set_brightness(&self, fraction: f32) -> bool {
        (**self).set_brightness(fraction)
    }

    fn powered(&self) -> Option<bool> {
        (**self).powered()
    }

    fn describe(&self) -> Option<String> {
        (**self).describe()
    }
}

/// How the display is currently dimmed.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Dim {
    /// Full brightness.
    Off,
    /// Backlight lowered; holds the brightness to restore.
    Backlight(f32),
    /// No backlight: the scene is drawn under the overlay.
    Overlay,
}

/// Owns the dim for one session: remembers what to restore and whether the overlay is needed.
#[derive(Debug)]
pub struct ScreenDimmer<B: Backlight = Box<dyn Backlight>> {
    backlight: B,
    dim: Dim,
}

impl Default for ScreenDimmer {
    fn default() -> Self {
        Self::new(Box::new(NoBacklight))
    }
}

impl std::fmt::Debug for dyn Backlight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Backlight")
    }
}

impl<B: Backlight> ScreenDimmer<B> {
    /// A dimmer driving `backlight`, starting at full brightness.
    pub fn new(backlight: B) -> Self {
        Self {
            backlight,
            dim: Dim::Off,
        }
    }

    /// Dim the display: lower the backlight when there is one, otherwise switch to the overlay.
    /// Dimming twice keeps the first saved brightness.
    pub fn dim(&mut self) {
        if self.dim != Dim::Off {
            return;
        }
        self.dim = match self.backlight.brightness() {
            Some(current) if self.backlight.set_brightness(current * DIM_FRACTION) => {
                Dim::Backlight(current)
            }
            _ => Dim::Overlay,
        };
    }

    /// Put the display back to the brightness it had before [`Self::dim`].
    /// A failed write keeps the saved level for the next restore, so the
    /// dimmed level is never taken for the user's own.
    pub fn restore(&mut self) {
        if let Dim::Backlight(saved) = self.dim
            && !self.backlight.set_brightness(saved)
        {
            return;
        }
        self.dim = Dim::Off;
    }

    /// Keep the session's dim state what the panel shows. A restore the
    /// driver refused leaves the panel dimmed, so the session must not take
    /// it for lifted: the next input then lifts it again.
    pub fn keep_session_truthful(&self, session: &mut SessionManager) {
        if matches!(self.dim, Dim::Backlight(_)) {
            session.keep_dimmed();
        }
    }

    /// The panel's backlight.
    pub fn backlight(&self) -> &B {
        &self.backlight
    }

    /// Whether the dim holds the backlight lowered, so nothing else (an
    /// ambient-light loop) should raise it behind the person's back.
    pub fn holds_backlight(&self) -> bool {
        matches!(self.dim, Dim::Backlight(_))
    }

    /// Opacity of the dimming overlay drawn on top of the scene, if any.
    pub fn overlay_alpha(&self) -> Option<f32> {
        (self.dim == Dim::Overlay).then_some(OVERLAY_ALPHA)
    }
}

impl<B: Backlight> Drop for ScreenDimmer<B> {
    /// A session ending while dimmed leaves the panel at the user's level.
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeBacklight {
        level: RefCell<Option<f32>>,
        writes: RefCell<Vec<f32>>,
        refuses: std::cell::Cell<bool>,
    }

    impl FakeBacklight {
        fn at(level: f32) -> Self {
            Self {
                level: RefCell::new(Some(level)),
                ..Self::default()
            }
        }
    }

    impl Backlight for &FakeBacklight {
        fn brightness(&self) -> Option<f32> {
            *self.level.borrow()
        }
        fn set_brightness(&self, fraction: f32) -> bool {
            if self.level.borrow().is_none() || self.refuses.get() {
                return false;
            }
            *self.level.borrow_mut() = Some(fraction);
            self.writes.borrow_mut().push(fraction);
            true
        }
    }

    #[test]
    fn dimming_lowers_the_backlight_to_a_fraction_of_the_current_level_and_restores_it() {
        let backlight = FakeBacklight::at(0.8);
        let mut dimmer = ScreenDimmer::new(&backlight);
        dimmer.dim();
        let lowered = backlight.level.borrow().unwrap();
        assert!((lowered - 0.8 * DIM_FRACTION).abs() < 1e-6);
        assert_eq!(dimmer.overlay_alpha(), None, "a backlight needs no overlay");

        dimmer.dim();
        assert_eq!(
            backlight.writes.borrow().len(),
            1,
            "dimming twice keeps the saved level"
        );

        dimmer.restore();
        assert_eq!(*backlight.level.borrow(), Some(0.8));
        dimmer.restore();
        assert_eq!(
            backlight.writes.borrow().len(),
            2,
            "restoring twice writes once"
        );
    }

    #[test]
    fn without_a_backlight_the_overlay_dims_instead() {
        let backlight = FakeBacklight::default();
        let mut dimmer = ScreenDimmer::new(&backlight);
        assert_eq!(dimmer.overlay_alpha(), None);
        dimmer.dim();
        assert_eq!(dimmer.overlay_alpha(), Some(OVERLAY_ALPHA));
        dimmer.restore();
        assert_eq!(dimmer.overlay_alpha(), None);
        assert!(backlight.writes.borrow().is_empty());
    }

    #[test]
    fn a_failed_restore_keeps_the_saved_level_for_the_next_one() {
        let backlight = FakeBacklight::at(0.8);
        let mut dimmer = ScreenDimmer::new(&backlight);
        dimmer.dim();
        backlight.refuses.set(true);
        dimmer.restore();
        dimmer.dim();
        backlight.refuses.set(false);
        dimmer.restore();
        assert_eq!(*backlight.level.borrow(), Some(0.8));
    }

    #[test]
    fn a_refused_restore_leaves_the_session_dimmed_for_the_next_input() {
        use crate::idle::{IdleConfig, SessionTransition};
        use std::time::{Duration, Instant};
        let backlight = FakeBacklight::at(0.8);
        let mut dimmer = ScreenDimmer::new(&backlight);
        let start = Instant::now();
        let mut session = SessionManager::new(
            IdleConfig {
                blank_timeout: Some(Duration::from_secs(600)),
                lock_timeout: None,
                suspend_timeout: None,
            },
            start,
        );
        let mut apply = |transitions: Vec<SessionTransition>, session: &mut SessionManager| {
            for transition in transitions {
                match transition {
                    SessionTransition::Dimmed => dimmer.dim(),
                    SessionTransition::Undimmed => dimmer.restore(),
                    _ => {}
                }
            }
            dimmer.keep_session_truthful(session);
        };
        let at = |seconds| start + Duration::from_secs(seconds);
        apply(session.tick(at(590)), &mut session);
        assert!(session.is_dimmed());
        backlight.refuses.set(true);
        apply(session.note_activity(at(591)), &mut session);
        assert!(
            session.is_dimmed(),
            "a dim that could not be lifted is still the display's state"
        );
        backlight.refuses.set(false);
        apply(session.note_activity(at(592)), &mut session);
        assert_eq!(*backlight.level.borrow(), Some(0.8));
        assert!(!session.is_dimmed());
    }

    #[test]
    fn dropping_a_dimmed_session_restores_the_backlight() {
        let backlight = FakeBacklight::at(0.6);
        let mut dimmer = ScreenDimmer::new(&backlight);
        dimmer.dim();
        drop(dimmer);
        assert_eq!(*backlight.level.borrow(), Some(0.6));
    }
}
