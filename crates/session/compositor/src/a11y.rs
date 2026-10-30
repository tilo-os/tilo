//! What the person chose to make the desktop easier to use, as the
//! compositor applies it: typing and pointing assists, the pointer's size and
//! crosshair, the colour filter, the magnifier and the visual alert.
//!
//! The compositor reads no settings store; the session fills an [`A11y`] from
//! the person's accessibility settings and hands it over
//! ([`crate::Compositor::set_a11y`]). Everything here applies live, and to
//! the outputs and devices that appear later.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "smithay geometry: logical coordinates of windows and outputs, far from i32 limits"
)]

use std::cell::RefCell;
use std::time::{Duration, Instant};

use smithay::output::Output;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale, Size};

use crate::backend::drm::access_keys::Assists;
use crate::color_filter::ColorFilter;
use crate::state::State;

/// The pointer size without the large cursor, in logical pixels.
pub const DEFAULT_CURSOR_SIZE: u32 = 24;
/// The largest pointer the assists offer, in logical pixels.
pub const MAX_CURSOR_SIZE: u32 = 128;
/// The lens's largest size, in output pixels.
pub const LENS_MAX: (i32, i32) = (420, 280);
/// The largest share of an output the lens covers.
pub const LENS_SHARE: f64 = 0.7;
/// How long a visual alert stays on screen as it fades.
pub const FLASH: Duration = Duration::from_millis(400);

/// The step between frames while the alert fades.
pub(crate) const FLASH_STEP: Duration = Duration::from_millis(16);

/// How the magnified image is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MagnifierView {
    /// A lens around what has the focus; the rest of the screen stays as it
    /// is.
    #[default]
    Lens,
    /// The whole screen magnified, moving with what has the focus.
    FullScreen,
}

/// The magnifier as the person set it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Magnifier {
    /// `1` is off.
    pub zoom: u8,
    pub view: MagnifierView,
}

impl Default for Magnifier {
    fn default() -> Self {
        Self {
            zoom: 1,
            view: MagnifierView::Lens,
        }
    }
}

impl Magnifier {
    /// The zoom factor the scene is drawn at.
    pub fn factor(self) -> f64 {
        f64::from(self.zoom.max(1))
    }

    pub fn is_on(self) -> bool {
        self.zoom > 1
    }
}

/// The pointer's size and its crosshair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorA11y {
    /// The pointer's size in logical pixels.
    pub size: u32,
    /// Draw full-height and full-width guides through the pointer, so it is
    /// easy to find on screen.
    pub crosshair: bool,
}

impl Default for CursorA11y {
    fn default() -> Self {
        Self {
            size: DEFAULT_CURSOR_SIZE,
            crosshair: false,
        }
    }
}

/// The person's accessibility settings, as the compositor applies them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct A11y {
    /// Typing and pointing assists, switch scanning included.
    pub assists: Assists,
    pub cursor: CursorA11y,
    pub color_filter: ColorFilter,
    pub magnifier: Magnifier,
    /// Flash the screen when an alert arrives.
    pub visual_alerts: bool,
}

/// What the magnifier looks at, in output-local logical coordinates: the
/// pointer and the focused window, each with when it last moved. Whichever
/// moved last leads, so tabbing through a form without touching the mouse
/// shows the control reached, and moving the pointer hands the view back.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Focus {
    pointer: Option<(Point<f64, Logical>, Instant)>,
    window: Option<(Point<f64, Logical>, Instant)>,
}

impl Focus {
    /// Note where the pointer and the focused window are, `None` when they
    /// are elsewhere. Only a change counts as movement.
    pub(crate) fn observe(
        &mut self,
        pointer: Option<Point<f64, Logical>>,
        window: Option<Point<f64, Logical>>,
        now: Instant,
    ) {
        Self::note(&mut self.pointer, pointer, now);
        Self::note(&mut self.window, window, now);
    }

    fn note(
        seen: &mut Option<(Point<f64, Logical>, Instant)>,
        at: Option<Point<f64, Logical>>,
        now: Instant,
    ) {
        match at {
            Some(at) => {
                if seen.is_none_or(|(was, _)| was != at) {
                    *seen = Some((at, now));
                }
            }
            None => *seen = None,
        }
    }

    /// The point to magnify.
    pub(crate) fn centre(&self) -> Option<Point<f64, Logical>> {
        match (self.pointer, self.window) {
            (Some((_, pointer_at)), Some((window, window_at))) if window_at > pointer_at => {
                Some(window)
            }
            (Some((pointer, _)), _) => Some(pointer),
            (None, Some((window, _))) => Some(window),
            (None, None) => None,
        }
    }
}

/// What the assists make an output show, beyond the scene itself.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Visual {
    pub(crate) color_filter: ColorFilter,
    pub(crate) magnifier_view: MagnifierView,
    pub(crate) crosshair: bool,
    pub(crate) focus: Focus,
    /// When the visual alert started, and how long it takes to fade.
    pub(crate) flash: Option<(Instant, Duration)>,
}

impl Visual {
    /// How strong the visual alert is at `now`: `0.0` when it is not
    /// running.
    pub(crate) fn flash_alpha(&self, now: Instant) -> f32 {
        let Some((started, length)) = self.flash else {
            return 0.0;
        };
        if length.is_zero() {
            return 0.0;
        }
        let elapsed = now.saturating_duration_since(started);
        if elapsed >= length {
            return 0.0;
        }
        // Half the run at full strength, then a fade to nothing.
        let left = 1.0 - elapsed.as_secs_f32() / length.as_secs_f32();
        (left * 2.0).min(1.0)
    }

    /// Whether the alert is still on screen at `now`, or was a step ago: the
    /// frame that draws it gone is still due.
    pub(crate) fn flashing(&self, now: Instant) -> bool {
        self.flash.is_some_and(|(started, length)| {
            now.saturating_duration_since(started) < length.saturating_add(FLASH_STEP)
        })
    }
}

/// An output's [`Visual`], in its user data.
struct VisualCell(RefCell<Visual>);

/// What `output` shows.
pub(crate) fn visual(output: &Output) -> Visual {
    output
        .user_data()
        .get::<VisualCell>()
        .map_or_else(Visual::default, |cell| *cell.0.borrow())
}

/// Change what `output` shows.
pub(crate) fn change(output: &Output, change: impl FnOnce(&mut Visual)) {
    output
        .user_data()
        .insert_if_missing(|| VisualCell(RefCell::new(Visual::default())));
    if let Some(cell) = output.user_data().get::<VisualCell>() {
        change(&mut cell.0.borrow_mut());
    }
}

/// Put `a11y` on `output`: the filter, the magnifier's view and the
/// crosshair, for an output found now or later.
pub(crate) fn seed(output: &Output, a11y: &A11y) {
    change(output, |visual| {
        visual.color_filter = a11y.color_filter;
        visual.magnifier_view = a11y.magnifier.view;
        visual.crosshair = a11y.cursor.crosshair;
    });
}

/// Where the magnifier looks on `output` at `now`, in its physical pixels:
/// the pointer or the focused window, whichever moved last. `None` when
/// neither is on this output.
pub(crate) fn follow(
    output: &Output,
    state: &State,
    geometry: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    now: Instant,
) -> Option<Point<i32, Physical>> {
    let pointer = geometry
        .to_f64()
        .contains(state.pointer)
        .then(|| state.pointer - geometry.loc.to_f64());
    let window = focused_rect(state)
        .filter(|rect| !rect.intersection(geometry).unwrap_or_default().is_empty())
        .map(|rect| centre_of(rect) - geometry.loc.to_f64());
    change(output, |visual| visual.focus.observe(pointer, window, now));
    let centre = visual(output).focus.centre()?;
    Some(centre.to_physical_precise_round(scale))
}

/// The middle of `rect`, as fractions of a logical pixel.
fn centre_of(rect: Rectangle<i32, Logical>) -> Point<f64, Logical> {
    Point::from((
        f64::from(rect.loc.x) + f64::from(rect.size.w) / 2.0,
        f64::from(rect.loc.y) + f64::from(rect.size.h) / 2.0,
    ))
}

/// The rect of the window whose surface has the keyboard focus, logical.
fn focused_rect(state: &State) -> Option<Rectangle<i32, Logical>> {
    let focus = state.seat.get_keyboard()?.current_focus()?;
    let window = state
        .space
        .elements()
        .find(|window| window.toplevel().is_some_and(|t| t.wl_surface() == &focus))?;
    let location = state.space.element_location(window)?;
    Some(Rectangle::new(location, window.geometry().size))
}

/// Flash every output at `now`: the visual alert for what a person who
/// cannot hear an alert sound would otherwise miss.
pub(crate) fn flash_all(state: &State, now: Instant) {
    for output in state.space.outputs() {
        change(output, |visual| visual.flash = Some((now, FLASH)));
    }
}

/// Whether any output is showing the visual alert at `now`.
pub(crate) fn flashing(state: &State, now: Instant) -> bool {
    state
        .space
        .outputs()
        .any(|output| visual(output).flashing(now))
}

/// The lens's rectangle around `centre` in an output of `viewport` size.
pub(crate) fn lens_rect(
    centre: Point<i32, Physical>,
    viewport: Size<i32, Physical>,
) -> Rectangle<i32, Physical> {
    let width = f64::from(LENS_MAX.0).min(f64::from(viewport.w) * LENS_SHARE);
    let height = f64::from(LENS_MAX.1).min(f64::from(viewport.h) * LENS_SHARE);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a share of an output's size, far inside i32"
    )]
    let size = Size::from((width as i32, height as i32));
    Rectangle::new(centre - Point::from((size.w / 2, size.h / 2)), size)
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "test fixture: instants offset by whole milliseconds from one start"
)]
mod tests {
    use smithay::output::{PhysicalProperties, Subpixel};

    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn point(x: f64, y: f64) -> Point<f64, Logical> {
        Point::from((x, y))
    }

    #[test]
    fn everything_is_off_until_the_person_turns_it_on() {
        let a11y = A11y::default();
        assert_eq!(a11y.cursor.size, DEFAULT_CURSOR_SIZE);
        assert!(!a11y.cursor.crosshair);
        assert!(a11y.color_filter.is_off());
        assert!(!a11y.magnifier.is_on());
        assert!(!a11y.visual_alerts);
        assert!(a11y.assists.switch.is_none());
        assert!(!a11y.assists.sticky_keys);
        assert!(!a11y.assists.mouse_keys);
    }

    #[test]
    fn the_magnifier_draws_at_its_step() {
        assert!((Magnifier::default().factor() - 1.0).abs() < f64::EPSILON);
        let magnifier = Magnifier {
            zoom: 3,
            view: MagnifierView::FullScreen,
        };
        assert!(magnifier.is_on());
        assert!((magnifier.factor() - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "the alert is exactly full or exactly gone at its ends"
    )]
    fn a_visual_alert_opens_at_full_strength_and_fades_out() {
        let t0 = Instant::now();
        let mut visual = Visual::default();
        assert_eq!(visual.flash_alpha(t0), 0.0);
        assert!(!visual.flashing(t0));
        visual.flash = Some((t0, FLASH));
        assert_eq!(visual.flash_alpha(t0), 1.0);
        // Full strength for the first half, then down to nothing.
        assert_eq!(visual.flash_alpha(t0 + FLASH / 2), 1.0);
        assert!(visual.flash_alpha(t0 + FLASH * 3 / 4) < 0.6);
        assert_eq!(visual.flash_alpha(t0 + FLASH), 0.0);
        assert!(!visual.flashing(t0 + FLASH + FLASH));
        assert!(visual.flashing(t0 + ms(100)));
    }

    #[test]
    fn an_output_carries_what_the_scene_and_the_final_pass_read() {
        let output = Output::new(
            "HEADLESS-1".to_owned(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Tilo".into(),
                model: "test".into(),
            },
        );
        seed(&output, &A11y::default());
        let plain = visual(&output);
        assert_eq!(plain.color_filter, ColorFilter::Off);
        assert!(!plain.crosshair);

        let a11y = A11y {
            color_filter: ColorFilter::GrayscaleInverted,
            magnifier: Magnifier {
                zoom: 2,
                view: MagnifierView::FullScreen,
            },
            cursor: CursorA11y {
                crosshair: true,
                ..CursorA11y::default()
            },
            ..A11y::default()
        };
        seed(&output, &a11y);
        assert_eq!(
            visual(&output).color_filter,
            ColorFilter::GrayscaleInverted,
            "the filter the output's final pass draws"
        );
        assert_eq!(visual(&output).magnifier_view, MagnifierView::FullScreen);
        assert!(visual(&output).crosshair);

        // And off again.
        seed(&output, &A11y::default());
        assert_eq!(visual(&output).color_filter, ColorFilter::Off);
        assert!(!visual(&output).crosshair);
    }

    #[test]
    fn the_magnifier_follows_the_window_until_the_pointer_moves() {
        let t0 = Instant::now();
        let mut focus = Focus::default();
        assert_eq!(focus.centre(), None);
        focus.observe(Some(point(100.0, 100.0)), None, t0);
        assert_eq!(focus.centre(), Some(point(100.0, 100.0)));
        // Tab moves the focus while the pointer rests.
        focus.observe(
            Some(point(100.0, 100.0)),
            Some(point(450.0, 320.0)),
            t0 + ms(10),
        );
        assert_eq!(focus.centre(), Some(point(450.0, 320.0)));
        // The pointer moving hands the view back.
        focus.observe(
            Some(point(120.0, 90.0)),
            Some(point(450.0, 320.0)),
            t0 + ms(20),
        );
        assert_eq!(focus.centre(), Some(point(120.0, 90.0)));
        // A pointer resting where it was is not movement.
        focus.observe(
            Some(point(120.0, 90.0)),
            Some(point(600.0, 320.0)),
            t0 + ms(30),
        );
        assert_eq!(focus.centre(), Some(point(600.0, 320.0)));
    }

    #[test]
    fn the_lens_is_centred_and_never_covers_the_whole_output() {
        let centre = Point::<i32, Physical>::from((960, 540));
        let lens = lens_rect(centre, Size::from((1920, 1080)));
        assert_eq!(lens.size, Size::from((LENS_MAX.0, LENS_MAX.1)));
        assert_eq!(
            lens.loc,
            Point::from((960 - LENS_MAX.0 / 2, 540 - LENS_MAX.1 / 2))
        );
        // A small output gets a share of it, not a lens larger than it is.
        let small = lens_rect(centre, Size::from((320, 240)));
        assert!(small.size.w < 320 && small.size.h < 240);
    }
}
