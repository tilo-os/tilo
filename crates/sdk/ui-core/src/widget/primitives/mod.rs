//! The design system's primitives (`docs/design-language.md`,
//! "Primitives"): the only way to build these things. Each owns its
//! padding, radius, colour and motion; callers pass content, never geometry
//! or colour.
//!
//! | Primitive | Is |
//! |---|---|
//! | [`Surface`] / [`paint_surface`] | one of the five surface levels with its edge and shadow |
//! | [`FloatingPanel`] / [`FloatingPanelFace`] | a floating surface with header, body and footer slots |
//! | [`TextStack`] / [`TextStackFace`] | title, secondary and tertiary lines |
//! | [`Facts`] / [`FactsFace`] | separate facts with space between, never a joined string |
//! | [`KeyHintsFace`] | drawn keycaps and what they do |
//! | [`BarControlFace`] | a top bar or window bar control |
//! | [`IconButton`] | a square icon button whose name is required |
//! | [`DisclosureRow`] | a row whose body opens in place |
//! | [`DensityRegion`] | a region laid out at compact (or comfortable) density |
//! | [`QuickTileFace`] / [`paint_level_gauge`] | a Quick Settings tile's ground, ink and chevron rule; a charge gauge |
//!
//! The rest of the set is the existing shared widgets, restyled on the
//! tokens: [`crate::widget::Button`] (primary, secondary, quiet =
//! [`crate::widget::ButtonRole::Plain`], destructive),
//! [`crate::widget::Chip`] (people, tags, filters only),
//! [`crate::widget::Toggle`], [`crate::widget::Checkbox`],
//! [`crate::widget::ToggleGroup`] (segmented), [`crate::widget::Slider`],
//! [`crate::widget::ListRow`], [`crate::widget::Section`],
//! [`crate::widget::PropertyList`], [`crate::widget::Breadcrumb`],
//! [`crate::widget::EmptyState`], [`crate::widget::Banner`] and the toast.

mod bar_control;
mod disclosure_row;
mod facts;
mod floating_panel;
mod key_hint;
mod quick_tile;
mod surface;
mod text_stack;

pub use bar_control::{BarControlFace, IconButton};
pub use disclosure_row::{DisclosureGroup, DisclosureRow, DisclosureState};
pub use facts::{Fact, Facts, FactsFace};
pub use floating_panel::{FloatingPanel, FloatingPanelFace, FloatingPanelLayout};
pub use key_hint::{KeyHint, KeyHints, KeyHintsFace};
pub use quick_tile::{QuickTileFace, paint_level_gauge};
pub use surface::{DensityRegion, Surface, SurfaceLevel, paint_surface};
pub use text_stack::{StackSize, TextStack, TextStackFace};
