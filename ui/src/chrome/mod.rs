//! Reusable *chrome* — the framed UI around content.
//!
//! Two layers live here, and they are not the same thing:
//!
//! - **Application chrome** ([`page`], [`status_bar`], [`toolbar`]) — the
//!   frame an app draws around its own content. Themed draw + hit-test
//!   helpers: the app owns the button specs and click handling, the
//!   toolkit owns the look.
//! - **Shell chrome** ([`shell`]) — the vocabulary the *OS* paints in,
//!   around and between apps: the top bar, window decorations, the
//!   workspace disc, quick settings, the collaborator surfaces. See
//!   `ui/HIG.md` §9.
//!
//! Both exist so a surface does not have to invent its own appearance.

pub mod page;
pub mod shell;
pub mod status_bar;
pub mod toolbar;

pub use page::PageGeometry;
pub use shell::{ChromeRole, SelectionState, SelectionTier, fill_scrim, scrim_maps, selection_tier};
pub use status_bar::STATUS_BAR_HEIGHT;
pub use toolbar::{LabelCue, ToolbarItem, TOOLBAR_HEIGHT};
