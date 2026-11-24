//! The shell's control hints, as data: how to open anything from search, move a window, resize one
//! and move the view, each taught once at the moment it is useful. Every shell style draws the same
//! set ([`super::ControlHints`]), whichever way it lays its windows out.

use super::performed::Performed;
use super::{Cue, Gesture, HintSpec};
use crate::time::Duration;

/// What the shell's hints are cued and taught by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// The person did this.
    Did(Performed),
    /// The shell shows two windows or more.
    TwoWindows,
    /// Windows lie out of view.
    WindowsOutOfView,
}

/// The shell's control hints, in the order they are offered.
pub static HINTS: &[HintSpec<Signal>] = &[
    HintSpec {
        id: "open-from-search",
        when: &[Cue::After(Duration::from_secs(17))],
        only_while: &[],
        learned_by: &[
            Signal::Did(Performed::OpenedSearch),
            Signal::Did(Performed::OpenedWindow),
        ],
        gesture: Gesture::OpenFromSearch,
        words: "control-hint-open",
        touch_words: None,
    },
    HintSpec {
        id: "move-window",
        when: &[
            Cue::Saw(Signal::Did(Performed::HoveredWindowBar)),
            Cue::Saw(Signal::Did(Performed::OpenedWindow)),
        ],
        // A lone window held by its bar has nowhere to go: moving one means carrying it past
        // another.
        only_while: &[Signal::TwoWindows],
        learned_by: &[Signal::Did(Performed::MovedWindow)],
        gesture: Gesture::MoveWindow,
        words: "control-hint-move",
        touch_words: None,
    },
    HintSpec {
        id: "resize-window",
        when: &[Cue::Holds(Signal::TwoWindows)],
        only_while: &[Signal::TwoWindows],
        learned_by: &[Signal::Did(Performed::ResizedWindow)],
        gesture: Gesture::ResizeEdge,
        words: "control-hint-resize",
        touch_words: None,
    },
    HintSpec {
        id: "pan-view",
        when: &[Cue::Holds(Signal::WindowsOutOfView)],
        only_while: &[Signal::WindowsOutOfView],
        learned_by: &[Signal::Did(Performed::PannedView)],
        gesture: Gesture::PanView,
        // What moves the view is a sideways scroll (a touchpad's fingers, a mouse's sideways
        // wheel) or a finger swiping; dragging the empty canvas works too, but there is rarely
        // any in sight.
        words: "control-hint-pan",
        touch_words: Some("control-hint-pan-touch"),
    },
];
