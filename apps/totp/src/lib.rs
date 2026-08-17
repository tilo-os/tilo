//! Authenticator — a 2FA TOTP authenticator for qOS, built on the
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
//! * [`totp`] — RFC 6238 code generation on the RustCrypto HMAC/SHA
//!   primitives, plus period timing for the ring.
//! * [`doc`] — the Automerge mirror of the vault (CRDT-native state).
//! * [`ring`] — the custom [`ring::CountdownRing`] widget (arc → Vello).
//! * [`list`] — the [`list::AccountList`] main screen.
//! * [`add_panel`] — the [`add_panel::AddAccountPanel`] modal.
//! * [`qr`] — QR ingestion: pure-Rust, offline decode from an image file.
//! * [`app`] — the [`app::AuthenticatorApp`] root that ties them together.
//! * [`storage`] — plaintext persistence today, with the documented
//!   encrypted-at-rest plan.
//!
//! [`app_root`] is the single integration point used by `main.rs` for
//! both the live event loop and the `--shots` offscreen renderer.

pub mod actions;
pub mod add_panel;
pub mod app;
pub mod doc;
pub mod list;
pub mod model;
pub mod portal;
pub mod qr;
pub mod ring;
pub mod search_provider;
pub mod storage;
pub mod totp;

#[cfg(test)]
pub(crate) mod test_events;

pub use app::AuthenticatorApp;
pub use model::{Account, Algorithm, Vault};

use ui::widget::Widget;

/// Build the app's root widget for the **live** session: it loads the
/// user's persisted vault and shows the empty state on a fresh install.
/// It never fabricates sample accounts.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(AuthenticatorApp::new())
}

/// Build the root widget seeded with a deterministic sample vault. Used by
/// the `--shots` renderer and demos so the frame is populated and
/// reproducible without touching (or leaking) the real on-disk vault.
pub fn app_root_demo() -> Box<dyn Widget> {
    Box::new(AuthenticatorApp::demo())
}

/// Build the root widget with no accounts — the first-run empty state. Used
/// by the `--shots` renderer to capture that experience deterministically.
pub fn app_root_empty() -> Box<dyn Widget> {
    Box::new(AuthenticatorApp::empty())
}

/// Build the sample-seeded root already showing the Add-account screen.
/// Used by the `--shots` renderer to capture the add flow.
pub fn app_root_add_screen() -> Box<dyn Widget> {
    let mut app = AuthenticatorApp::demo();
    app.seed_open_add();
    Box::new(app)
}

/// Build the sample-seeded root on the Add-account screen with Advanced
/// options expanded — the `--shots` renderer uses it to capture the tall,
/// scrolling form.
pub fn app_root_add_advanced_screen() -> Box<dyn Widget> {
    let mut app = AuthenticatorApp::demo();
    app.seed_open_add_advanced();
    Box::new(app)
}

/// Build the sample-seeded root on the Add-account screen with the
/// paste-a-link row disclosed — the `--shots` renderer uses it to capture the
/// expanded paste field reflowing the manual details below it (no overlap).
pub fn app_root_add_link_screen() -> Box<dyn Widget> {
    let mut app = AuthenticatorApp::demo();
    app.seed_open_add_link();
    Box::new(app)
}

/// Build the sample-seeded root on the Add-account screen — the entry point
/// the "Scan a QR image" import flow starts from. At runtime that action hands
/// off to the **host** file chooser via the XDG Desktop Portal (there is no
/// app-drawn picker to capture offscreen, per HIG §5), so this shot shows the
/// add screen with its import method cards; the host dialog itself is rendered
/// by the compositor's portal backend.
pub fn app_root_add_picker_screen() -> Box<dyn Widget> {
    let mut app = AuthenticatorApp::demo();
    app.seed_open_add_picker();
    Box::new(app)
}
