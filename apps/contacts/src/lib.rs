//! Contacts — a CRDT-native address book for Tilo, built on the `ui`
//! toolkit.
//!
//! The app is [`ContactsShell`], a framework app over the [`Contacts`]
//! address book: every host mounts it through [`definition`]. The data
//! model, vCard interop and the master/detail widget live in the modules
//! below so each layer is testable in isolation.

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
mod account_editor;
pub mod actions;
pub mod app;
pub mod date;
mod detail;
mod form;
mod import_inbox;
pub mod model;
mod priority_contacts;
pub mod resource;
pub mod vcard;
mod view;

pub use app::ContactsApp;
pub use resource::{Contacts, ContactsShell};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(ContactsShell)
}
