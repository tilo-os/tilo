//! Authenticator: a TOTP account list with rotating codes, a countdown ring,
//! and add-by-QR, add-by-link and manual entry. Seeds stay in this device's
//! secret store; only secret-free account metadata is shared.

// An app runs as its own sandboxed process: a crash costs one app, not the
// desktop, so wrapping arithmetic and casts are allowed here. The panic lints
// stay denied.
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

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(TotpShell)
}
