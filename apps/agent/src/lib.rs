//! `qos-agent` — a participant without a window.
//!
//! The agent joins a room the way another person's session would: it
//! connects to the compositor's collaboration broker, announces a route,
//! asks the room for the current state, and from then on holds live
//! replicas of the documents the room's apps are sharing. It never sees a
//! pixel and never touches a pointer. When a document contains an
//! instruction addressed to it — a paragraph that starts with its name,
//! `Claude: turn the bullets into a table` — it acts, under its own actor,
//! so every change is attributed to it in the history strip and its caret
//! shows up in the other participants' windows while it works.
//!
//! ## What it can act on
//!
//! Whatever the room's apps are publishing. An app on the qOS action model
//! exports a resource tree, and `qos_actions::door` turns a live tree into
//! addressed descriptors and performs one of them attributed to the caller.
//! `surface` asks every app the agent holds a document for, every time, so
//! the reachable set is what is open at that moment rather than a list kept
//! in this crate. A second app becomes reachable by naming its contract, not
//! by writing down what it can do.
//!
//! On top of that sit a few named editorial `plan::RECIPES` — compositions
//! of several word-processor operations over blocks chosen by reading the
//! document. No app exports those as single operations and a survey cannot
//! discover a composition, so they stay named; they are offered alongside
//! the surveyed surface, never instead of it.
//!
//! ## How it decides, and what it refuses
//!
//! A model (`model`) is shown the survey and answers with one address. That
//! answer is untrusted: it is checked against a **freshly taken** survey
//! before anything is dispatched, and the door resolves the address against
//! a tree rebuilt at that instant, so an operation that has gone away is a
//! refusal rather than a substitution. Answering "nothing fits" is a
//! legitimate outcome and the agent says so in the document.
//!
//! With no model reachable a deterministic floor decides instead: the
//! instruction's own words matched against the words the apps put on their
//! operations, insisting on a clear winner. It refuses ambiguity rather than
//! picking, and it never invents an argument. Which path ran is recorded and
//! logged, so a run on the floor is never indistinguishable from a live one.
//!
//! Every operation is capability-gated against `ui::permissions`, twice: one
//! the agent may not perform never becomes an offer, and the question is
//! asked again at dispatch.
//!
//! The point is the shape: anything that can speak an app's actions can
//! drive that app, in the background, without competing with anyone for the
//! screen.

pub mod apply;
pub mod listen;
pub mod model;
pub mod plan;
pub mod session;
pub mod surface;
/// The named tools the decision is made through: what applications are
/// here, what may be done to a thing or a kind of thing, what one
/// operation takes, and the plan that answers.
pub mod tools;
pub mod wire;

pub use listen::{Address, LastAction, Subject, Summons};
pub use session::{Config, Session};
pub use surface::{AppContract, Offer, View};
