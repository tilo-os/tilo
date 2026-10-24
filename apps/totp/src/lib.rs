//! Authenticator — a 2FA TOTP authenticator for Tilo, built on the
//! CRDT-native `ui` toolkit.
//!
//! # What it does
//! A list of accounts, each rendering a rotating TOTP code (6 or 8 digits)
//! and a countdown ring showing the seconds left in the current period.
//! Click a code to copy it; reorder, edit or delete accounts from the row's
//! hover actions. Add accounts by importing a QR code from a screenshot or
//! photo, pasting an `otpauth://` setup link, or typing the details by hand,
//! with the cryptographic parameters tucked behind Advanced options. Finding
//! an account is the platform search's job.
//!
//! # Architecture
//! * [`model`] — the [`model::Account`] / [`model::Vault`] data, the
//!   `otpauth://` URI parser, and the account colour slots.
//! * [`totp`] — RFC 6238 code generation on the `RustCrypto` HMAC/SHA
//!   primitives, plus period timing for the countdown ring.
//! * `doc` — the secret-free Automerge account-metadata mirror.
//! * [`list`] — the [`list::AccountList`] main screen.
//! * [`add_panel`] — the [`add_panel::AddAccountPanel`] sheet.
//! * [`qr`] — QR ingestion: pure-Rust, offline decode from an image file.
//! * [`app`] — the [`app::AuthenticatorApp`] surface that ties them
//!   together.
//! * [`resource`] — the [`Totp`] resource, its effects and the
//!   [`TotpShell`] app: the navigation shell with its header row around the
//!   surface.
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
pub mod add_panel;
pub mod app;
pub mod i18n;
pub mod list;
pub mod model;
pub mod qr;
pub mod resource;
mod secrets;
pub mod totp;

#[cfg(test)]
pub(crate) mod test_events;

pub use app::AuthenticatorApp;
pub use model::{Account, Algorithm, Vault};

pub use resource::{Totp, TotpEffect, TotpShell};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(TotpShell)
}
