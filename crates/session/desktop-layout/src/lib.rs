//! The layout vocabulary the desktop model and the toolkit share: window-group
//! widths as fractions of the strip, a group's shape as a strip widget reports
//! it, and where a dragged tab lands. The toolkit draws strips with these
//! without depending on the desktop model itself.

pub mod width;

pub use width::WindowWidth;

/// A window group's id, as the desktop model mints it.
pub type GroupId = u64;
/// A tab's id, as the desktop model mints it.
pub type TabId = u64;

/// Where a dragged tab lands within a strip: at an exact index in an
/// existing group, or as a new group at an exact seam between groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropTarget {
    ExistingGroup { group_id: GroupId, tab_index: usize },
    GroupGap { gap_index: usize },
}

/// One window group as a live strip widget reports it: identities, order,
/// the active tab and width, but no tile content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupShape {
    pub id: GroupId,
    pub tab_ids: Vec<TabId>,
    pub active_tab: TabId,
    pub width: WindowWidth,
}
