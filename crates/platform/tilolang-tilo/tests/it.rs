//! The platform integration without a window: documents, replayable actions, the published
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
mod board_lists;
mod bundle;
mod capture;
mod chained;
mod chat;
#[cfg(feature = "document")]
mod chunk_documents;
mod chunks;
mod collab;
mod columns;
mod composer;
mod discussion;
#[cfg(feature = "document")]
mod document;
mod documents;
mod done_days;
mod engines;
mod fixtures;
mod foreign;
mod gallery;
mod headings;
#[cfg(feature = "document")]
mod history_views;
mod incremental;
mod index;
mod inline_views;
mod items;
mod legacy;
mod legibility;
mod library;
mod link_editor;
mod links;
mod make_from;
mod marks;
mod may_read;
#[cfg(feature = "document")]
mod mention_asks;
mod migrations;
mod naming;
mod native_chat;
mod native_mail;
mod native_notes;
mod note_import;
mod notebook_order;
mod notes;
mod notes_checklist;
mod notices;
mod outbox;
mod outcomes;
mod policy;
mod publication;
mod reflection;
mod relatives;
mod resource_service;
mod rich_text;
mod scenarios;
#[cfg(feature = "document")]
mod shell_document;
mod suggest;
mod surface;
mod sync;
mod task;
mod task_api;
mod tasks;
mod tasks_places;
mod tasks_time;
mod tasks_today;
mod triggers;
#[cfg(feature = "document")]
mod undo_own;
mod unions;
mod unsaved;
mod upgrade;
mod window;
mod words;
