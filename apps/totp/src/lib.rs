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
//! * [`totp`] — RFC 6238 code generation on the `RustCrypto` HMAC/SHA
//!   primitives, plus period timing for the ring.
//! * [`doc`] — the secret-free Automerge account-metadata mirror.
//! * [`ring`] — the custom [`ring::CountdownRing`] widget (arc → Vello).
//! * [`list`] — the [`list::AccountList`] main screen.
//! * [`add_panel`] — the [`add_panel::AddAccountPanel`] modal.
//! * [`qr`] — QR ingestion: pure-Rust, offline decode from an image file.
//! * [`app`] — the [`app::AuthenticatorApp`] root that ties them together.
//! * [`storage`] — encrypted local vault persistence with explicit durability
//!   status.
//!
//! [`app_root`] is the single integration point used by `main.rs` for
//! both the live event loop and the `--shots` offscreen renderer.

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

/// The app as every host mounts it: the live root and its actions.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.totp", app_root)
        .component_actions(actions::register)
        .as_definition("TOTP")
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

/// Deterministic secret-free shared-account state for visual/a11y review of
/// the local provisioning flow. No real vault material is read or rendered.
pub fn app_root_provision_screen() -> Box<dyn Widget> {
    Box::new(AuthenticatorApp::demo_provision())
}

/// Deterministic durability-warning state for visual review. It contains only
/// demo accounts and no real keyring/storage failure details.
pub fn app_root_persistence_error() -> Box<dyn Widget> {
    Box::new(AuthenticatorApp::demo_persistence_error())
}

/// Deterministic shared-account preview with one account lacking this
/// device's secret, used to review the honest setup affordance.
pub fn app_root_missing_secret() -> Box<dyn Widget> {
    Box::new(AuthenticatorApp::demo_missing_secret())
}
#[cfg(target_arch = "wasm32")]
pub use resource::{Totp, TotpShell};
#[cfg(not(target_arch = "wasm32"))]
pub use resource::{Totp, TotpShell, launch_spec};
