//! Code — Tilo native code editor / lightweight IDE.
//!
//! A file-tree sidebar, editor tabs, a syntax-highlighted code pane with line
//! numbers and a gutter, a command palette, a status bar, and a bottom panel
//! for problems/terminal. Buffer text lives in the shared Automerge document,
//! so edits are collaborative and undoable; colours come entirely from the OS
//! theme.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer.

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
mod app;
mod bottom_panel;
mod command_palette;
mod disk;
mod doc;
mod editor_view;
mod explorer;
mod find;
mod highlight;
mod lint;
/// The Language Server Protocol client. Public so the end-to-end test in
/// `tests/language_server.rs` can drive a real `rust-analyzer` through the same
/// entry points the app uses, rather than a copy of them.
pub mod lsp;
pub mod model;
pub mod native_fixture;
pub mod resource;
mod sample_project;
mod search;
pub mod search_provider;
mod status_bar;

use std::path::{Path, PathBuf};

use ui::widget::Widget;

pub use app::CodeApp;

/// Build the app's root widget tree over whatever the command line named.
///
/// With no path the editor opens **empty**. That is the honest state for an
/// editor nobody has given a project to: the alternative — the bundled demo
/// tree — puts files on screen that do not exist on this machine, which is a
/// lie the moment anyone tries to save one. The demo survives behind
/// [`sample_root`] for `--shots`.
pub fn app_root_at(path: Option<&Path>) -> Box<dyn Widget> {
    match path {
        Some(p) => Box::new(app::CodeApp::open_path(p)),
        None => Box::new(app::CodeApp::new()),
    }
}

/// The app's root with no project, for hosts that pass no path.
pub fn app_root() -> Box<dyn Widget> {
    app_root_at(None)
}

/// The app's root over the bundled demo project. `--shots` and the demo only.
pub fn sample_root() -> Box<dyn Widget> {
    Box::new(app::CodeApp::with_sample())
}

/// Parse the path the editor was asked to open: a bare positional argument, or
/// `--path PATH` / `--path=PATH`.
///
/// A positional argument is what makes `code_editor .` and `code_editor
/// src/main.rs` work, which is the whole point of being able to develop Tilo
/// from inside Tilo.
pub fn parse_path_arg(args: &[String]) -> Option<PathBuf> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--path" {
            return iter.next().map(PathBuf::from);
        }
        if let Some(rest) = arg.strip_prefix("--path=") {
            return Some(PathBuf::from(rest));
        }
        // Skip the flags that take a value, so their value is not mistaken for
        // a path to open.
        if matches!(
            arg.as_str(),
            "--shots" | "--viewport" | "--scenario" | "--headless-drive"
        ) {
            iter.next();
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(PathBuf::from(arg));
    }
    None
}

pub use resource::{CodeEditor, CodeEditorShell};

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        std::iter::once("code_editor")
            .chain(list.iter().copied())
            .map(String::from)
            .collect()
    }

    #[test]
    fn parse_path_arg_resolves_the_project_path() {
        let cases: &[(&str, &[&str], Option<&str>)] = &[
            // A bare path is the project to open.
            ("bare dot", &["."], Some(".")),
            ("bare absolute", &["/home/me/tilo"], Some("/home/me/tilo")),
            (
                "bare relative file",
                &["crates/session/shell/src/main.rs"],
                Some("crates/session/shell/src/main.rs"),
            ),
            // The explicit flag works in both spellings.
            ("--path separate value", &["--path", "/a"], Some("/a")),
            ("--path= joined value", &["--path=/b"], Some("/b")),
            // No arguments means no project.
            ("no arguments", &[], None),
            // A flag's value is not mistaken for a path: `--shots out.png`
            // must not open a project called "out.png".
            ("--shots value", &["--shots", "out.png"], None),
            (
                "--viewport value then bare flag",
                &["--viewport", "800x600", "--sidebar-open"],
                None,
            ),
            // …but a real path after them still resolves.
            (
                "path after bare flag",
                &["--sidebar-open", "/a/b"],
                Some("/a/b"),
            ),
        ];
        for (name, list, want) in cases {
            assert_eq!(
                parse_path_arg(&args(list)),
                want.map(PathBuf::from),
                "case {name}"
            );
        }
    }
}
