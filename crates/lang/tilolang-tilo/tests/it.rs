//! The platform integration without a window: the resources of tilolang apps in the device's
//! store, the published surface, and replicas of one workspace running the real compiled
//! packages.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::needless_pass_by_value,
    clippy::doc_markdown,
    reason = "test assertions; YouTube is a name, not code"
)]

mod agent_queries;
mod atomic;
mod block_search;
mod board_lists;
mod bundle;
mod capture;
mod chained;
mod chat;
mod composer;
mod discussion;
mod done_days;
mod engines;
mod fixtures;
mod foreign;
mod gallery;
mod headings;
mod held_records;
mod inline_views;
mod legibility;
mod library;
mod link_editor;
mod links;
mod make_from;
mod marks;
mod may_read;
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
mod opened;
mod outbox;
mod outcomes;
mod policy;
mod publication;
mod readers;
mod reflection;
mod relatives;
mod removed;
mod resource_service;
mod resource_store;
mod rich_text;
mod scenarios;
mod share;
mod suggest;
mod surface;
mod task;
mod task_api;
mod tasks;
mod tasks_places;
mod tasks_time;
mod tasks_today;
mod triggers;
mod tube;
mod unions;
mod upgrade;
mod vision;
mod words;
