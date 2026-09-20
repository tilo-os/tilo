//! Canonical desktop model shared by native and portable hosts.
//!
//! Contains no identity discovery, persistence, or room admission. Native hosts
//! additionally use [`fs_authority`] to publish the canonical capability projection;
//! portable hosts never invoke that adapter.
//! Local view fields retain their legacy persisted positions; only an admitted
//! topology projection may supply shared state to a host.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "test assertions"
    )
)]

pub mod fs_authority;
pub mod identity;
mod local_view;
mod model;
mod navigation;
mod persistence;
mod shared_surfaces;
mod topology;
pub use model::*;
pub use shared_surfaces::SharedSurfaces;
pub use topology::TabMove;

pub mod backdrop;
pub mod chrome;
pub mod composition;
pub mod intents;
pub mod records;
pub mod resolver;
pub mod session_commands;
pub mod topbar;

pub mod projection;

extern crate self as qos_desktop;
pub mod activity;
