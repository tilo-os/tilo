//! Contacts — a CRDT-native address book for Tilo, built on the `ui`
//! toolkit.
//!
//! `app_root()` is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer. Everything else —
//! data model, vCard interop, the Automerge document layer, and the
//! master/detail widget — lives in the modules below so each layer is
//! testable in isolation.

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
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
pub mod date;
pub mod doc;
mod export_dialog;
pub mod model;
mod priority_contacts;
pub mod resource;
pub mod responsive;
pub mod search_provider;
pub mod vcard;

pub use app::ContactsApp;

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(ContactsApp::new())
}

/// The app as every host mounts it: the live root and its actions.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.contacts", app_root)
        .history(&framework_history::HISTORY)
        .component_actions(actions::register)
        .as_definition("Contacts")
}

#[cfg(test)]
mod framework_history_tests;

pub mod framework_history;
pub use resource::{Contacts, ContactsShell};
