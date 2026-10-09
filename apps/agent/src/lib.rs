//! `tilo-agent` — a participant without a window.
//!
//! The agent joins a word-processor document the way a second copy of the
//! app would: it connects to the compositor's collaboration broker,
//! announces the same route `(app, room, record)`, asks the room for the
//! current state, and from then on holds a live replica of the document.
//! It never sees a pixel and never touches a pointer. When the document
//! contains an instruction addressed to it — a paragraph that starts with
//! its name, `Claude: turn the bullets into a table` — it edits the
//! document with the word processor's own exported actions, under its own
//! actor, so every change is attributed to it in the history strip and its
//! caret shows up in the other participants' windows while it works.
//!
//! With `app_id = "so.tilo.chat"` and no record, the same Session reads the
//! canonical package-primary Chat document. Structured mentions target the
//! stable admitted `agent_id`; name addressing uses the display name. Replies
//! and `AgentWork` live in that document and conservatively prevent repeats
//! after restart, including an unfinished dispatch. This bounded mode only
//! executes broker-published document actions. External effects, pause/resume,
//! authenticated agent roster projection, and durable delegated effect
//! admission remain separate integrations.
//!
//! *Which* published operations answer a request is decided by a model
//! (`model`) that discovers, invokes and observes the live, permission-filtered
//! catalogue the applications publish — the same one a person reaches from
//! the palette — through a handful of generic tools (`tools`). There are no
//! agent-only recipes: every id it names is checked against what a tool
//! showed it and against the catalogue again at dispatch, and the broker
//! authorizes each step. Enrolled tasks run that as a step-by-step loop
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
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
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

pub mod apply;
pub mod listen;
pub mod live;
pub mod memory;
pub mod model;
pub mod plan;
pub mod providers;
pub mod session;
pub mod surface;
pub mod tools;
pub mod transform;
pub mod wire;

pub use session::{Config, Session};
