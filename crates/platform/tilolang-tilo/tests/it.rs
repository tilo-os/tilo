//! The platform integration without a window: records, replayable actions, the published
//! surface, and two replicas of one workspace running the real compiled packages.

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
mod chained;
mod chat;
mod collab;
mod columns;
mod composer;
mod engines;
mod fixtures;
mod foreign;
#[cfg(feature = "record")]
mod history_views;
mod incremental;
mod index;
mod inline_views;
mod items;
mod lanes;
mod legacy;
mod legibility;
mod marks;
mod naming;
mod outbox;
mod outcomes;
mod policy;
mod publication;
#[cfg(feature = "record")]
mod record;
mod records;
mod records_service;
mod reflection;
mod rich_text;
mod scenarios;
#[cfg(feature = "record")]
mod shell_record;
mod surface;
mod sync;
mod task;
mod task_door;
mod tasks;
#[cfg(feature = "record")]
mod undo_own;
mod unions;
mod unsaved;
mod upgrade;
mod views_migration;
mod words;
