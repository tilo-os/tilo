//! Logs — a graphical systemd-journal viewer, built on the qOS `ui`
//! toolkit (CRDT-native, retained-mode).
//!
//! The viewer is a single self-rendering [`app::LogsApp`] widget: a
//! scrollable, severity-coloured entry list; a detail pane that surfaces
//! every field of the selected record; and a filter bar (free-text
//! search, unit, boot, and a syslog-priority threshold) plus a
//! live-tail toggle. All *view state* lives in the Automerge
//! document (see [`doc::LogsDoc`]) so selection, filters, and search sync
//! across collaborators and participate in undo/redo for free; the
//! journal entries themselves are an external render cache.
//!
//! Data sources:
//! - **Live**: parses `journalctl -o json` ([`journal::load_live`]).
//! - **`--shots` / offline**: a bundled, realistic sample journal
//!   ([`journal::sample_entries`]) so the headless screenshot is fully
//!   populated without systemd access.
//!
//! [`app_root`] is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
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

pub mod app;
pub mod doc;
pub mod journal;
pub mod model;
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

    let live = journal::load_live(2000);
    // Live-tail only makes sense against a journal we could actually read.
    let allow_live = !live.is_empty();
    Box::new(LogsApp::new(live, allow_live))
}
