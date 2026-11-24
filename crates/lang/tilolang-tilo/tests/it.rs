//! The platform integration without a window: the resources of tilolang apps in the device's
//! store, the published surface, and replicas of one workspace running the real compiled
//! packages.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::needless_pass_by_value,
    reason = "test assertions"
)]

mod action_ids;
mod agent_queries;
mod asking_form;
mod atomic;
mod block_search;
mod board_lists;
mod bundle;
mod capture;
mod chained;
mod chat;
mod chat_persona_shot;
mod composer;
mod contacts;
mod containers;
mod discussion;
mod discord_migration;
mod disks;
mod done_days;
mod engines;
mod feedback_picker;
mod files;
mod firewall;
mod fixtures;
mod foreign;
mod gallery;
mod headings;
mod held_records;
mod inline_views;
mod layers;
mod legibility;
mod levels;
mod library;
mod link_editor;
mod links;
mod logs;
mod mail_recipients;
mod make_from;
mod marks;
mod may_read;
mod migrations;
mod monitor;
mod naming;
mod native_chat;
mod native_chat_skills;
mod native_mail;
mod native_notes;
mod note_import;
mod notebook_order;
mod notes;
mod notes_checklist;
mod opened;
mod opens_page;
mod outbox;
mod outcomes;
mod policy;
mod print_centre;
mod publication;
mod reach;
mod read;
mod readers;
mod reflection;
mod relatives;
mod removed;
mod resource_store;
mod rich_text;
mod scenarios;
mod settings_panes;
mod suggest;
mod surface;
mod task;
mod task_api;
mod tasks;
mod tasks_devices;
mod tasks_features;
mod tasks_menu;
mod tasks_places;
mod tasks_time;
mod tasks_today;
mod triggers;
mod tube;
mod unions;
mod upgrade;
mod vision;
mod windows_apps;
mod words;
mod written_views;
