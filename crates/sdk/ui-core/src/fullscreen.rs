//! Shared fullscreen / presentation mechanism. A root widget expresses a [`FullscreenRequest`] via
//! `crate::widget::Widget::fullscreen_request`.

/// Which output an app wants its fullscreen surface promoted onto.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FullscreenTarget {
    /// Let the compositor choose (fill whatever output it decides).
    Auto,
    /// The audience-facing screen — the presentation default, so "Present" /
    /// immersive video plays on the big screen: an external display before
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

/// A monitor as the App enumerated it from winit, reduced to the fields the output-selection needs.
/// Pure data so [`select_output`] is unit-testable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonitorInfo {
    /// Position of this monitor in the enumerated list.
    pub index: u32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    pub scale: f64,
    /// The laptop panel rather than a plugged-in display — see [`connector_is_builtin`].
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
    /// The app window becomes the full-output surface (immersive video).
    Mirror,
    /// A *distinct* view — the presentation case, where the fullscreen surface shows something other
    /// than the app's windowed (editor) view.
    Distinct,
}

/// A fullscreen request a root widget expresses.
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
    /// A request to fullscreen the app window on `target` (immersive).
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

/// Pure output selection: given the enumerated monitors and a target, return the **index** of the
/// chosen monitor, or `None` to defer output choice to the compositor (`Auto`, an empty monitor list,
/// or an out-of-range `Output`).
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

    tilo_test_env::scenario! {
        /// Which output a fullscreen request lands on: by target, for an audience and for the
        /// presenter.
        /// Each phase was a test of its own and builds its own fixture.
        #[allow(clippy::too_many_lines, reason = "one row per case")]
        fn an_output_is_chosen_for_each_target_and_set_of_screens;

        phase select_output_resolves_each_target_or_defers_to_the_compositor;
        // `select_output` for every target: the index it picks, or `None` to defer to the compositor.
        {
            use FullscreenTarget::{Auto, Largest, Output, Primary, PrimaryExternal};
            let three = vec![
                mon(0, 1920, 1080, true),
                mon(1, 3840, 2160, false),
                mon(2, 1280, 720, false),
            ];
            let single = vec![mon(0, 1920, 1080, true)];
            let flagged_second = vec![mon(0, 1280, 720, false), mon(1, 1920, 1080, true)];
            let none_primary = vec![mon(0, 1280, 720, false), mon(1, 1920, 1080, false)];
            let twins_second_primary = vec![mon(0, 1920, 1080, false), mon(1, 1920, 1080, true)];
            let twins = vec![mon(0, 1920, 1080, false), mon(1, 1920, 1080, false)];
            let flagged_tv = MonitorInfo {
                primary: true,
                ..television(2)
            };
            let cases = [
                (
                    "Present targets the biggest monitor, not the primary laptop panel",
                    three,
                    Largest,
                    Some(1),
                ),
                ("an empty list defers: largest", vec![], Largest, None),
                ("an empty list defers: primary", vec![], Primary, None),
                ("an empty list defers: auto", vec![], Auto, None),
                (
                    "an empty list defers: primary external",
                    vec![],
                    PrimaryExternal,
                    None,
                ),
                ("an empty list defers: output 0", vec![], Output(0), None),
                ("auto always defers", single.clone(), Auto, None),
                ("a single monitor is the largest", single, Largest, Some(0)),
                (
                    "primary picks the flagged monitor",
                    flagged_second,
                    Primary,
                    Some(1),
                ),
                (
                    "primary without a flag is the first monitor",
                    none_primary.clone(),
                    Primary,
                    Some(0),
                ),
                (
                    "an output index selects that output",
                    none_primary.clone(),
                    Output(1),
                    Some(1),
                ),
                (
                    "an output index out of range defers",
                    none_primary,
                    Output(9),
                    None,
                ),
                (
                    "largest ties break toward primary",
                    twins_second_primary,
                    Largest,
                    Some(1),
                ),
                (
                    "largest ties with no primary break toward the lowest index",
                    twins,
                    Largest,
                    Some(0),
                ),
                (
                    "of a laptop and a television the television is the largest",
                    vec![laptop(0), television(1)],
                    Largest,
                    Some(1),
                ),
                (
                    "primary external is the primary-flagged television",
                    vec![laptop(0), desk_monitor(1), flagged_tv],
                    PrimaryExternal,
                    Some(2),
                ),
                (
                    "a lone built-in panel is the only primary external there is",
                    vec![laptop(0)],
                    PrimaryExternal,
                    Some(0),
                ),
            ];
            for (case, monitors, target, want) in cases {
                assert_eq!(select_output(&monitors, target), want, "case: {case}");
            }
        }

        phase builtin_connectors_are_the_panels_a_device_ships_with;
        {
            for builtin in [
                "eDP-1",
                "LVDS-1",
                "DSI-1",
                "card1/eDP-2",
                // Only the connector part is judged, however deep the qualifier.
                "card0/card1/LVDS-2",
                "HDMI-A-1/eDP-1",
                // The bare prefix, with no number.
                "DSI",
            ] {
                assert!(connector_is_builtin(builtin), "{builtin}");
            }
            for external in [
                "HDMI-A-1",
                "DP-3",
                "Tilo-0",
                "card0/DP-1",
                "Virtual-1",
                // No name, and a name that ends in its separator, name no panel.
                "",
                "eDP-1/",
                // A panel's name starts the connector part; it does not merely appear in the name.
                "HDMI-eDP-1",
                "DP-eDP",
                "eDP-1/HDMI-A-1",
            ] {
                assert!(!connector_is_builtin(external), "{external:?}");
            }
        }

        phase the_audience_output_is_chosen_for_each_set_of_screens_and_target;
        // `select_audience_output`: where the slides go, for each set of screens, target and screen
        // the presenter window is on.
        {
            use FullscreenTarget::{Auto, Largest, Output, Primary, PrimaryExternal};
            let laptop_tv = vec![laptop(0), television(1)];
            let twins = vec![mon(0, 1920, 1080, false), mon(1, 1920, 1080, false)];
            let flagged_laptop = MonitorInfo {
                primary: true,
                ..laptop(0)
            };
            let flagged_tv = MonitorInfo {
                primary: true,
                ..television(2)
            };
            let cases = [
                (
                    "slides go to the external screen with no presenter window",
                    laptop_tv.clone(),
                    Largest,
                    None,
                    Some(1),
                ),
                (
                    "slides go to the external screen with the presenter on the laptop",
                    laptop_tv.clone(),
                    Largest,
                    Some(0),
                    Some(1),
                ),
                (
                    "slides go to the external screen with the presenter on the television",
                    laptop_tv.clone(),
                    Largest,
                    Some(1),
                    Some(1),
                ),
                (
                    "a 65-inch 1080p television beats a 27-inch 4K monitor for an audience",
                    vec![desk_monitor(0), television(1)],
                    Largest,
                    Some(1),
                    Some(1),
                ),
                (
                    "a projector reports no size, so pixel area decides",
                    vec![desk_monitor(0), mon(1, 1920, 1080, false)],
                    Largest,
                    None,
                    Some(0),
                ),
                (
                    "of two identical screens the presenter keeps the one it is on (0)",
                    twins.clone(),
                    Largest,
                    Some(0),
                    Some(1),
                ),
                (
                    "of two identical screens the presenter keeps the one it is on (1)",
                    twins.clone(),
                    Largest,
                    Some(1),
                    Some(0),
                ),
                (
                    "two identical screens and no presenter window: the lowest index",
                    twins,
                    Largest,
                    None,
                    Some(0),
                ),
                (
                    "laptop, desk monitor and television: the television",
                    vec![laptop(0), desk_monitor(1), television(2)],
                    Largest,
                    Some(1),
                    Some(2),
                ),
                (
                    "a single laptop has no audience output",
                    vec![laptop(0)],
                    Largest,
                    None,
                    None,
                ),
                (
                    "a single television has no audience output",
                    vec![television(0)],
                    Largest,
                    None,
                    None,
                ),
                (
                    "no screen has no audience output",
                    vec![],
                    Largest,
                    None,
                    None,
                ),
                (
                    "a single screen has no audience output even when it is named",
                    vec![laptop(0)],
                    Output(0),
                    None,
                    None,
                ),
                (
                    "the first external display, not the primary-flagged laptop panel",
                    vec![flagged_laptop, desk_monitor(1), television(2)],
                    PrimaryExternal,
                    None,
                    Some(1),
                ),
                (
                    "primary external is the primary-flagged television",
                    vec![laptop(0), desk_monitor(1), flagged_tv],
                    PrimaryExternal,
                    None,
                    Some(2),
                ),
                (
                    "the compositor moves the presenter view off an explicitly chosen screen",
                    laptop_tv.clone(),
                    Output(0),
                    Some(0),
                    Some(0),
                ),
                (
                    "an explicit output that does not exist is no audience output",
                    laptop_tv.clone(),
                    Output(9),
                    None,
                    None,
                ),
                (
                    "auto chooses as largest does",
                    laptop_tv.clone(),
                    Auto,
                    None,
                    Some(1),
                ),
                (
                    "primary is the flagged screen, even the smaller one",
                    vec![mon(0, 1280, 720, true), mon(1, 1920, 1080, false)],
                    Primary,
                    None,
                    Some(0),
                ),
                (
                    "primary without a flag chooses as largest does",
                    laptop_tv,
                    Primary,
                    None,
                    Some(1),
                ),
            ];
            for (case, monitors, target, presenter, want) in cases {
                assert_eq!(
                    select_audience_output(&monitors, target, presenter),
                    want,
                    "case: {case}"
                );
            }
        }

        phase the_presenter_output_is_the_built_in_or_smallest_screen_left;
        // `select_presenter_output`: where the presenter view goes once the audience owns a screen.
        {
            let laptop_tv = vec![laptop(0), television(1)];
            let twins = vec![mon(0, 1920, 1080, false), mon(1, 1920, 1080, false)];
            let cases = [
                (
                    "laptop and television: the laptop",
                    laptop_tv.clone(),
                    1,
                    Some(0),
                ),
                (
                    "desk monitor and television: the desk monitor",
                    vec![desk_monitor(0), television(1)],
                    1,
                    Some(0),
                ),
                (
                    "laptop, desk monitor and television: the presenter stays on the laptop",
                    vec![laptop(0), desk_monitor(1), television(2)],
                    2,
                    Some(0),
                ),
                (
                    "a single laptop has no other output",
                    vec![laptop(0)],
                    0,
                    None,
                ),
                (
                    "a single television has no other output",
                    vec![television(0)],
                    0,
                    None,
                ),
                ("no screen has no other output", vec![], 0, None),
                (
                    "an audience explicitly on the laptop sends the presenter to the television",
                    laptop_tv.clone(),
                    0,
                    Some(1),
                ),
                (
                    "an audience output that is not listed leaves the built-in panel",
                    laptop_tv,
                    9,
                    Some(0),
                ),
                (
                    "with no built-in panel and a size unknown, the fewer pixels",
                    vec![desk_monitor(0), television(1), mon(2, 1920, 1080, false)],
                    1,
                    Some(2),
                ),
                (
                    "identical screens: the other one (0)",
                    twins.clone(),
                    1,
                    Some(0),
                ),
                ("identical screens: the other one (1)", twins, 0, Some(1)),
            ];
            for (case, monitors, audience, want) in cases {
                assert_eq!(
                    select_presenter_output(&monitors, audience),
                    want,
                    "case: {case}"
                );
            }
        }
    }

    #[test]
    fn request_constructors_carry_view() {
        assert!(!FullscreenRequest::mirror(FullscreenTarget::Largest).is_distinct());
        assert!(FullscreenRequest::distinct(FullscreenTarget::Largest).is_distinct());
    }
}
