//! Authenticator — a 2FA TOTP authenticator for qOS, built on the
//! CRDT-native `ui` toolkit.
//!
//! # What it does
//! A searchable list of accounts, each rendering a large rotating TOTP
//! code (6 or 8 digits) and a circular countdown ring showing seconds
//! left in the current period. Click a code to copy it; reorder or
//! delete accounts inline. Add accounts manually (issuer, label, base32
//! secret, algorithm, digits, period) or by pasting an `otpauth://` URI.
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
pub mod ring;
pub mod storage;
pub mod totp;

pub use app::AuthenticatorApp;
pub use model::{Account, Algorithm, Vault};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(AuthenticatorApp::new())
}
