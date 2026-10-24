//! Content zoom: one mechanism for every app window.
//!
//! Ctrl+Plus, Ctrl+Minus and Ctrl+0 (⌘ on macOS), Ctrl+scroll and a
//! touchpad pinch zoom the content of the window they reach. The window's
//! host resolves the gesture to a [`ZoomStep`] and offers it to the widgets
//! first: a widget whose content has a zoom of its own (a web page's
//! per-site zoom, a PDF page) provides [`ContentZoom`] and answers it.
//! Otherwise the step changes the window's content scale ([`WindowZoom`]):
//! the theme is rebuilt with the factor multiplied into its UI scale, so
//! text, spacing, icons and every layout reflow at the new size, rasterised
//! at the display's resolution, never a bitmap stretch. The person's text
//! size setting stays on top of it (`text_scale` is untouched), and a
//! terminal's grid, set in the theme's code type, grows its font with it.
//!
//! A window's factor is remembered per app on this device, and every change
//! is said in the window's bar for a while ("110%").

use winit::event::{ElementState, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::keyboard::{Key, ModifiersState};

use crate::platform::PlatformConventions;
use crate::time::Instant;

/// The zoom levels every step moves between, as factors of the default.
pub const LEVELS: [f32; 13] = [
    0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0,
];

/// The smallest content zoom.
pub const MIN: f32 = 0.5;
/// The largest content zoom.
pub const MAX: f32 = 3.0;

/// How far a pinch spreads (in the gesture's scale units) for one step.
const PINCH_STEP: f64 = 0.12;

/// How many scrolled pixels of a precise (touchpad) Ctrl+scroll make one
/// step; a wheel notch is one step.
const SCROLL_STEP_PX: f64 = 60.0;

/// What a zoom gesture asks for, and what the window's Zoom in, Zoom out
/// and Actual size actions carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoomStep {
    /// One level larger (Ctrl+Plus, scroll up, pinch out).
    In,
    /// One level smaller (Ctrl+Minus, scroll down, pinch in).
    Out,
    /// Back to the default size (Ctrl+0).
    Reset,
}

impl ZoomStep {
    /// The factor one step from `current`: the next level above or below it,
    /// or `1.0` for [`Self::Reset`].
    #[must_use]
    pub fn apply(self, current: f32) -> f32 {
        let current = if current.is_finite() { current } else { 1.0 };
        match self {
            Self::Reset => 1.0,
            Self::In => LEVELS
                .iter()
                .copied()
                .find(|level| *level > current + 0.001)
                .unwrap_or(MAX),
            Self::Out => LEVELS
                .iter()
                .rev()
                .copied()
                .find(|level| *level < current - 0.001)
                .unwrap_or(MIN),
        }
    }

    /// The step a pressed key means: the platform's command modifier with
    /// `+` (or `=`, its unshifted key on many layouts), `-` or `0`, from the
    /// main row or the keypad. Alt keeps its own meaning.
    #[must_use]
    pub fn from_key(
        conventions: PlatformConventions,
        modifiers: ModifiersState,
        key: &Key,
    ) -> Option<Self> {
        if !conventions.command_modifier_down(modifiers) || modifiers.alt_key() {
            return None;
        }
        let Key::Character(text) = key else {
            return None;
        };
        match text.as_str() {
            "+" | "=" => Some(Self::In),
            "-" | "_" | "\u{2212}" => Some(Self::Out),
            "0" => Some(Self::Reset),
            _ => None,
        }
    }
}

/// A zoom gesture as it arrives: a key, a Ctrl+scroll or a touchpad
/// pinch. Scroll and pinch add up to steps ([`WindowZoom::step_of`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ZoomGesture {
    Step(ZoomStep),
    Scroll(MouseScrollDelta),
    Pinch(f64, TouchPhase),
}

impl ZoomGesture {
    #[must_use]
    pub fn of(
        event: &WindowEvent,
        conventions: PlatformConventions,
        modifiers: ModifiersState,
    ) -> Option<Self> {
        match event {
            WindowEvent::KeyboardInput { event: key, .. } if key.state == ElementState::Pressed => {
                ZoomStep::from_key(conventions, modifiers, &key.logical_key).map(Self::Step)
            }
            WindowEvent::MouseWheel { delta, .. } if is_zoom_modifier(conventions, modifiers) => {
                Some(Self::Scroll(*delta))
            }
            WindowEvent::PinchGesture { delta, phase, .. } => Some(Self::Pinch(*delta, *phase)),
            _ => None,
        }
    }
}

/// Whether `modifiers` make a scroll a zoom: the platform's command
/// modifier, without Alt.
#[must_use]
pub fn is_zoom_modifier(conventions: PlatformConventions, modifiers: ModifiersState) -> bool {
    conventions.command_modifier_down(modifiers) && !modifiers.alt_key()
}

/// Whether `event` is a Ctrl+scroll, which zooms rather than scrolls: a
/// scrolling container leaves it to the window (or to a widget whose
/// content zooms by itself).
#[must_use]
pub fn is_zoom_scroll(event: &WindowEvent, modifiers: &winit::event::Modifiers) -> bool {
    matches!(event, WindowEvent::MouseWheel { .. })
        && is_zoom_modifier(PlatformConventions::default(), modifiers.state())
}

/// A widget whose content zooms by itself: a web page (per site), a
/// document page. The host offers it every zoom gesture before it zooms the
/// window; it answers with the factor it now shows (for the indicator), or
/// `None` to leave the gesture to the window.
pub trait ContentZoom {
    fn zoom_content(&mut self, step: ZoomStep) -> Option<f32>;
}

/// Offer `step` to the widgets under `widget`, depth first: the factor the
/// first one that took it now shows.
pub fn offer(widget: &mut dyn crate::widget::Widget, step: ZoomStep) -> Option<f32> {
    if let Some(zoom) = crate::widget::capability::capability_mut::<dyn ContentZoom>(&mut *widget)
        && let Some(factor) = zoom.zoom_content(step)
    {
        return Some(factor);
    }
    widget
        .children_mut()
        .into_iter()
        .find_map(|child| offer(child, step))
}

pub fn provided(widget: &mut dyn crate::widget::Widget) -> bool {
    crate::widget::capability::capability_mut::<dyn ContentZoom>(&mut *widget).is_some()
        || widget
            .children_mut()
            .into_iter()
            .any(|child| provided(&mut *child))
}

/// What the window remembers per app: its content zoom on this device.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ZoomPrefs {
    /// The window's content zoom; none is the default size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_zoom: Option<f32>,
}

impl crate::storage::SettingsSchema for ZoomPrefs {
    const SCOPE: crate::storage::Scope = crate::storage::Scope::DeviceLocal;
}

/// A window's content zoom: its factor, where it is remembered, the level
/// the window's bar says after a change, and the scroll and pinch it
/// accumulates into steps.
#[derive(Debug)]
pub struct WindowZoom {
    factor: f32,
    settings: Option<crate::storage::Settings<ZoomPrefs>>,
    /// The level, said in the window's bar for a while after it changed.
    level: crate::widget::toast::Transient<String>,
    pinch: f64,
    scroll: f64,
}

impl Default for WindowZoom {
    fn default() -> Self {
        Self {
            factor: 1.0,
            settings: None,
            level: crate::widget::toast::Transient::new(),
            pinch: 0.0,
            scroll: 0.0,
        }
    }
}

impl WindowZoom {
    /// The factor the window's content is scaled by (`1.0`: default).
    #[must_use]
    pub fn factor(&self) -> f32 {
        self.factor
    }

    /// Remember the factor in `app_id`'s settings on this device, and take
    /// up the one stored there. Returns whether the factor changed.
    pub fn restore_for_app(&mut self, app_id: &str) -> bool {
        self.restore_from(crate::storage::Settings::for_app(app_id))
    }

    /// [`Self::restore_for_app`], with the factor kept in `settings`.
    pub(crate) fn restore_from(&mut self, settings: crate::storage::Settings<ZoomPrefs>) -> bool {
        let stored = settings
            .get()
            .content_zoom
            .filter(|factor| factor.is_finite())
            .map_or(1.0, |factor| factor.clamp(MIN, MAX));
        self.settings = Some(settings);
        self.set_factor(stored)
    }

    /// Whether a window a person sees has this zoom: one was opened for an
    /// app ([`Self::restore_for_app`]). A test's or a host worker's app has
    /// no window to zoom.
    #[must_use]
    pub fn in_window(&self) -> bool {
        self.settings.is_some()
    }

    fn set_factor(&mut self, factor: f32) -> bool {
        let factor = factor.clamp(MIN, MAX);
        if (factor - self.factor).abs() <= f32::EPSILON {
            return false;
        }
        self.factor = factor;
        true
    }

    /// Take `step` on the window's own scale: remember it and show the
    /// indicator. Returns whether the factor changed.
    pub fn step(&mut self, step: ZoomStep, now: Instant) -> bool {
        let changed = self.set_factor(step.apply(self.factor));
        if changed && let Some(settings) = &self.settings {
            let stored = ((self.factor - 1.0).abs() > f32::EPSILON).then_some(self.factor);
            if let Err(error) = settings.update(|prefs| prefs.content_zoom = stored) {
                tracing::debug!(%error, "content zoom: could not remember the zoom");
            }
        }
        self.indicate(self.factor, now);
        changed
    }

    /// Say the level `factor` in the window's bar (a widget's own zoom
    /// says its level the same way); the same level again (a step at a
    /// bound) says it once more.
    pub fn indicate(&mut self, factor: f32, _now: Instant) {
        let text = crate::format::percent(f64::from(factor));
        self.level.show(text);
        self.publish();
    }

    /// Whether the window's bar says the level.
    #[must_use]
    pub fn indicator_visible(&self, _now: Instant) -> bool {
        self.level.is_shown()
    }

    /// Keep the window's bar saying the level while it lasts, and let it go
    /// once its time is up; the host calls this on its frames and at
    /// [`Self::deadline`].
    pub fn publish(&mut self) {
        self.level.expire();
        self.level.publish(|level| {
            crate::window_toolbar::ToolbarNotice::new(
                level.clone(),
                crate::widget::toast::ToastKind::Info,
            )
        });
    }

    /// When the level leaves the window's bar, for the host's wake.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.level.deadline()
    }

    /// The step `gesture` amounts to, if any yet.
    pub fn step_of(&mut self, gesture: ZoomGesture) -> Option<ZoomStep> {
        match gesture {
            ZoomGesture::Step(step) => Some(step),
            ZoomGesture::Scroll(delta) => self.scroll_step(&delta),
            ZoomGesture::Pinch(delta, phase) => self.pinch_step(delta, phase),
        }
    }

    /// A Ctrl+scroll: the step it adds up to, if any. A wheel notch is a
    /// step; a touchpad's pixels add up to one.
    pub fn scroll_step(&mut self, delta: &MouseScrollDelta) -> Option<ZoomStep> {
        let y = match delta {
            MouseScrollDelta::LineDelta(_, y) => {
                self.scroll = 0.0;
                return match f64::from(*y) {
                    y if y > 0.0 => Some(ZoomStep::In),
                    y if y < 0.0 => Some(ZoomStep::Out),
                    _ => None,
                };
            }
            MouseScrollDelta::PixelDelta(p) => p.y,
        };
        self.scroll += y;
        Self::take_step(&mut self.scroll, SCROLL_STEP_PX)
    }

    /// A touchpad pinch's change of scale: the step it adds up to, if any.
    pub fn pinch_step(&mut self, delta: f64, phase: TouchPhase) -> Option<ZoomStep> {
        if matches!(phase, TouchPhase::Started) {
            self.pinch = 0.0;
        }
        if delta.is_finite() {
            self.pinch += delta;
        }
        let step = Self::take_step(&mut self.pinch, PINCH_STEP);
        if matches!(phase, TouchPhase::Ended | TouchPhase::Cancelled) {
            self.pinch = 0.0;
        }
        step
    }

    fn take_step(accumulated: &mut f64, per_step: f64) -> Option<ZoomStep> {
        if *accumulated >= per_step {
            *accumulated -= per_step;
            Some(ZoomStep::In)
        } else if *accumulated <= -per_step {
            *accumulated += per_step;
            Some(ZoomStep::Out)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Platform;

    #[test]
    fn steps_move_between_the_levels_and_reset_to_the_default() {
        assert!((ZoomStep::In.apply(1.0) - 1.1).abs() < 1e-6);
        assert!((ZoomStep::Out.apply(1.0) - 0.9).abs() < 1e-6);
        assert!((ZoomStep::In.apply(MAX) - MAX).abs() < 1e-6);
        assert!((ZoomStep::Out.apply(MIN) - MIN).abs() < 1e-6);
        assert!((ZoomStep::Reset.apply(2.5) - 1.0).abs() < 1e-6);
        // A factor between levels steps to the next one, not past it.
        assert!((ZoomStep::In.apply(1.2) - 1.25).abs() < 1e-6);
    }

    #[test]
    fn the_keys_are_the_command_modifier_with_plus_minus_and_zero() {
        let linux = PlatformConventions::new(Platform::Linux);
        let ctrl = ModifiersState::CONTROL;
        let key = |s: &str| Key::Character(s.into());
        assert_eq!(
            ZoomStep::from_key(linux, ctrl, &key("+")),
            Some(ZoomStep::In)
        );
        assert_eq!(
            ZoomStep::from_key(linux, ctrl, &key("=")),
            Some(ZoomStep::In)
        );
        assert_eq!(
            ZoomStep::from_key(linux, ctrl, &key("-")),
            Some(ZoomStep::Out)
        );
        assert_eq!(
            ZoomStep::from_key(linux, ctrl, &key("0")),
            Some(ZoomStep::Reset)
        );
        assert_eq!(
            ZoomStep::from_key(linux, ModifiersState::empty(), &key("+")),
            None
        );
        assert_eq!(
            ZoomStep::from_key(linux, ctrl | ModifiersState::ALT, &key("+")),
            None
        );
        let mac = PlatformConventions::new(Platform::MacOs);
        assert_eq!(
            ZoomStep::from_key(mac, ModifiersState::SUPER, &key("+")),
            Some(ZoomStep::In)
        );
    }

    #[test]
    fn scroll_and_pinch_add_up_to_steps() {
        let mut zoom = WindowZoom::default();
        assert_eq!(
            zoom.scroll_step(&MouseScrollDelta::LineDelta(0.0, 1.0)),
            Some(ZoomStep::In)
        );
        assert_eq!(
            zoom.scroll_step(&MouseScrollDelta::LineDelta(0.0, -1.0)),
            Some(ZoomStep::Out)
        );
        let px = |y| MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, y));
        assert_eq!(zoom.scroll_step(&px(30.0)), None);
        assert_eq!(zoom.scroll_step(&px(30.0)), Some(ZoomStep::In));
        assert_eq!(zoom.pinch_step(0.05, TouchPhase::Started), None);
        assert_eq!(zoom.pinch_step(0.08, TouchPhase::Moved), Some(ZoomStep::In));
        assert_eq!(
            zoom.pinch_step(-0.2, TouchPhase::Moved),
            Some(ZoomStep::Out)
        );
        assert_eq!(zoom.pinch_step(0.0, TouchPhase::Ended), None);
    }

    #[test]
    fn a_step_changes_the_factor_and_shows_the_indicator() {
        let mut zoom = WindowZoom::default();
        let now = crate::time::now();
        assert!(zoom.step(ZoomStep::In, now));
        assert!((zoom.factor() - 1.1).abs() < 1e-6);
        assert!(zoom.indicator_visible(now));
        assert!(zoom.step(ZoomStep::Reset, now));
        assert!((zoom.factor() - 1.0).abs() < 1e-6);
        assert!(!zoom.step(ZoomStep::Reset, now), "already the default");
        assert!(
            zoom.indicator_visible(now),
            "a step at a bound still says the level"
        );
    }
}
