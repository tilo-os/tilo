//! Containers: a rootless-podman and distrobox manager. The window has a sidebar of
//! sections, a container list beside a tabbed detail, a new-container form and a
//! page per section. The draft is the app's record; the engine snapshot is host data
//! mirrored into live rows.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints stay denied; shared crates keep the full set.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "app-local arithmetic; see the note above"
)]
pub mod actions;
pub mod app;
mod create_form;
pub mod draft;
pub mod model;
pub mod podman;
pub mod resource;
pub mod search;
pub mod terminal;
mod view;

pub use app::ContainersApp;
pub use resource::{Containers, ContainersShell};
