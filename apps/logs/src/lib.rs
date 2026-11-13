//! Logs — a graphical systemd-journal viewer, built on the Tilo `ui` toolkit (CRDT-native,
//! retained-mode).

// Apps run isolated, so app-local arithmetic checks are relaxed; a fault in a shared crate reaches the session, so its lints stay.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
pub mod actions;
pub mod journal;
pub mod locales;
pub mod model;
pub mod resource;
mod view;
mod window;

pub use model::{Filter, LogEntry, Priority};
pub use resource::{Logs, LogsShell};
