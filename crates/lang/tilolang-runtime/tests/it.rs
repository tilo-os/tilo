#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary instead of one per file; #[path] keeps the files in place.

#[path = "common/mod.rs"]
mod common;

#[path = "abi_parity.rs"]
mod abi_parity;
#[path = "authority.rs"]
mod authority;
#[path = "boundaries.rs"]
mod boundaries;
#[path = "calendar.rs"]
mod calendar;
#[path = "clipboard.rs"]
mod clipboard;
#[path = "counter.rs"]
mod counter;
#[path = "e2e_abi.rs"]
mod e2e_abi;
#[path = "e2e_apps.rs"]
mod e2e_apps;
#[path = "e2e_blocks.rs"]
mod e2e_blocks;
#[path = "e2e_call_fn.rs"]
mod e2e_call_fn;
#[path = "e2e_cleanup.rs"]
mod e2e_cleanup;
#[path = "e2e_counter.rs"]
mod e2e_counter;
#[path = "e2e_enums.rs"]
mod e2e_enums;
#[path = "e2e_layout.rs"]
mod e2e_layout;
#[path = "e2e_mail_files.rs"]
mod e2e_mail_files;
#[path = "e2e_native.rs"]
mod e2e_native;
#[path = "e2e_native_ui.rs"]
mod e2e_native_ui;
#[path = "e2e_notes.rs"]
mod e2e_notes;
#[path = "e2e_std.rs"]
mod e2e_std;
#[path = "e2e_todo.rs"]
mod e2e_todo;
#[path = "effects.rs"]
mod effects;
#[path = "event_values.rs"]
mod event_values;
#[path = "filters.rs"]
mod filters;
#[path = "foreign_readings.rs"]
mod foreign_readings;
#[path = "held_records.rs"]
mod held_records;
#[path = "image.rs"]
mod image;
#[path = "interpolation.rs"]
mod interpolation;
#[path = "json.rs"]
mod json;
mod page;
#[path = "list.rs"]
mod list;
#[path = "members.rs"]
mod members;
#[path = "multi_select.rs"]
mod multi_select;
#[path = "names.rs"]
mod names;
mod nested_proposals;
#[path = "outcomes.rs"]
mod outcomes;
#[path = "outside.rs"]
mod outside;
#[path = "programs.rs"]
mod programs;
#[path = "props.rs"]
mod props;
#[path = "provenance.rs"]
mod provenance;
#[path = "published_actions.rs"]
mod published_actions;
#[path = "queries.rs"]
mod queries;
#[path = "random.rs"]
mod random;
#[path = "reaches.rs"]
mod reaches;
#[path = "rich_text.rs"]
mod rich_text;
#[path = "runs.rs"]
mod runs;
#[path = "styles.rs"]
mod styles;
#[path = "sync.rs"]
mod sync;
#[path = "text.rs"]
mod text;
#[path = "text_math.rs"]
mod text_math;
#[path = "time.rs"]
mod time;
#[path = "time_reads.rs"]
mod time_reads;
#[path = "view_state_seed.rs"]
mod view_state_seed;
#[path = "wall_clock.rs"]
mod wall_clock;
#[path = "web.rs"]
mod web;
#[path = "window_bar.rs"]
mod window_bar;
#[path = "words.rs"]
mod words;
#[path = "written_views.rs"]
mod written_views;
