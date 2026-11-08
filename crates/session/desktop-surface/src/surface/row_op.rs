//! A widget-side follow-up to a command already applied to the tree.

use tilo_desktop_model::{GroupId, StripId, Tab, TabId, WindowWidth};
use ui::widget::TabDescriptor;

use crate::navigation::{HistoryDirection, ScrollAction};

/// A widget-side follow-up to a command already applied to the tree.
pub(super) enum RowOp {
    Show(StripId, GroupId, TabId),
    /// A window renamed in the tree, as the row names its pill.
    Rename {
        strip: StripId,
        group: GroupId,
        tab: TabDescriptor,
    },
    Width(StripId, GroupId, WindowWidth),
    ToggleMaximized(StripId, GroupId),
    /// A launched window, already in the tree at `index` of `strip` .
    Open {
        strip: StripId,
        group: GroupId,
        index: usize,
        tab: Tab,
    },
    /// A tab opened into a group that is already there, at `index` of its tabs, with the tab that
    /// ends up in front.
    TabInGroup {
        strip: StripId,
        group: GroupId,
        index: usize,
        tab: Tab,
        front: TabId,
        background: bool,
    },
    Close {
        strip: StripId,
        group: GroupId,
        tab: TabId,
        group_removed: bool,
    },
    Pan(StripId, f64),
    Scroll(StripId, GroupId, ScrollAction),
    History(StripId, GroupId, TabId, HistoryDirection),
    /// The share row of a window: share it with its workspace's people, or stop sharing it.
    WindowShare(TabId, bool),
}
