//! Git — a graphical git client for Tilo, built on the `ui` toolkit.
//!
//! The app presents three screens over a repository:
//!
//! * **Changes** — staged / unstaged / untracked file lists with one-click
//!   stage and unstage, a syntax-highlighted diff pane (unified or
//!   side-by-side) with per-hunk staging, and a commit box.
//! * **History** — a commit graph with branch and merge lanes, author and
//!   date, plus a detail panel showing each commit's message, refs and
//!   changed files.
//! * **Branches & Remotes** — cards for branches, tags, stashes and
//!   remotes, with fetch / pull / push actions and ahead/behind tracking.
//!
//! The live binary reads a real repository through [`repo`] (libgit2 via
//! `git2`). When no repository is reachable it opens an empty repository
//! picker. The explicit `--shots` renderer uses a bundled [`sample`]
//! so the screenshot shows a fully populated state offline. All rendering
//! is theme-driven; durable UI preferences live in the CRDT via
//! [`settings`].
//!
//! Git mutations against the on-disk repository are Effects. Shared CRDT state
//! is durable view preferences plus a repository projection (branches, commits,
//! remotes); working-tree status stays live. See [`actions`] and [`resource`].

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

use ui::widget::Widget;

pub mod actions;
pub mod app;
pub mod changes;
pub mod diff;
pub mod diffview;
mod doc;
pub mod draw;
pub mod graph;
pub mod history;
mod http_transport;
pub mod model;
pub mod panels;
pub mod repo;
pub mod resource;
pub mod sample;
pub mod settings;
mod ssh_transport;
mod transport;

pub use app::{GitClientApp, Screen};

/// Build and return the app's root widget tree. This is the single
/// integration point used by both the live event loop and the `--shots`
/// offscreen renderer.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(GitClientApp::new())
}

/// Every `--scenario` name the screenshot renderer can stage. Declared to
/// `LaunchSpec::scenarios` so an unknown name is an error listing these.
pub const SCENARIOS: &[&str] = &[
    "changes", "split", "history", "branches", "commit", "discard", "empty",
];

/// Build the `--shots` root staged for `scenario` over the bundled sample
/// (or the honest empty state for `empty`). `None` is the default Changes
/// screen, the same frame `--shots` rendered before scenarios existed.
pub fn app_root_shot(scenario: Option<&str>) -> Box<dyn Widget> {
    if scenario == Some("empty") {
        return Box::new(GitClientApp::open_empty());
    }
    let mut app = GitClientApp::new();
    match scenario {
        Some("split") => app.shot_split_diff(),
        Some("history") => app.shot_screen(Screen::History),
        Some("branches") => app.shot_screen(Screen::Panels),
        Some("commit") => app.shot_commit_message("Flush the blur pass before presenting"),
        Some("discard") => app.shot_arm_discard(),
        _ => {}
    }
    Box::new(app)
}
#[cfg(target_arch = "wasm32")]
pub use resource::{GitClient, GitClientShell};
#[cfg(not(target_arch = "wasm32"))]
pub use resource::{GitClient, GitClientShell, launch_spec};
