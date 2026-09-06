//! Shared fullscreen / presentation mechanism.
//!
//! A root widget expresses a [`FullscreenRequest`] via
//! [`crate::widget::Widget::fullscreen_request`]. The App
//! ([`crate::app::App`]) polls it each frame, runs [`select_output`] over
//! the live monitors, and drives `winit::window::Window::set_fullscreen` —
//! a genuine borderless full-output surface, not a maximized window.
//! [`select_output`] is pure over [`MonitorInfo`] so it's unit-tested
//! without a live window.
//!
//! A headless session or a compositor which cannot expose its output globals
//! yields an empty monitor list; that degrades to `Borderless(None)` — still
//! real fullscreen, just compositor-placed. Multi-output presentation is
//! activated only after a distinct monitor is actually enumerated.

/// Which output an app wants its fullscreen surface promoted onto.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FullscreenTarget {
    /// Let the compositor choose (fill whatever output it decides).
    Auto,
    /// The largest available monitor by pixel area — the presentation default,
    /// so "Present" / immersive video lands on the big screen.
    #[default]
    Largest,
    /// The primary monitor (falls back to the first if none is flagged primary).
    Primary,
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
}

impl MonitorInfo {
    /// Pixel area, used to rank monitors for [`FullscreenTarget::Largest`].
    pub fn area(&self) -> u64 {
        u64::from(self.width).saturating_mul(u64::from(self.height))
    }
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
        FullscreenTarget::Largest => monitors
            .iter()
            // Break area ties toward the primary, then the earliest index, so
            // the choice is deterministic across identical monitors.
            .max_by(|a, b| {
                a.area()
                    .cmp(&b.area())
                    .then(a.primary.cmp(&b.primary))
                    .then(b.index.cmp(&a.index))
            })
            .map(|m| m.index),
        FullscreenTarget::Primary => monitors
            .iter()
            .find(|m| m.primary)
            .or_else(|| monitors.first())
            .map(|m| m.index),
        FullscreenTarget::Output(i) => monitors.iter().find(|m| m.index == i).map(|m| m.index),
    }
}

/// Pick an audience display which is *not* the display containing the
/// presenter window. `None` means that there is no honest second output.
///
/// An explicit request for the presenter's own output is rejected rather than
/// silently putting a second surface on top of the dashboard. For broad
/// targets, selection is deterministic among the remaining outputs.
pub fn select_audience_output(
    monitors: &[MonitorInfo],
    target: FullscreenTarget,
    presenter_output: Option<u32>,
) -> Option<u32> {
    let candidates: Vec<MonitorInfo> = monitors
        .iter()
        .copied()
        .filter(|monitor| Some(monitor.index) != presenter_output)
        .collect();
    if candidates.is_empty() {
        return None;
    }
    match target {
        FullscreenTarget::Auto => candidates.first().map(|monitor| monitor.index),
        FullscreenTarget::Largest => candidates
            .iter()
            .max_by(|a, b| {
                a.area()
                    .cmp(&b.area())
                    .then(a.primary.cmp(&b.primary))
                    .then(b.index.cmp(&a.index))
            })
            .map(|monitor| monitor.index),
        FullscreenTarget::Primary => candidates
            .iter()
            .find(|monitor| monitor.primary)
            .or_else(|| candidates.first())
            .map(|monitor| monitor.index),
        FullscreenTarget::Output(index) => candidates
            .iter()
            .find(|monitor| monitor.index == index)
            .map(|monitor| monitor.index),
    }
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
    fn audience_selection_never_reuses_the_presenter_output() {
        let mons = vec![
            mon(0, 1920, 1080, true),
            mon(1, 3840, 2160, false),
            mon(2, 1280, 720, false),
        ];
        assert_eq!(
            select_audience_output(&mons, FullscreenTarget::Largest, Some(1)),
            Some(0),
            "the largest display is already the local presenter output"
        );
        assert_eq!(
            select_audience_output(&mons, FullscreenTarget::Output(1), Some(1)),
            None,
            "an explicit self-target must not fake dual display mode"
        );
        assert_eq!(
            select_audience_output(
                &[mon(0, 1920, 1080, true)],
                FullscreenTarget::Largest,
                Some(0)
            ),
            None
        );
    }
}
