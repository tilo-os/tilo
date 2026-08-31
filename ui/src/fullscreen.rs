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
//! The current qOS compositor advertises no `wl_output` globals, so the
//! enumerated list is empty and this degrades to `Borderless(None)` — still
//! real fullscreen, just compositor-placed. Selection logic is already
//! correct for when the compositor advertises monitors.

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
}
