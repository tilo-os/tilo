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

mod agent_queries;
mod bundle;
mod chat;
mod collab;
mod columns;
mod composer;
mod engines;
mod fixtures;
mod incremental;
mod index;
mod items;
mod lanes;
mod legacy;
mod outbox;
mod outcomes;
mod policy;
mod publication;
mod records;
mod reflection;
mod rich_text;
#[cfg(feature = "room")]
mod room;
mod scenarios;
mod surface;
mod sync;
mod task;
mod task_door;
mod tasks;
mod unions;
mod upgrade;
mod views_migration;
