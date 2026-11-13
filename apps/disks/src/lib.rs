//! Disks — the machine's drives, their partitions and the operations on them.

// Apps run isolated, so app-local arithmetic checks are relaxed; a fault in a shared crate reaches the session, so its lints stay.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "app-local arithmetic; see the note above"
)]
pub mod app;
pub mod locales;
pub mod map;
pub mod model;
mod offer;
pub mod ops;
pub mod probe;
pub mod resource;
pub mod view;

pub use app::DisksApp;
pub use model::{Drive, DriveKind, Partition, SmartInfo, Topology};
pub use resource::{Disks, DisksEffect, DisksShell};
