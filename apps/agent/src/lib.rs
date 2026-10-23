//! `tilo-agent` — a participant without a window.
//!
//! The agent connects to the compositor's collaboration broker and carries
//! out the runs of issues assigned to it (`--task-runs`) under its own actor, so every
//! change is attributed to it. It never sees a pixel and never touches a
//! pointer.
//!
//! *Which* published operations answer a request is decided by a model
//! (`model`) that discovers, invokes and observes the live, permission-filtered
//! catalogue the applications publish — the same one a person reaches from
//! the palette — through a handful of generic tools (`tools`). There are no
//! agent-only recipes: every id it names is checked against what a tool
//! showed it and against the catalogue again at dispatch, and the broker
//! authorizes each step. Model tasks run that as a step-by-step loop
//! (`session::agent_loop`). Without a model, a deterministic word match over
//! the same live catalogue decides, and which path ran is recorded.
//!
//! The point is the shape, not the vocabulary: anything that can speak an
//! app's actions can drive that app, in the background, without competing
//! with anyone for the screen.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::panic_in_result_fn,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

// Reuse the production-broker integration fixture in private Session tests.
#[cfg(test)]
extern crate self as tilo_agent;

pub mod model;
pub mod pages;
pub mod plan;
pub mod providers;
pub mod session;
pub mod surface;
pub mod tools;
pub mod transform;
pub mod wire;

pub use session::{Config, Session};
