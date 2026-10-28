//! The platform integration without a window: records, replayable actions, the published
//! surface, and two replicas of one room running the real compiled packages.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::needless_pass_by_value,
    reason = "test assertions"
)]

mod collab;
mod columns;
mod engines;
mod fixtures;
mod incremental;
mod index;
mod outbox;
mod outcomes;
mod policy;
mod publication;
mod records;
mod reflection;
#[cfg(feature = "room")]
mod room;
mod scenarios;
mod surface;
mod sync;
mod upgrade;
