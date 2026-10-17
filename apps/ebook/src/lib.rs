//! Books — Tilo native EPUB reader (CRDT-native, built on the `ui` toolkit).
//!
//! Layered so each piece is testable in isolation: [`model`] (pure data +
//! the shared annotation model reused by the PDF reader/word processor),
//! [`doc`] (the Automerge document layer), [`xhtml`]/[`epub`]/[`content`]
//! (EPUB parsing + library), [`paginate`] (text layout), [`reader_theme`]/
//! [`paint`] (palettes + drawing), and [`app`] (the retained-mode
//! [`Widget`](ui::widget::Widget) composing the shelf and reader).
//!
//! [`EbookShell`] is the app: every host mounts its [`definition`], and the
//! binary runs it.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
mod comment_overlay;
pub mod component;
pub mod content;
pub mod epub;
mod hosted_dropdown;
mod input_controls;
mod local_library;
pub mod model;
pub mod paginate;
pub mod paint;
mod reader_bubble;
mod reader_keys;
pub mod reader_theme;
mod reading_settings;
pub mod resource;
mod scenarios;
pub mod search_provider;
mod shelf_controls;
#[cfg(test)]
mod test_epub;
pub mod xhtml;

pub use app::BooksApp;
pub use component::definition;
pub use resource::{BooksEffect, Ebook, EbookShell};
