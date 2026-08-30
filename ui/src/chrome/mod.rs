//! Reusable *chrome* — the framed UI around content. Two layers, not the
//! same thing: **application chrome** ([`page`], [`status_bar`],
//! [`toolbar`]) is the frame an app draws around its own content (the app
//! owns button specs and click handling, the toolkit owns the look);
//! **shell chrome** ([`shell`]) is the vocabulary the *OS* paints in
//! around and between apps — top bar, window decorations, workspace disc,
//! quick settings, collaborator surfaces (see `ui/HIG.md` §9).

pub mod page;
pub mod shell;
pub mod status_bar;
pub mod toolbar;

pub use page::PageGeometry;
pub use shell::{
    ChromeRole, SelectionState, SelectionTier, fill_scrim, scrim_maps, selection_tier,
};
pub use status_bar::STATUS_BAR_HEIGHT;
pub use toolbar::{LabelCue, TOOLBAR_HEIGHT, ToolbarItem};
