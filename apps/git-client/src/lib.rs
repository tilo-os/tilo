//! Git — a graphical git client for qOS, built on the `ui` toolkit.
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
//! `git2`). When no repository is reachable — notably the headless
//! `--shots` renderer — it falls back to a bundled, realistic [`sample`]
//! so the screenshot shows a fully populated state offline. All rendering
//! is theme-driven; durable UI preferences live in the CRDT via
//! [`settings`].
//!
//! Every git operation is **external** — it mutates the on-disk repository
//! through libgit2, not a shared document. The client's only shared-CRDT state
//! is those durable view preferences, and it exports each as a replayable
//! [`actions::GitClientAction`] so the shell's Ctrl+K palette and the on-screen
//! controls dispatch one and the same action. See [`actions`] for the split.

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

use ui::widget::Widget;

pub mod actions;
pub mod app;
pub mod changes;
pub mod diff;
pub mod diffview;
pub mod draw;
pub mod graph;
pub mod history;
pub mod model;
pub mod panels;
pub mod repo;
pub mod sample;
pub mod settings;

pub use app::GitClientApp;

/// Build and return the app's root widget tree. This is the single
/// integration point used by both the live event loop and the `--shots`
/// offscreen renderer.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(GitClientApp::new())
}
