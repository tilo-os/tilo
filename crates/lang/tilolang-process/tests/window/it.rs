//! The runtime as the framework runs it, with a package installed: the real component runtime,
//! storage, permissions, history and api, without a window on a screen.
//!
//! One test binary instead of one per file: a file per binary linked the whole crate once each.
//! `#[path]` keeps the files where they are, `support` below is the one copy they share, and
//! nextest runs each test in its own process — the one package a process installs is still one.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

/// What the process holds allocated is metered, so a test bounds what an operation keeps by the
/// bytes it holds, not by pages the allocator has yet to hand back.
#[global_allocator]
static ALLOCATOR: tilo_test_env::allocation::Meter = tilo_test_env::allocation::Meter;

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
#[path = "gallery_close.rs"]
mod gallery_close;
#[path = "gallery_frames.rs"]
mod gallery_frames;
#[path = "gallery_html.rs"]
mod gallery_html;
#[path = "gallery_pages.rs"]
mod gallery_pages;
#[path = "gallery_sidebar.rs"]
mod gallery_sidebar;
#[path = "gallery_start.rs"]
mod gallery_start;
#[path = "interaction.rs"]
mod interaction;
#[path = "mentions.rs"]
mod mentions;
#[path = "motion.rs"]
mod motion;
#[path = "narrow.rs"]
mod narrow;
#[path = "open_by.rs"]
mod open_by;
mod settle;
mod support;
#[path = "video_decode.rs"]
mod video_decode;
#[path = "views.rs"]
mod views;

#[path = "hosted.rs"]
mod hosted;
