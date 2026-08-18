//! Code — qOS native code editor / lightweight IDE.
//!
//! A file-tree sidebar, editor tabs, a syntax-highlighted code pane with line
//! numbers and a gutter, a command palette, a status bar, and a bottom panel
//! for problems/terminal. Buffer text lives in the shared Automerge document,
//! so edits are collaborative and undoable; colours come entirely from the OS
//! theme.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer.

pub mod actions;
mod app;
mod bottom_panel;
mod command_palette;
pub mod command_provider;
mod disk;
mod doc;
mod editor_view;
mod file_tree;
mod find;
mod format;
mod highlight;
mod lint;
/// The Language Server Protocol client. Public so the end-to-end test in
/// `tests/language_server.rs` can drive a real `rust-analyzer` through the same
/// entry points the app uses, rather than a copy of them.
pub mod lsp;
pub mod model;
pub mod search_provider;
mod sample_project;
mod search;
mod status_bar;
mod tab_bar;

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
/// src/main.rs` work, which is the whole point of being able to develop qOS
/// from inside qOS.
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
    fn a_bare_path_is_the_project_to_open() {
        assert_eq!(parse_path_arg(&args(&["."])), Some(PathBuf::from(".")));
        assert_eq!(
            parse_path_arg(&args(&["/home/me/qos"])),
            Some(PathBuf::from("/home/me/qos"))
        );
        assert_eq!(
            parse_path_arg(&args(&["shell/src/main.rs"])),
            Some(PathBuf::from("shell/src/main.rs"))
        );
    }

    #[test]
    fn the_explicit_flag_works_in_both_spellings() {
        assert_eq!(parse_path_arg(&args(&["--path", "/a"])), Some(PathBuf::from("/a")));
        assert_eq!(parse_path_arg(&args(&["--path=/b"])), Some(PathBuf::from("/b")));
    }

    #[test]
    fn no_arguments_means_no_project() {
        assert_eq!(parse_path_arg(&args(&[])), None);
    }

    #[test]
    fn a_flags_value_is_not_mistaken_for_a_path() {
        // `--shots out.png` must not open a project called "out.png".
        assert_eq!(parse_path_arg(&args(&["--shots", "out.png"])), None);
        assert_eq!(
            parse_path_arg(&args(&["--viewport", "800x600", "--sidebar-open"])),
            None
        );
        // …but a real path after them still resolves.
        assert_eq!(
            parse_path_arg(&args(&["--sidebar-open", "/a/b"])),
            Some(PathBuf::from("/a/b"))
        );
    }
}
