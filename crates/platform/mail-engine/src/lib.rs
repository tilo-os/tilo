//! Mail's model and its engine, without a window: the accounts, folders and messages a mailbox
//! holds ([`model`]), what one folder's sync asks of its server and brings back ([`sync`]), and
//! the worker that reaches IMAP and SMTP servers off the event loop, keeping the mailbox in step
//! ([`engine`]) through Tilo's one mail client (`tilo-mail-session`).
//!
//! The Mail app draws these values; the tilolang Mail origin runs the same engine for a hosted
//! instance. Account identities, endpoints and credentials are device-local: the shared account
//! store ([`account`]) and the encrypted vault, never the mailbox document.

// App-local arithmetic checks are relaxed: this model's arithmetic is on message sizes and
// timestamps, and a wrap is the value an IMAP UID means.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "mailbox arithmetic; see the note above"
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

pub mod account;
pub mod attachment_store;
pub mod engine;
pub mod model;
pub mod oauth;
pub mod store;
pub mod sync;
pub mod transport;
