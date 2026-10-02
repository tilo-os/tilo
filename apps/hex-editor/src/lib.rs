//! Hex — a device-local hex editor for Tilo, built on the `ui` toolkit.
//!
//! Raw file bytes, paths, edits, and undo history remain on this device. The
//! app publishes canonical local resource operations so palette and agent
//! clients can drive the same editor without putting sensitive file contents
//! into Automerge or collaboration history.
//! The UI is a classic hex view — offset gutter, hex byte columns, and an
//! ASCII column with a cursor synced across both — plus a data inspector
//! that interprets the bytes at the cursor as every common scalar type,
//! search (hex pattern or text), and goto-offset.
//!
//! `app_root()` is the single integration point used by both the live
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
pub mod inspector;
pub mod model;
pub mod resource;
pub mod sample;
pub mod search;
pub mod view;
mod web_file;

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
    let want_empty = std::env::var("TILO_HEX_DEMO").as_deref() == Ok("empty");
    let view = if shots_mode && !want_empty {
        // Arm the transient floating status bubble so the single headless frame
        // captures the floating cursor read-out chip (it otherwise appears on a
        // change / hover and fades, so a still frame would miss it).
        let mut v = app::sample_view();
        match std::env::var("TILO_HEX_DEMO").as_deref() {
            Ok("search") => v.arm_search_for_shots(),
            Ok("error") => v.arm_error_for_shots(),
            _ => v.arm_status_bubble_for_shots(),
        }
        v
    } else {
        HexView::empty()
    };
    Box::new(view)
}

/// The app as every host mounts it: the live root and its actions.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.hex-editor", app_root)
        .component_actions(actions::register)
        .as_definition("Hex Editor")
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
    fn parse_file_arg_skips_shots_output_and_missing_paths() {
        let cases: &[(&str, &[&str])] = &[
            // The PATH after --shots must never be opened as input, even if it
            // happens to exist on disk.
            (
                "shots flag and value",
                &["--shots", "/tmp/whatever-out.png"],
            ),
            ("equals-form shots", &["--shots=/tmp/x.png"]),
            ("nonexistent positional", &["/no/such/file.bin"]),
        ];
        for (name, args) in cases {
            let args = args.iter().map(|a| (*a).to_string()).collect::<Vec<_>>();
            assert_eq!(parse_file_arg(args), None, "case {name}");
        }
    }
}
pub use resource::{HexEditor, HexEditorShell, launch_spec};
