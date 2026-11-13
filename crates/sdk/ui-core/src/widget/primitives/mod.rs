//! The design system's primitives (`docs/design-language.md`, "Primitives"): the only way to
//! build these things.

mod activity_card;
mod bar_chip;
#[path = "bar_chip_component.rs"]
mod bar_chip_component;
mod bar_control;
mod control_row_paint;
mod disclosure_row;
mod facts;
mod floating_panel;
mod key_hint;
mod notification_list;
mod quick_tile;
mod status_cluster;
mod surface;
#[path = "surfaces_component.rs"]
mod surfaces_component;
mod text_stack;

pub use activity_card::ActivityCardFace;
pub use bar_chip::{BarChipFace, BarChipLayout};
pub use bar_chip_component::{BarChip, BarChipProps};
pub use bar_control::{BarControlFace, IconButton};
pub use control_row_paint::{control_fill, control_radius, paint_control_row, paint_severity_rail};
pub use disclosure_row::{DisclosureGroup, DisclosureRow, DisclosureRowProps, DisclosureState};
pub use facts::{Fact, FactMark, FactProps, Facts, FactsFace, FactsProps};
pub use notification_list::{
    NotificationListControl, NotificationListLayout, NotificationListRow, NotificationListView,
    NotificationRowControls,
};
pub use surfaces_component::{
    DeclaredModal, DeclaredPanel, DeclaredPopover, DeclaredPopoverButton, FloatingPanelProps,
    ModalProps, PopoverButtonProps, PopoverProps,
};

pub use floating_panel::{FloatingPanel, FloatingPanelFace, FloatingPanelLayout};
pub use key_hint::{KeyHint, KeyHintProps, KeyHints, KeyHintsFace, KeyHintsProps};
pub use quick_tile::{QuickTileFace, paint_level_gauge};
pub use status_cluster::{
    LinkKind, LinkStatus, StatusCluster, StatusClusterFace, StatusClusterLayout, StatusSlot,
    badge_label, badge_size, paint_unread_badge,
};
pub use surface::{
    DensityRegion, Surface, SurfaceLevel, paint_document_sheet, paint_floating_window_card,
    paint_see_through, paint_surface, paint_window_card,
};
pub use text_stack::{
    StackSize, TextBlockFace, TextLineAlign, TextLineFace, TextStack, TextStackFace, TextStackProps,
};
