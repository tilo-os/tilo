//! The runtime as the framework runs it, with a package installed: the real component runtime,
//! storage, permissions, history and door, without a window on a screen.
//!
//! One test binary instead of one per file: a file per binary linked the whole crate once each.
//! `#[path]` keeps the files where they are, `support` below is the one copy they share, and
//! nextest runs each test in its own process — the one package a process installs is still one.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]

#[allow(
    dead_code,
    reason = "each test reads what it needs of the change marks"
)]
mod change_marks;
mod door;
mod history;
mod history_window;
mod marks;
mod narrow;
mod opening;
mod settle;
mod support;
mod sync;
mod workspaces;

#[path = "board.rs"]
mod board;
#[path = "counter.rs"]
mod counter;
#[path = "forms.rs"]
mod forms;
#[path = "history_undo.rs"]
mod history_undo;
#[path = "hosted.rs"]
mod hosted;
#[path = "interaction.rs"]
mod interaction;
#[path = "mentions.rs"]
mod mentions;
#[path = "motion.rs"]
mod motion;
#[path = "tasks_board.rs"]
mod tasks_board;
#[path = "views.rs"]
mod views;
