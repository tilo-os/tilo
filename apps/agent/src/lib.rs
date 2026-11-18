//! `tilo-agent` — a member without a window.
//!
//! The agent connects to the compositor's collaboration broker and answers
//! the turns a person's messages in its conversations start, under its own
//! actor, so every change is attributed to it. It never sees a pixel and
//! never touches a pointer.
//!
//! A turn is answered the way every agent in Tilo works: through the
//! workspace's MCP tools (`tilo_ls`, `tilo_read`, `tilo_do`, `tilo_find`,
//! `tilo_view`, …), served here over the session's action API on the agent's
//! own connection, which the broker decides as the turn's
//! ([`tilo_agent_turn`]). Beside them the agent searches and reads the web
//! with its own two tools. What the turn does, and what it says, is written
//! in the conversation that asked (`session::conversation`).
//!
//! It also answers the AI transforms the workspace's shortcuts and apps ask
//! of it ([`transform`]).

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

/// The agent's catalogue of the sentences it writes into a conversation.
pub mod i18n;
pub mod model;
pub mod providers;
pub mod session;
pub mod transform;
pub mod wire;

pub use session::{Config, Session};
