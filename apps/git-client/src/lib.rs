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
//! * **Branches** — sections for local and remote branches, tags, stashes
//!   and remotes, with fetch / pull / push and ahead/behind tracking.
//!
//! The live binary reads a real repository through [`repo`] (libgit2 via
//! `git2`): the one the record opens or an explicitly selected launch path.
//! Until a repository is selected, it opens an empty repository picker. Screenshots use a bundled [`sample`] so they
//! show a fully populated state offline. All rendering is theme-driven;
//! durable UI preferences live in the CRDT via [`settings`].
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
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
pub mod changes;
pub mod diff;
pub mod diffview;
pub mod graph;
pub mod history;
mod http_transport;
pub mod i18n;
pub mod model;
pub mod panels;
pub mod repo;
pub mod resource;
pub mod sample;
pub mod settings;
pub mod shared;
mod ssh_transport;
mod transport;

pub use app::{GitClientApp, Screen};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(GitClientShell)
}

pub use resource::{GitClient, GitClientShell, SCENARIOS};
