//! Containers — a rootless-podman container & distrobox manager for Tilo.
//!
//! The app projects the local container engine (containers, images,
//! distroboxes) into a surface built on the `ui` toolkit: a navigation rail,
//! a master list with inline lifecycle actions, a tabbed detail view (Logs /
//! Stats / Inspect / Terminal), a create-container flow, and a distrobox
//! section. The new-container draft is the app's resource document; the
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
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
pub mod combobox;
pub mod doc;
pub mod draw;
pub mod model;
pub mod panels;
pub mod podman;
pub mod presentation;
pub mod resource;
pub mod search_provider;
pub mod terminal;

pub use app::ContainersApp;
pub use resource::{Containers, ContainersShell};
