//! `qos-agent` — a participant without a window.
//!
//! The agent joins a word-processor document the way a second copy of the
//! app would: it connects to the compositor's collaboration broker,
//! announces the same route `(app, room, record)`, asks the room for the
//! current state, and from then on holds a live replica of the document.
//! It never sees a pixel and never touches a pointer. When the document
//! contains an instruction addressed to it — a paragraph that starts with
//! its name, `Scribe: turn the bullets into a table` — it edits the
//! document with the word processor's own exported actions, under its own
//! actor, so every change is attributed to it in the history strip and its
//! caret shows up in the other participants' windows while it works.
//!
//! What it can do today is scripted (`plan`): a small vocabulary a demo can
//! rely on. The point is the shape, not the vocabulary: anything that can
//! speak an app's actions can drive that app, in the background, without
//! competing with anyone for the screen.

pub mod apply;
pub mod plan;
pub mod session;
pub mod wire;

pub use session::{Config, Session};
