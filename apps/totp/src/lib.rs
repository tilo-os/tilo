//! Authenticator — a 2FA TOTP authenticator for qOS, built on the
//! CRDT-native `ui` toolkit.
//!
//! # What it does
//! A searchable list of accounts, each rendering a large rotating TOTP
//! code (6 or 8 digits) and a circular countdown ring showing seconds
//! left in the current period. Click a code to copy it; reorder or
//! delete accounts inline. Add accounts by scanning a QR code, importing
//! one from a screenshot, pasting an `otpauth://` setup link, or typing
//! the details by hand — with the cryptographic parameters tucked behind
//! an Advanced-options disclosure.
//!
//! # Architecture
//! * [`model`] — the [`model::Account`] / [`model::Vault`] data, the
//!   `otpauth://` URI parser, and the theme-derived account palette.
//! * [`totp`] — RFC 6238 code generation on the RustCrypto HMAC/SHA
//!   primitives, plus period timing for the ring.
//! * [`doc`] — the Automerge mirror of the vault (CRDT-native state).
//! * [`ring`] — the custom [`ring::CountdownRing`] widget (arc → Vello).
//! * [`list`] — the [`list::AccountList`] main screen.
//! * [`add_panel`] — the [`add_panel::AddAccountPanel`] modal.
//! * [`qr`] — QR ingestion: pure-Rust decode from an image file today,
//!   plus the clean hook for the pending shared camera-capture pipeline.
//! * [`app`] — the [`app::AuthenticatorApp`] root that ties them together.
//! * [`storage`] — plaintext persistence today, with the documented
//!   encrypted-at-rest plan.
//!
//! [`app_root`] is the single integration point used by `main.rs` for
//! both the live event loop and the `--shots` offscreen renderer.

pub mod add_panel;
pub mod app;
pub mod doc;
pub mod list;
pub mod model;
pub mod qr;
pub mod ring;
pub mod storage;
pub mod totp;

pub use app::AuthenticatorApp;
pub use model::{Account, Algorithm, Vault};

use ui::widget::Widget;

/// Build and return the app's root widget tree (the account list screen).
pub fn app_root() -> Box<dyn Widget> {
    Box::new(AuthenticatorApp::new())
}

/// Build the root widget already showing the Add-account screen. Used by
/// the `--shots` renderer to capture the add flow, and available to any
/// caller that wants to land directly on it.
pub fn app_root_add_screen() -> Box<dyn Widget> {
    let mut app = AuthenticatorApp::new();
    app.seed_open_add();
    Box::new(app)
}
