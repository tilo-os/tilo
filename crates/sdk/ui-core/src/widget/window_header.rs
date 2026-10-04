//! The window's header row, as a capability: the one place window-level
//! controls stand.
//!
//! A widget that draws the header row across the top of the window
//! ([`NavShell`](crate::widget::NavShell)) provides [`WindowHeader`]. The
//! host offers it the window's own controls (today: History) before it
//! builds the app, and draws them itself only where the app has no header
//! row.

use super::Widget;
use super::capability::capability_mut;

/// What the window's History control shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HistoryControl {
    /// History is closed; the control opens it.
    Closed,
    /// History is showing; the control reads as on and closes it.
    Open,
}

impl HistoryControl {
    pub(crate) fn of(open: bool) -> Self {
        if open { Self::Open } else { Self::Closed }
    }

    pub(crate) fn is_open(self) -> bool {
        self == Self::Open
    }
}

/// A widget that draws the window's header row.
pub(crate) trait WindowHeader {
    /// Show the window's History control at the trailing edge of the row,
    /// or none (`None`: the app has no History, or the host's own chrome
    /// presents it).
    fn set_history_control(&mut self, control: Option<HistoryControl>);
}

/// Hand `control` to the first header row in `root`'s tree. Returns whether
/// the app has one; the host then draws no History control of its own.
pub(crate) fn offer_history_control(
    root: &mut dyn Widget,
    control: Option<HistoryControl>,
) -> bool {
    if let Some(header) = capability_mut::<dyn WindowHeader>(root) {
        header.set_history_control(control);
        return true;
    }
    root.children_mut()
        .into_iter()
        .any(|child| offer_history_control(child, control))
}
