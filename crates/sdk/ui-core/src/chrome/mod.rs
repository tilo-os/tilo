//! Reusable *chrome* — the framed UI around content. Two layers, not the
//! same thing: **application chrome** ([`page`], [`status_bar`] and the
//! [`crate::widget::Toolbar`]) is the frame an app draws around its own content (the app
//! owns button specs and click handling, the toolkit owns the look);
//! **shell chrome** ([`shell`]) is the vocabulary the *OS* paints in
//! around and between apps — top bar, window decorations, workspace disc,
//! quick settings, collaborator surfaces (see `crates/sdk/ui-core/HIG.md` §9).

pub mod nav_row;
pub mod page;
pub mod shell;
pub mod status_bar;

pub use nav_row::{
    NavCount, NavRowFace, NavRowLayout, NavRowStyle, NavSectionFace, NavSectionLayout,
    nav_rail_row_height, nav_row_height, nav_row_layout, nav_section_height, nav_section_layout,
    paint_nav_row, paint_nav_section,
};
pub use page::PageGeometry;
pub use shell::{
    ChromeRole, GLOW_ALPHA, HAIRLINE_ALPHA, PANEL_BORDER_ALPHA, SELECTION_MIN_STEP, SelectionState,
    SelectionTier, chrome_hairline, fill_scrim, glass_selector, lightness_step,
    paint_row_selection, panel_border, panel_ground, panel_shadow, scrim_maps,
    selection_floor_ramp, selection_tier, selection_tier_on,
};
