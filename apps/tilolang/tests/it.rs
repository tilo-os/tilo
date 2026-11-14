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
mod history_window;

#[path = "board.rs"]
mod board;
#[path = "forms.rs"]
mod forms;
#[path = "interaction.rs"]
mod interaction;
#[path = "mentions.rs"]
mod mentions;
#[path = "motion.rs"]
mod motion;
#[path = "narrow.rs"]
mod narrow;
mod settle;
mod support;
#[path = "views.rs"]
mod views;

#[path = "hosted.rs"]
mod hosted;
