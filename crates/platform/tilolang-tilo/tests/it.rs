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
mod atomic;
mod blobs;
mod bundle;
mod capture;
mod chained;
mod chat;
#[cfg(feature = "record")]
mod chunk_records;
mod chunks;
mod collab;
mod columns;
mod composer;
mod discussion;
mod done_days;
mod engines;
mod fixtures;
mod foreign;
mod headings;
#[cfg(feature = "record")]
mod history_views;
mod incremental;
mod index;
mod inline_views;
mod items;
mod lanes;
mod legacy;
mod legibility;
mod library;
mod link_editor;
mod links;
mod make_from;
mod marks;
mod naming;
mod note_import;
mod notebook_order;
mod notes;
mod notices;
mod outbox;
mod outcomes;
mod policy;
mod publication;
#[cfg(feature = "record")]
mod record;
mod records;
mod records_service;
mod reflection;
mod relatives;
mod rich_text;
mod scenarios;
#[cfg(feature = "record")]
mod shell_record;
mod suggest;
mod surface;
mod sync;
mod task;
mod task_door;
mod tasks;
mod tasks_places;
mod tasks_today;
mod triggers;
#[cfg(feature = "record")]
mod undo_own;
mod unions;
mod unsaved;
mod upgrade;
mod views_migration;
mod window;
mod words;
