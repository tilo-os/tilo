//! Reusable application *chrome* — the framed UI around content: toolbars and
//! status bars. These are themed draw + hit-test helpers (the app owns the
//! button specs and click handling; the toolkit owns the look), so any `ui` app
//! gets a consistent, OS-themed frame without re-implementing pixel layout.

pub mod page;
pub mod status_bar;
pub mod toolbar;

pub use page::PageGeometry;
pub use status_bar::STATUS_BAR_HEIGHT;
pub use toolbar::{LabelCue, ToolbarItem, TOOLBAR_HEIGHT};
