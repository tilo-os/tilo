//! Reusable *chrome* — the framed UI around content. Two layers, not the
//! same thing: **application chrome** ([`page`], [`status_bar`] and the
//! [`crate::widget::Toolbar`]) is the frame an app draws around its own content (the app
//! owns button specs and click handling, the toolkit owns the look);
//! **shell chrome** ([`shell`]) is the vocabulary the *OS* paints in
//! around and between apps — top bar, window decorations, workspace disc,
//! quick settings, collaborator surfaces (see `crates/sdk/ui-core/HIG.md` §9).

pub mod header_row;
pub mod nav_row;
pub mod page;
pub mod selector;
pub mod shell;
pub mod status_bar;

pub use header_row::{HeaderRowFace, HeaderRowMetrics, paint_history_control};
pub use nav_row::{
    NavCount, NavRowFace, NavRowLayout, NavRowStyle, NavSectionFace, NavSectionLayout,
    nav_badge_column, nav_item_height, nav_rail_item_height, nav_rail_row_height,
    nav_row_description, nav_row_height, nav_row_layout, nav_section_height, nav_section_layout,
    paint_nav_row, paint_nav_section, rail_caption_lines, rail_width_fitting,
};
pub use page::PageGeometry;
pub use selector::{
    SelectorRole, paint_selector, selector_finish, selector_tier, strip_ground, strip_shape,
};
pub use shell::{
    ChromeRole, GLOW_ALPHA, HAIRLINE_ALPHA, PANEL_BORDER_ALPHA, RowJoin, SELECTION_MIN_STEP,
    SelectionState, SelectionTier, chrome_hairline, column_gutter, fill_scrim, lightness_step,
    paint_row_selection, paint_row_selection_joined, panel_border, panel_ground, panel_shadow,
    row_bleed, scrim_channel, scrim_maps, selection_floor_ramp, selection_tier, selection_tier_on,
};
