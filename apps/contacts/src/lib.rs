//! Contacts — a CRDT-native address book for qOS, built on the `ui`
//! toolkit.
//!
//! `app_root()` is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer. Everything else —
//! data model, vCard interop, the Automerge document layer, and the
//! master/detail widget — lives in the modules below so each layer is
//! testable in isolation.

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
        reason = "a test that unwraps is stating an invariant"
    )
)]

pub mod actions;
pub mod app;
pub mod date;
pub mod doc;
pub mod model;
pub mod portal;
mod priority_contacts;
pub mod responsive;
pub mod search_provider;
pub mod storage;
pub mod vcard;

pub use app::ContactsApp;

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(ContactsApp::new())
}
