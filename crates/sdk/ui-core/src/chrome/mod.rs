//! Reusable *chrome* — the framed UI around content. Two layers, not the
//! same thing: **application chrome** ([`page`], [`status_bar`],
//! [`toolbar`]) is the frame an app draws around its own content (the app
//! owns button specs and click handling, the toolkit owns the look);
//! **shell chrome** ([`shell`]) is the vocabulary the *OS* paints in
//! around and between apps — top bar, window decorations, workspace disc,
//! quick settings, collaborator surfaces (see `crates/sdk/ui-core/HIG.md` §9).

pub mod page;
pub mod shell;
pub mod status_bar;
pub mod toolbar;

pub use page::PageGeometry;
pub use shell::{
    ChromeRole, GLOW_ALPHA, HAIRLINE_ALPHA, PANEL_BORDER_ALPHA, SelectionState, SelectionTier,
    chrome_hairline, fill_scrim, glass_selector, panel_border, panel_ground, panel_shadow,
    scrim_maps, selection_tier, selection_tier_on,
};
pub use status_bar::STATUS_BAR_HEIGHT;
pub use toolbar::{
    LabelCue, TOOLBAR_HEIGHT, ToolbarItem, ToolbarMetrics, bar_padding, item_rects, paint_bar,
};
