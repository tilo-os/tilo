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
//! *Which* action answers an instruction is decided by a model (`model`),
//! from the catalogue of things the app exports (`plan::CATALOGUE`) and
//! nothing else: the model names one entry, that name is checked against the
//! exported set, and the app's own action carries it out. It never proposes a
//! document mutation of its own, and an answer outside the catalogue is
//! refused rather than acted on. When no key is present, or a call is slow or
//! fails, a keyword vocabulary decides instead — the run continues, and which
//! path ran is recorded and logged rather than left indistinguishable.
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
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

// Reuse the production-broker integration fixture in private Session tests.
#[cfg(test)]
extern crate self as qos_agent;
#[cfg(test)]
extern crate self as tilo_agent;

pub mod apply;
pub mod listen;
pub mod model;
pub mod plan;
pub mod providers;
pub mod session;
pub mod surface;
pub mod tools;
pub mod wire;

pub use session::{Config, Session};
