//! The design system's primitives (`docs/design-language.md`, "Primitives"): the only way to build
//! these things. Each owns its padding, radius, colour and motion; callers pass content, never
//! geometry or colour.
//!
//! | Primitive | Is |
//! |---|---|
//! | [`Surface`] / [`paint_surface`] | one of the five surface levels with its edge and shadow |
//! | [`paint_window_card`] | a window's card and its frame over the parts that reach its edge |
//! | [`paint_document_sheet`] | a document that brings its own colours, on a sheet with an edge |
//! | [`paint_see_through`] | the see-through material for chrome over pixels being chosen |
//! | [`FloatingPanel`] / [`FloatingPanelFace`] | a floating surface with header, body and footer slots |
//! | [`TextStack`] / [`TextStackFace`] | title, secondary and tertiary lines |
//! | [`TextLineFace`] | one line of words in a role, measured and centred or started |
//! | [`TextBlockFace`] | wrapped running text in one style, at a width |
//! | [`paint_control_row`] | a Control's fill, edge and focus ring, the list language |
//! | [`Facts`] / [`FactsFace`] | separate facts with space between, never a joined string |
//! | [`KeyHintsFace`] | drawn keycaps and what they do |
//! | [`BarControlFace`] | a top bar or window bar control |
//! | [`ActivityCardFace`] | an activity's card on the floating layer |
//! | [`BarChip`] / [`BarChipFace`] | a bar control holding a face stack, a label and a press |
//! | [`IconButton`] | a square icon button whose name is required |
//! | [`DisclosureRow`] | a row whose body opens in place |
//! | [`DensityRegion`] | a region laid out at compact (or comfortable) density |
//! | [`QuickTileFace`] / [`paint_level_gauge`] | a Quick Settings tile's ground, ink and chevron rule; a charge gauge |
//! | [`StatusCluster`] / [`StatusClusterFace`] | the bar's status cluster: slots, a count and the critical-battery wash |
//!
//! The rest of the set is the existing shared widgets, restyled on the tokens:
//! [`crate::widget::Button`] (primary, secondary, quiet = [`crate::widget::ButtonRole::Plain`],
//! destructive), [`crate::widget::Chip`] (people, tags, filters only), [`crate::widget::Toggle`],
//! [`crate::widget::Checkbox`], [`crate::widget::ToggleGroup`] (segmented),
//! [`crate::widget::Slider`], [`crate::widget::ListRow`], [`crate::widget::Section`],
//! [`crate::widget::PropertyList`], [`crate::widget::Breadcrumb`],
//! [`crate::widget::EmptyState`], [`crate::widget::Banner`] and the toast.

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
pub use facts::{Fact, FactProps, Facts, FactsFace, FactsProps};
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
    DensityRegion, Surface, SurfaceLevel, paint_document_sheet, paint_see_through, paint_surface,
    paint_window_card,
};
pub use text_stack::{
    StackSize, TextBlockFace, TextLineAlign, TextLineFace, TextStack, TextStackFace, TextStackProps,
};
