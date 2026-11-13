//! Git — a graphical git client for Tilo, built on the `ui` toolkit. The app
//! presents three screens over a repository: **Changes** (staged, unstaged and
//! untracked lists, a syntax-highlighted diff pane with per-hunk staging, and a
//! commit box), **History** (a commit graph with lanes, and each commit's message,
//! refs and changed files) and **Branches** (local and remote branches, tags,
//! stashes and remotes, with fetch, pull, push and tracking). The live binary reads
//! a real repository through [`repo`]; screenshots use the bundled [`sample`]. Git
//! mutations are Effects, durable state is view preferences plus a repository
//! projection, and working-tree status stays live.

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
#[cfg(feature = "samples")]
pub mod sample;
pub mod settings;
pub mod shared;
mod ssh_transport;
mod transport;

pub use app::{GitClientApp, Screen};

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(GitClientShell)
}

#[cfg(feature = "samples")]
pub use resource::SCENARIOS;
pub use resource::{GitClient, GitClientShell};
