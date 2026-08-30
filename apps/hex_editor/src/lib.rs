//! Hex — a hex editor for qOS, built on the `ui` toolkit and CRDT-native.
//!
//! The editable byte buffer lives in the shared Automerge document, so
//! undo/redo and live collaboration work without any app-level plumbing.
//! The UI is a classic hex view — offset gutter, hex byte columns, and an
//! ASCII column with a cursor synced across both — plus a data inspector
//! that interprets the bytes at the cursor as every common scalar type,
//! search (hex pattern or text), and goto-offset.
//!
//! `app_root()` is the single integration point used by both the live
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
        reason = "a test that unwraps is stating an invariant"
    )
)]

pub mod actions;
pub mod app;
pub mod doc;
pub mod inspector;
pub mod model;
pub mod portal;
pub mod sample;
pub mod search;
pub mod view;

use ui::widget::Widget;

pub use model::HexModel;
pub use view::HexView;

/// Build and return the app's root widget tree.
///
/// Honours a single optional positional argument: a path to open. Otherwise:
/// `--shots` opens the bundled sample buffer so the headless screenshot shows
/// real structured data; the live app with no file opens the honest empty
/// state — it never fabricates a buffer.
pub fn app_root() -> Box<dyn Widget> {
    if let Some(path) = file_arg() {
        return Box::new(app::view_for_path(std::path::Path::new(&path)));
    }
    let shots_mode = std::env::args().any(|a| a == "--shots" || a.starts_with("--shots="));
    // A `--shots`-only hook to capture the empty state deterministically.
    let want_empty = std::env::var("QOS_HEX_DEMO").as_deref() == Ok("empty");
    let view = if shots_mode && !want_empty {
        // Arm the transient floating status bubble so the single headless frame
        // captures the floating cursor read-out chip (it otherwise appears on a
        // change / hover and fades, so a still frame would miss it).
        let mut v = app::sample_view();
        v.arm_status_bubble_for_shots();
        v
    } else {
        HexView::empty()
    };
    Box::new(view)
}

/// First positional file argument, skipping the renderer's own flags so
/// `--shots OUT.png` never gets mistaken for a file to open. Returns the
/// path only if it actually exists.
fn file_arg() -> Option<String> {
    parse_file_arg(std::env::args().skip(1).collect())
}

/// Pure argument scanner: pull the first positional argument, treating
/// `--shots` (and its `PATH` value) plus any other `--flag` as
/// non-positional. Split out so it is unit-testable without touching the
/// process environment.
fn parse_file_arg(args: Vec<String>) -> Option<String> {
    let mut iter = args.into_iter();
    while let Some(a) = iter.next() {
        if a == "--shots" {
            // Consume the flag's value too.
            iter.next();
            continue;
        }
        if a.starts_with("--") {
            continue;
        }
        if std::path::Path::new(&a).exists() {
            return Some(a);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse_file_arg;

    #[test]
    fn shots_flag_and_value_are_not_treated_as_file() {
        let args = vec!["--shots".to_string(), "/tmp/whatever-out.png".to_string()];
        // The PATH after --shots must never be opened as input, even if it
        // happens to exist on disk.
        assert_eq!(parse_file_arg(args), None);
    }

    #[test]
    fn equals_form_shots_is_skipped() {
        let args = vec!["--shots=/tmp/x.png".to_string()];
        assert_eq!(parse_file_arg(args), None);
    }

    #[test]
    fn nonexistent_positional_is_ignored() {
        let args = vec!["/no/such/file.bin".to_string()];
        assert_eq!(parse_file_arg(args), None);
    }
}
