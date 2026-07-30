//! Contacts — a CRDT-native address book for qOS, built on the `ui`
//! toolkit.
//!
//! `app_root()` is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer. Everything else —
//! data model, vCard interop, the Automerge document layer, and the
//! master/detail widget — lives in the modules below so each layer is
//! testable in isolation.

pub mod actions;
pub mod app;
pub mod date;
pub mod doc;
pub mod model;
pub mod portal;
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
