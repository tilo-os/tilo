//! Mail — Tilo native email client, built on the CRDT-native `ui` toolkit.
//!
//! ## Layout
//! Three panes: a folder/account **sidebar** (Inbox/Starred/Sent/Drafts/
//! Archive/Junk/Trash with unread counts), a **message list** threaded by
//! conversation (sender · subject · snippet · date · read/star/attachment
//! indicators), and a **reader** (headers · body · attachments). A modal
//! **Compose** window (To/Cc/Bcc · subject · body · attachments · send /
//! save-draft) and an **Account-setup** form (IMAP/SMTP host/port/TLS/user)
//! layer on top.
//!
//! ## Protocols
//! * Incoming MIME is parsed with `mail-parser` (`transport::parse_eml`).
//! * Outgoing MIME is built with `mail-builder` (`transport::build_mime`).
//! * The live app drives IMAP (`async-imap`) and SMTP (`lettre`) on a worker
//!   thread; see `transport.rs` for the seam. Those TLS/async stacks are kept
//!   out of the headless build so it stays small and green.
//!
//! ## State
//! Per-message flags (read/starred/folder), selected folder/view state, and
//! the active draft live in the Automerge document via `doc::MailDoc`, so they
//! sync and undo. Account identities, connection endpoints, and credentials
//! are device-local in the shared Tilo account store + encrypted vault. The
//! immutable message corpus is hydrated into `model::Mailbox`.
//!
//! ## Screenshots and history
//! Only a live window reaches a mail server. A screenshot scenario renders the
//! bundled sample mailbox (`sample.rs`: threaded messages with bodies and
//! attachments, two of them hydrated through the `mail-parser` path) and sample
//! accounts as its fixture, never the person's accounts; a past version in
//! History starts no engine.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
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
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod account;
pub mod actions;
pub mod app;
pub mod attachment_store;
pub mod autoconfig;
pub mod compose;
pub mod doc;
pub mod draw;
pub mod engine;
pub mod html_mail;
pub mod i18n;
pub mod model;
pub mod oauth;
pub mod pane;
pub mod resource;
pub mod sample;
pub mod search_provider;
pub mod shortcuts_overlay;
pub mod state;
pub mod transport;
pub mod util;
pub mod validate;

pub use app::MailApp;
pub use resource::{Mail, MailShell};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<MailShell>()
}
