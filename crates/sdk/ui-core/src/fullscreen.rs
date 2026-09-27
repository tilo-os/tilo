//! Shared fullscreen / presentation mechanism.
//!
//! A root widget expresses a [`FullscreenRequest`] via
//! `crate::widget::Widget::fullscreen_request`. The App
//! ([`crate::app::App`]) polls it each frame, runs [`select_output`] over
//! the live monitors, and drives `winit::window::Window::set_fullscreen` —
//! a genuine borderless full-output surface, not a maximized window.
//! [`select_output`] is pure over [`MonitorInfo`] so it's unit-tested
//! without a live window.
//!
//! A headless session or a compositor which cannot expose its output globals
//! yields an empty monitor list; that degrades to `Borderless(None)`, placed by
//! the compositor. Multi-output presentation is activated only after a distinct
//! monitor is enumerated.
//!
//! The ranking behind [`FullscreenTarget::Largest`] is what a presenter
//! expects from a laptop plugged into a television: the slides go to the
//! external screen, the physically biggest one if there are several, and
//! the presenter view stays on the built-in panel ([`select_presenter_output`]).
//! The compositor runs the same functions, so a client that could not name
//! an output still lands where the app would have put it.

/// Which output an app wants its fullscreen surface promoted onto.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FullscreenTarget {
    /// Let the compositor choose (fill whatever output it decides).
    Auto,
    /// The audience-facing screen — the presentation default, so "Present" /
    /// immersive video lands on the big screen: an external display before
    /// the built-in panel, then the physically largest, then the most pixels.
    #[default]
    Largest,
    /// The primary monitor (falls back to the first if none is flagged primary).
    Primary,
    /// The primary *external* display: the flagged primary when it is not the
    /// built-in panel, else the first external display in enumeration order.
    /// With no external display it ranks like [`Self::Largest`].
    PrimaryExternal,
    /// A specific monitor by its index in the enumerated list.
    Output(u32),
}

/// A monitor as the App enumerated it from winit, reduced to the fields the
/// output-selection needs. Pure data so [`select_output`] is unit-testable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonitorInfo {
    /// Position of this monitor in the enumerated list.
    pub index: u32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    pub scale: f64,
    /// The laptop's own panel rather than a plugged-in display — see
    /// [`connector_is_builtin`].
    pub builtin: bool,
    /// Physical extent in millimetres, when the display reports one.
    pub physical_mm: Option<(u32, u32)>,
}

impl MonitorInfo {
    /// Pixel area, the last tie-break when ranking for [`FullscreenTarget::Largest`].
    pub fn area(&self) -> u64 {
        u64::from(self.width).saturating_mul(u64::from(self.height))
    }

    /// Physical area in square millimetres, when known.
    pub fn physical_area(&self) -> Option<u64> {
        self.physical_mm
            .map(|(width, height)| u64::from(width).saturating_mul(u64::from(height)))
    }
}

/// Whether a KMS connector name names a built-in panel: embedded
/// DisplayPort, LVDS and MIPI-DSI are how laptops, tablets and phones wire
/// their own screen. Anything else (HDMI, DP, USB-C alt-mode, virtual) is a
/// display someone plugged in. A collision-qualified name such as
/// `card1/eDP-1` is judged by its connector part.
pub fn connector_is_builtin(name: &str) -> bool {
    let connector = name.rsplit('/').next().unwrap_or(name);
    ["eDP", "LVDS", "DSI"]
        .iter()
        .any(|prefix| connector.starts_with(prefix))
}

/// Order monitors by how well they suit an audience: external before
/// built-in, then physically larger (compared only when both sizes are
/// known, since a projector reports none), then more pixels, then primary.
/// Callers add their own final tie-breaks so identical displays still
/// choose deterministically.
fn audience_rank(a: &MonitorInfo, b: &MonitorInfo) -> std::cmp::Ordering {
    let physical = match (a.physical_area(), b.physical_area()) {
        (Some(a), Some(b)) => a.cmp(&b),
        _ => std::cmp::Ordering::Equal,
    };
    b.builtin
        .cmp(&a.builtin)
        .then(physical)
        .then(a.area().cmp(&b.area()))
        .then(a.primary.cmp(&b.primary))
}

fn best_for_audience<'a>(monitors: impl Iterator<Item = &'a MonitorInfo>) -> Option<u32> {
    monitors
        .max_by(|a, b| audience_rank(a, b).then(b.index.cmp(&a.index)))
        .map(|m| m.index)
}

fn primary_external(monitors: &[MonitorInfo]) -> Option<u32> {
    monitors
        .iter()
        .find(|m| m.primary && !m.builtin)
        .or_else(|| monitors.iter().find(|m| !m.builtin))
        .map(|m| m.index)
        .or_else(|| best_for_audience(monitors.iter()))
}

/// What the fullscreen surface should show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FullscreenView {
    /// The app's own window becomes the full-output surface (immersive video).
    Mirror,
    /// A *distinct* view — the presentation case, where the fullscreen surface
    /// shows something other than the app's windowed (editor) view.
    Distinct,
}

/// A fullscreen request a root widget expresses. Level-triggered: the App
/// compares it against the live state each frame, so returning a steady request
/// for the whole duration of a slideshow / immersive playback is correct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FullscreenRequest {
    pub target: FullscreenTarget,
    pub view: FullscreenView,
}

/// A second, audience-only surface requested by a root widget.
///
/// Unlike [`FullscreenRequest`], this never repurposes the application's
/// existing window. The host creates a separate borderless toplevel only when
/// it can bind it to a monitor other than the window the presenter is using.
/// This makes the API safe on one-output and headless sessions: callers get
/// the ordinary fullscreen fallback instead of two panes in one surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudiencePresentationRequest {
    pub target: FullscreenTarget,
}

impl AudiencePresentationRequest {
    pub const fn new(target: FullscreenTarget) -> Self {
        Self { target }
    }
}

impl FullscreenRequest {
    /// A request to fullscreen the app's own window on `target` (immersive).
    pub fn mirror(target: FullscreenTarget) -> Self {
        Self {
            target,
            view: FullscreenView::Mirror,
        }
    }

    /// A request to present a distinct view on `target` (presentation).
    pub fn distinct(target: FullscreenTarget) -> Self {
        Self {
            target,
            view: FullscreenView::Distinct,
        }
    }

    /// Whether this request wants a distinct presentation view.
    pub fn is_distinct(&self) -> bool {
        matches!(self.view, FullscreenView::Distinct)
    }
}

/// Pure output selection: given the enumerated monitors and a target, return the
/// **index** of the chosen monitor, or `None` to defer output choice to the
/// compositor (`Auto`, an empty monitor list, or an out-of-range `Output`).
pub fn select_output(monitors: &[MonitorInfo], target: FullscreenTarget) -> Option<u32> {
    if monitors.is_empty() {
        return None;
    }
    match target {
        FullscreenTarget::Auto => None,
        FullscreenTarget::Largest => best_for_audience(monitors.iter()),
        FullscreenTarget::Primary => monitors
            .iter()
            .find(|m| m.primary)
            .or_else(|| monitors.first())
            .map(|m| m.index),
        FullscreenTarget::PrimaryExternal => primary_external(monitors),
        FullscreenTarget::Output(i) => monitors.iter().find(|m| m.index == i).map(|m| m.index),
    }
}

/// Pick the audience display for a two-screen presentation. `None` means
/// there is no second output, so the caller keeps its single-window
/// fallback.
///
/// The presenter's current output does not disqualify a display: a
/// compositor that pairs the audience surface moves the presenter view to
/// [`select_presenter_output`], the way a laptop plugged into a television
/// is expected to behave. It only breaks ties, so of two identical screens
/// the one the presenter is not using gets the slides.
pub fn select_audience_output(
    monitors: &[MonitorInfo],
    target: FullscreenTarget,
    presenter_output: Option<u32>,
) -> Option<u32> {
    if monitors.len() < 2 {
        return None;
    }
    let broad = || {
        monitors.iter().max_by(|a, b| {
            audience_rank(a, b)
                .then_with(|| {
                    (Some(b.index) == presenter_output).cmp(&(Some(a.index) == presenter_output))
                })
                .then(b.index.cmp(&a.index))
        })
    };
    match target {
        FullscreenTarget::Auto | FullscreenTarget::Largest => broad().map(|monitor| monitor.index),
        FullscreenTarget::Primary => monitors
            .iter()
            .find(|monitor| monitor.primary)
            .or_else(broad)
            .map(|monitor| monitor.index),
        FullscreenTarget::PrimaryExternal => primary_external(monitors),
        FullscreenTarget::Output(index) => monitors
            .iter()
            .find(|monitor| monitor.index == index)
            .map(|monitor| monitor.index),
    }
}

/// Where the presenter view goes once the audience owns `audience`: the
/// built-in panel if there is one, else the smallest remaining display
/// (the laptop-sized screen beside a projector), else the earliest index.
/// `None` when no other output exists — one screen keeps its ordinary
/// layout.
pub fn select_presenter_output(monitors: &[MonitorInfo], audience: u32) -> Option<u32> {
    monitors
        .iter()
        .filter(|monitor| monitor.index != audience)
        .min_by(|a, b| {
            b.builtin
                .cmp(&a.builtin)
                .then_with(|| match (a.physical_area(), b.physical_area()) {
                    (Some(a), Some(b)) => a.cmp(&b),
                    _ => std::cmp::Ordering::Equal,
                })
                .then(a.area().cmp(&b.area()))
                .then(a.index.cmp(&b.index))
        })
        .map(|monitor| monitor.index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(index: u32, w: u32, h: u32, primary: bool) -> MonitorInfo {
        MonitorInfo {
            index,
            width: w,
            height: h,
            primary,
            scale: 1.0,
            builtin: false,
            physical_mm: None,
        }
    }

    /// A 14" laptop panel: dense, small, built in.
    fn laptop(index: u32) -> MonitorInfo {
        MonitorInfo {
            builtin: true,
            physical_mm: Some((302, 189)),
            ..mon(index, 2880, 1800, false)
        }
    }

    /// A 65" television at 1080p — fewer pixels than the laptop.
    fn television(index: u32) -> MonitorInfo {
        MonitorInfo {
            physical_mm: Some((1430, 800)),
            ..mon(index, 1920, 1080, false)
        }
    }

    /// A 27" desk monitor at 4K — more pixels than the television.
    fn desk_monitor(index: u32) -> MonitorInfo {
        MonitorInfo {
            physical_mm: Some((597, 336)),
            ..mon(index, 3840, 2160, false)
        }
    }

    #[test]
    fn largest_picks_biggest_area() {
        let mons = vec![
            mon(0, 1920, 1080, true),
            mon(1, 3840, 2160, false),
            mon(2, 1280, 720, false),
        ];
        assert_eq!(
            select_output(&mons, FullscreenTarget::Largest),
            Some(1),
            "Present targets the biggest monitor, not the primary laptop panel"
        );
    }

    #[test]
    fn empty_list_defers_to_compositor() {
        assert_eq!(select_output(&[], FullscreenTarget::Largest), None);
        assert_eq!(select_output(&[], FullscreenTarget::Primary), None);
    }

    #[test]
    fn auto_always_defers() {
        let mons = vec![mon(0, 1920, 1080, true)];
        assert_eq!(select_output(&mons, FullscreenTarget::Auto), None);
    }

    #[test]
    fn primary_picks_flagged_else_first() {
        let mons = vec![mon(0, 1280, 720, false), mon(1, 1920, 1080, true)];
        assert_eq!(select_output(&mons, FullscreenTarget::Primary), Some(1));
        let none_primary = vec![mon(0, 1280, 720, false), mon(1, 1920, 1080, false)];
        assert_eq!(
            select_output(&none_primary, FullscreenTarget::Primary),
            Some(0)
        );
    }

    #[test]
    fn output_index_selects_or_none_when_out_of_range() {
        let mons = vec![mon(0, 1280, 720, false), mon(1, 1920, 1080, false)];
        assert_eq!(select_output(&mons, FullscreenTarget::Output(1)), Some(1));
        assert_eq!(select_output(&mons, FullscreenTarget::Output(9)), None);
    }

    #[test]
    fn largest_ties_break_toward_primary() {
        // Two identical-area monitors: the primary wins the tie deterministically.
        let mons = vec![mon(0, 1920, 1080, false), mon(1, 1920, 1080, true)];
        assert_eq!(select_output(&mons, FullscreenTarget::Largest), Some(1));
    }

    #[test]
    fn request_constructors_carry_view() {
        assert!(!FullscreenRequest::mirror(FullscreenTarget::Largest).is_distinct());
        assert!(FullscreenRequest::distinct(FullscreenTarget::Largest).is_distinct());
    }

    #[test]
    fn builtin_connectors_are_the_panels_a_device_ships_with() {
        for builtin in ["eDP-1", "LVDS-1", "DSI-1", "card1/eDP-2"] {
            assert!(connector_is_builtin(builtin), "{builtin}");
        }
        for external in ["HDMI-A-1", "DP-3", "Tilo-0", "card0/DP-1", "Virtual-1"] {
            assert!(!connector_is_builtin(external), "{external}");
        }
    }

    #[test]
    fn laptop_and_television_put_slides_on_the_television() {
        // The television has fewer pixels than the laptop's HiDPI panel;
        // pixel count alone would keep the slides on the laptop.
        let mons = [laptop(0), television(1)];
        for presenter in [None, Some(0), Some(1)] {
            assert_eq!(
                select_audience_output(&mons, FullscreenTarget::Largest, presenter),
                Some(1),
                "slides go to the external screen wherever the presenter window is ({presenter:?})"
            );
        }
        assert_eq!(select_presenter_output(&mons, 1), Some(0));
        assert_eq!(select_output(&mons, FullscreenTarget::Largest), Some(1));
    }

    #[test]
    fn two_externals_put_slides_on_the_physically_larger_one() {
        let mons = [desk_monitor(0), television(1)];
        assert_eq!(
            select_audience_output(&mons, FullscreenTarget::Largest, Some(1)),
            Some(1),
            "a 65-inch 1080p television beats a 27-inch 4K monitor for an audience"
        );
        assert_eq!(select_presenter_output(&mons, 1), Some(0));
    }

    #[test]
    fn unknown_physical_sizes_fall_back_to_pixels_then_the_free_screen() {
        let projector = mon(1, 1920, 1080, false);
        let mons = [desk_monitor(0), projector];
        assert_eq!(
            select_audience_output(&mons, FullscreenTarget::Largest, None),
            Some(0),
            "a projector reports no size, so pixel area decides"
        );
        let twins = [mon(0, 1920, 1080, false), mon(1, 1920, 1080, false)];
        assert_eq!(
            select_audience_output(&twins, FullscreenTarget::Largest, Some(0)),
            Some(1),
            "of two identical screens the presenter keeps the one it is on"
        );
    }

    #[test]
    fn laptop_with_desk_monitor_and_television_keeps_presenter_on_the_laptop() {
        let mons = [laptop(0), desk_monitor(1), television(2)];
        assert_eq!(
            select_audience_output(&mons, FullscreenTarget::Largest, Some(1)),
            Some(2)
        );
        assert_eq!(select_presenter_output(&mons, 2), Some(0));
    }

    #[test]
    fn a_single_screen_has_no_audience_output() {
        for mons in [vec![laptop(0)], vec![television(0)], Vec::new()] {
            assert_eq!(
                select_audience_output(&mons, FullscreenTarget::Largest, None),
                None
            );
            assert_eq!(select_presenter_output(&mons, 0), None);
        }
    }

    #[test]
    fn primary_external_skips_the_builtin_panel() {
        let flagged_laptop = MonitorInfo {
            primary: true,
            ..laptop(0)
        };
        let mons = [flagged_laptop, desk_monitor(1), television(2)];
        assert_eq!(
            select_audience_output(&mons, FullscreenTarget::PrimaryExternal, None),
            Some(1),
            "the first external display, not the primary-flagged laptop panel"
        );
        let flagged_tv = MonitorInfo {
            primary: true,
            ..television(2)
        };
        let mons = [laptop(0), desk_monitor(1), flagged_tv];
        assert_eq!(
            select_audience_output(&mons, FullscreenTarget::PrimaryExternal, None),
            Some(2)
        );
        assert_eq!(
            select_output(&mons, FullscreenTarget::PrimaryExternal),
            Some(2)
        );
    }

    #[test]
    fn explicit_audience_output_is_honoured_even_under_the_presenter() {
        let mons = [laptop(0), television(1)];
        assert_eq!(
            select_audience_output(&mons, FullscreenTarget::Output(0), Some(0)),
            Some(0),
            "the compositor moves the presenter view off an explicitly chosen screen"
        );
        assert_eq!(select_presenter_output(&mons, 0), Some(1));
        assert_eq!(
            select_audience_output(&mons, FullscreenTarget::Output(9), None),
            None
        );
    }
}
