//! `qos-agent` — a participant without a window.
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

pub mod apply;
pub mod listen;
pub mod model;
pub mod plan;
pub mod session;
pub mod surface;
pub mod tools;
pub mod wire;

pub use session::{Config, Session};
