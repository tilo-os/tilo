//! Applies the idle dim that precedes a blank ([`crate::compositor::idle::SessionTransition::Dimmed`]).
//!
//! With a backlight the panel is lowered to [`DIM_FRACTION`] of whatever the user had and put
//! back exactly on input. Desktops and most VMs have no backlight to lower, so there the paint
//! path darkens the scene with an overlay instead ([`ScreenDimmer::overlay_alpha`]); either way
//! the user sees the same warning.

use std::sync::atomic::{AtomicBool, Ordering};

/// Set while the idle dim holds the backlight, so the ambient-light loop does not raise it
/// again behind the user's back; the dim restores the level itself on input.
static BACKLIGHT_HELD: AtomicBool = AtomicBool::new(false);

/// Whether the idle dim currently owns the backlight level.
pub fn backlight_held() -> bool {
    BACKLIGHT_HELD.load(Ordering::Relaxed)
}

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
}

/// The real backlight under `/sys/class/backlight`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SysBacklight;

impl Backlight for SysBacklight {
    fn brightness(&self) -> Option<f32> {
        os_actions::get_brightness().ok()
    }

    fn set_brightness(&self, fraction: f32) -> bool {
        match os_actions::set_brightness(fraction * 100.0) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(target: "tilo::power", %error, "idle dim brightness write failed");
                false
            }
        }
    }
}

/// How the display is currently dimmed.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Dim {
    /// Full brightness.
    Off,
    /// Backlight lowered; holds the brightness to restore.
    Backlight(f32),
    /// No backlight: the paint path draws the overlay.
    Overlay,
}

/// Owns the dim for one session: remembers what to restore and whether the overlay is needed.
#[derive(Debug)]
pub struct ScreenDimmer<B: Backlight = SysBacklight> {
    backlight: B,
    dim: Dim,
    restore_pending: bool,
}

impl Default for ScreenDimmer<SysBacklight> {
    fn default() -> Self {
        Self::new(SysBacklight)
    }
}

impl<B: Backlight> ScreenDimmer<B> {
    /// A dimmer driving `backlight`, starting at full brightness.
    pub fn new(backlight: B) -> Self {
        Self {
            backlight,
            dim: Dim::Off,
            restore_pending: false,
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
                BACKLIGHT_HELD.store(true, Ordering::Relaxed);
                Dim::Backlight(current)
            }
            _ => Dim::Overlay,
        };
    }

    /// Put the display back to the brightness it had before [`Self::dim`].
    pub fn restore(&mut self) {
        if let Dim::Backlight(saved) = self.dim {
            if !self.backlight.set_brightness(saved) {
                self.restore_pending = true;
                return;
            }
            BACKLIGHT_HELD.store(false, Ordering::Relaxed);
        }
        self.restore_pending = false;
        self.dim = Dim::Off;
    }

    /// Retry one refused restore on actual activity or a wake/unlock edge.
    /// An ordinary dim is left alone until its Undimmed transition; no timer
    /// or frame repeatedly writes the backlight while a driver refuses it.
    pub fn retry_restore(&mut self) {
        if self.restore_pending {
            self.restore();
        }
    }

    /// Opacity of the dimming overlay the paint path must draw on top of the scene, if any.
    pub fn overlay_alpha(&self) -> Option<f32> {
        (self.dim == Dim::Overlay).then_some(OVERLAY_ALPHA)
    }
}

impl<B: Backlight> Drop for ScreenDimmer<B> {
    fn drop(&mut self) {
        // Normal session restart must not make the next compositor inherit a
        // dimmed level as the person's brightness. Hard termination cannot run
        // this cleanup; a refused driver write remains a best-effort restore.
        self.restore();
    }
}

/// Darken `scene` over `rect` with the dimming overlay.
pub fn paint_overlay(scene: &mut vello::Scene, rect: vello::kurbo::Rect, alpha: f32) {
    scene.fill(
        vello::peniko::Fill::NonZero,
        vello::kurbo::Affine::IDENTITY,
        vello::peniko::Color::new([0.0, 0.0, 0.0, alpha]),
        None,
        &rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeBacklight {
        level: RefCell<Option<f32>>,
        writes: RefCell<Vec<f32>>,
    }

    impl FakeBacklight {
        fn at(level: f32) -> Self {
            Self {
                level: RefCell::new(Some(level)),
                writes: RefCell::default(),
            }
        }
    }

    impl Backlight for &FakeBacklight {
        fn brightness(&self) -> Option<f32> {
            *self.level.borrow()
        }
        fn set_brightness(&self, fraction: f32) -> bool {
            if self.level.borrow().is_none() {
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
    fn failed_restore_retains_the_original_level_for_retry() {
        struct RefusingBacklight {
            level: std::cell::Cell<f32>,
            refuse: std::cell::Cell<bool>,
        }
        impl Backlight for &RefusingBacklight {
            fn brightness(&self) -> Option<f32> {
                Some(self.level.get())
            }
            fn set_brightness(&self, fraction: f32) -> bool {
                if self.refuse.get() {
                    return false;
                }
                self.level.set(fraction);
                true
            }
        }
        let backlight = RefusingBacklight {
            level: std::cell::Cell::new(0.8),
            refuse: std::cell::Cell::new(false),
        };
        let mut dimmer = ScreenDimmer::new(&backlight);
        dimmer.dim();
        backlight.refuse.set(true);
        dimmer.restore();
        assert_eq!(dimmer.dim, Dim::Backlight(0.8));
        backlight.refuse.set(false);
        dimmer.retry_restore();
        assert_eq!(backlight.level.get(), 0.8);
        assert_eq!(dimmer.dim, Dim::Off);
    }

    #[test]
    fn dropping_a_dimmed_session_restores_its_level_without_replacing_explicit_zero() {
        for level in [0.8, 0.0] {
            let backlight = FakeBacklight::at(level);
            {
                let mut dimmer = ScreenDimmer::new(&backlight);
                dimmer.dim();
            }
            assert_eq!(*backlight.level.borrow(), Some(level));
        }
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
}
