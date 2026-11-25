//! Reusable *chrome* — the framed UI around content. Two layers, not the same thing: **application
//! chrome** ([`page`], [`status_bar`] and the [`crate::widget::Toolbar`]) is the frame an app draws
//! around its content (the app owns button specs and click handling, the toolkit owns the look);
//! **shell chrome** ([`shell`]) is the vocabulary the *OS* paints in around and between apps — top bar,
//! window decorations, workspace disc, quick settings, member surfaces (see
//! `crates/ui/ui-core/HIG.md` §9).

pub mod marks;
pub mod page;
pub mod selector;
pub mod shell;
pub mod status_bar;

pub use marks::{paint_selected_ring, paint_selection_band};
pub use page::PageGeometry;
pub use selector::{paint_selector, selector_finish, selector_tier, strip_ground, strip_shape};
pub use shell::{
    ChromeRole, GLOW_ALPHA, HAIRLINE_ALPHA, PANEL_BORDER_ALPHA, RowJoin, SELECTION_MIN_STEP,
    SelectionState, SelectionTier, chrome_hairline, column_gutter, disclosure_gutter, fill_scrim,
    lightness_step, paint_alignment_guide, paint_panel, paint_row_selection,
    paint_row_selection_joined, panel_border, panel_ground, panel_shadow, row_bleed, scrim_channel,
    scrim_maps, selection_floor, selection_floor_ramp, selection_tier, selection_tier_on,
};
