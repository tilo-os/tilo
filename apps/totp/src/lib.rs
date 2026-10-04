//! Authenticator — a 2FA TOTP authenticator for Tilo, built on the
//! CRDT-native `ui` toolkit.
//!
//! # What it does
//! A searchable list of accounts, each rendering a large rotating TOTP
//! code (6 or 8 digits) and a circular countdown ring showing seconds
//! left in the current period. Click a code to copy it; reorder or
//! delete accounts inline. Add accounts by importing a QR code from a
//! screenshot or photo, pasting an `otpauth://` setup link, or typing
//! the details by hand — with the cryptographic parameters tucked behind
//! an Advanced-options disclosure.
//!
//! # Architecture
//! * [`model`] — the [`model::Account`] / [`model::Vault`] data, the
//!   `otpauth://` URI parser, and the theme-derived account palette.
//! * [`resource`] — the [`Totp`] resource, its effects and the
//!   [`TotpShell`] app.
//! * [`totp`] — RFC 6238 code generation on the `RustCrypto` HMAC/SHA
//!   primitives, plus period timing for the ring.
//! * [`doc`] — the secret-free Automerge account-metadata mirror.
//! * [`ring`] — the custom [`ring::CountdownRing`] widget (arc → Vello).
//! * [`list`] — the [`list::AccountList`] main screen.
//! * [`add_panel`] — the [`add_panel::AddAccountPanel`] modal.
//! * [`qr`] — QR ingestion: pure-Rust, offline decode from an image file.
//! * [`app`] — the [`app::AuthenticatorApp`] surface that ties them together.
//! * `secrets` — each account's seed in this device's secret store.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
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
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod add_panel;
pub mod app;
pub mod doc;
pub mod i18n;
pub mod list;
pub mod model;
pub mod qr;
pub mod resource;
pub mod ring;
mod secrets;
pub mod totp;

#[cfg(test)]
pub(crate) mod test_events;

pub use app::AuthenticatorApp;
pub use model::{Account, Algorithm, Vault};

pub use resource::{Totp, TotpEffect, TotpShell};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<TotpShell>()
}
