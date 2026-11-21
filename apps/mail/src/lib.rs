//! Mail: Tilo's email client, on the CRDT-native `ui` toolkit.
//!
//! The window shell's three columns are a sidebar of accounts and folders (`sidebar.rs`), the
//! message list threaded by conversation (`list.rs`) and the reader (`reader.rs`); a compose
//! window and an account sheet layer on top.
//!
//! Per-message flags, the reader's folders and the active draft live in the mailbox resource
//! ([`resource::Mail`]), so they sync and undo. Account identities, endpoints and credentials
//! are device-local: the shared account store and the encrypted vault. The message corpus is
//! hydrated into `mail_engine::model::Mailbox`.
//!
//! Only a live window reaches a mail server; a screenshot renders the bundled sample mailbox
//! (`sample.rs`) as its fixture, and a past version in History starts no engine. The engine
//! itself, and the model it syncs, are `mail-engine`'s: this app draws them.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
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
pub(crate) mod address_book;
pub mod app;
pub mod avatars;
pub mod bindings;
pub mod compose;
pub mod copies;
pub mod held;
pub mod html_mail;
pub mod html_text;
pub mod i18n;
pub mod list;
mod mailbox_migration;
pub mod message_states;
pub mod notice;
pub mod reader;
pub mod resource;
#[cfg(all(feature = "live", any(test, feature = "fake-servers")))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test fixture: what fails fails the test that set it up"
)]
pub mod review_fixture;
#[cfg(feature = "samples")]
pub mod sample;

pub mod search;
pub mod shortcuts_overlay;
pub mod sidebar;
pub mod state;
pub mod trusted_senders;
pub mod util;
pub mod window_bar;

pub use app::MailApp;
pub use resource::{Mail, MailShell};

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(MailShell)
}
