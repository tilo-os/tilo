//! Logs — a graphical systemd-journal viewer, built on the qOS `ui`
//! toolkit (CRDT-native, retained-mode).
//!
//! The viewer is a single self-rendering [`app::LogsApp`] widget: a
//! scrollable, severity-coloured entry list; a detail pane that surfaces
//! every field of the selected record; and a filter bar (free-text
//! search, unit, boot, and a syslog-priority threshold) plus a severity
//! legend and a live-tail toggle. All *view state* lives in the Automerge
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

pub mod app;
pub mod doc;
pub mod journal;
pub mod model;
pub mod theme_ext;

pub use app::LogsApp;
pub use model::{Filter, LogEntry, Priority};

use ui::widget::Widget;

/// Build the app's root widget.
///
/// When invoked under `--shots` we deliberately render the bundled sample
/// journal so the screenshot is deterministic and populated even with no
/// journald access. Otherwise we try the live journal and fall back to
/// the sample if it is unavailable or empty (e.g. missing permissions),
/// so the viewer is never blank.
pub fn app_root() -> Box<dyn Widget> {
    let shots_mode = std::env::args()
        .any(|a| a == "--shots" || a.starts_with("--shots="));

    if shots_mode {
        return Box::new(LogsApp::new(journal::sample_entries(), false));
    }

    let live = journal::load_live(2000);
    if live.is_empty() {
        // No journald (or no access) — show the sample so the UI has
        // something to render, but don't enable live tail against a
        // source we couldn't read.
        Box::new(LogsApp::new(journal::sample_entries(), false))
    } else {
        Box::new(LogsApp::new(live, true))
    }
}
