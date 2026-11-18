//! Git client's resources: the repository a client opens and what members see
//! of it — its branches, history, remotes, tags, stashes and identity — as the
//! app that draws them and an agent that reads them both see them.
//!
//! Reading a repository with `git2`, the working tree and transfers are the
//! app's; only the shared projection travels in the workspace.

// Graph widths and line counts are bounded by what a commit graph and a diff
// show; the arithmetic on them cannot overflow an address. Panic-producing
// lints stay denied.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "bounded graph and diff arithmetic"
)]

pub mod model;
mod repo;
pub mod shared;

pub use repo::GitClient;
