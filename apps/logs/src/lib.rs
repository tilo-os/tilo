//! Logs — a graphical systemd-journal viewer, built on the Tilo `ui`
//! toolkit (CRDT-native, retained-mode).
//!
//! The viewer is a single self-rendering [`app::LogsApp`] widget: a
//! scrollable, severity-coloured entry list; a detail pane that surfaces
//! every field of the selected record; and a filter bar (free-text
//! search, unit, boot, and a syslog-priority threshold) plus a
//! live-tail toggle. Shared filter and selection preferences sync as document
//! actions on [`resource::Logs`]. Live tail is a process-local Effect (it
//! starts this process's journal poller); journal entries themselves are an
//! external render cache. The resource catalogue also exposes refresh,
//! follow/pause, and an explicit-path JSON Lines export; it publishes
//! metadata and stable entry identities, never raw log payloads.
//!
//! Data sources:
//! - **Live**: parses `journalctl -o json` ([`journal::load_live`]).
//! - **`--shots` / offline**: a bundled, realistic sample journal
//!   ([`journal::sample_entries`]) so the headless screenshot is fully
//!   populated without systemd access.
//!
//! [`app_root`] is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
pub mod doc;
pub mod framework_history;
#[cfg(test)]
mod framework_history_tests;
pub mod journal;
pub mod model;
pub mod resource;
pub mod responsive;
pub mod theme_ext;

pub use app::LogsApp;
pub use model::{Filter, LogEntry, Priority};

use ui::widget::Widget;

/// Build the app's root widget.
///
/// When invoked under `--shots` we deliberately render the bundled sample
/// journal so the screenshot is deterministic and populated even with no
/// journald access. Otherwise we read the *real* journal: if it has entries
/// the viewer shows them with live-tail enabled; if it is empty or
/// unreadable the viewer shows an honest zero-state, never the sample — a
/// system log app must not fabricate log lines.
pub fn app_root() -> Box<dyn Widget> {
    let shots_mode = std::env::args().any(|a| a == "--shots" || a.starts_with("--shots="));

    if shots_mode {
        return Box::new(LogsApp::new(journal::sample_entries(), false));
    }

    match journal::load_live(2000) {
        // A successful empty journal is still a live-capable source: Refresh
        // and Follow stay available as recovery controls.
        Ok(entries) => Box::new(LogsApp::new(entries, true)),
        Err(error) => Box::new(LogsApp::new_with_source_error(Vec::new(), true, error)),
    }
}
pub use resource::{Logs, LogsShell};
