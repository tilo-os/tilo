//! Containers — a rootless-podman container & distrobox manager for Tilo.
//!
//! The app projects the local container engine (containers, images,
//! volumes, distroboxes) into a window built from the `ui` toolkit: the
//! sections in a sidebar, the container list beside a tabbed detail (logs,
//! usage, settings, shell), a new-container form, and a page per section.
//! The new-container draft is the app's resource document; the
//! engine snapshot is transient host data mirrored from `podman`, published
//! as live rows.
//!
//! [`ContainersShell`] is the app: the live window, `--shots` and every other
//! host run it through the framework.

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
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
mod create_form;
pub mod doc;
pub mod model;
pub mod podman;
pub mod resource;
pub mod search;
pub mod terminal;
mod view;

pub use app::ContainersApp;
pub use resource::{Containers, ContainersShell};
