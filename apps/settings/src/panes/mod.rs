//! One module per settings category.
//!
//! Each pane exposes a `build()` (or category-specific) function returning a
//! `Box<dyn Widget>` containing that pane's content. The wrapper chrome
//! (the pane header) lives in `settings_app::build_pane`, so the modules
//! here focus on the category-specific body.

pub mod about;
pub mod audio;
pub mod bluetooth;
pub mod collaboration;
pub mod display;
pub mod network;
pub mod workspaces;

use ui::widget::{Column, Text, Widget};

/// Shared placeholder used by every pane that's a stub today. Renders a
/// dimmed `Coming soon` line plus an explanatory note pointing at the
/// shell-side `qs_actions` binding the pane will plug into later.
///
/// Lives in `panes::mod` so individual stubs don't repeat the same
/// scaffolding (DRY for the placeholder; once a pane gains real content
/// it just stops calling `coming_soon`).
pub(crate) fn coming_soon(detail: &str) -> Box<dyn Widget> {
    Box::new(
        Column::new()
            .push(Box::new(Text::new("Coming soon").with_size(16.0)))
            .push(Box::new(Text::new(detail).with_size(12.0))),
    )
}
