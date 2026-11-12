//! What an application declared it may do on the action door under its own identity.
//!
//! The declaration is a ceiling, never a grant: invoking another app's action still needs the
//! workspace to grant it to the app. A host reads the declaration from the app's signed manifest
//! ([`AppActionAccessResolver`]) and the broker holds both sides to it.

use std::sync::Arc;

/// What an application declared it may do on the door under its own identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AppActionAccess {
    /// Read the catalogue of actions the person using the app is offered.
    pub query: bool,
    /// Invoke actions other apps publish, as this app.
    pub invoke: bool,
}

/// Where declared access is read from, by package id.
pub type AppActionAccessResolver = Arc<dyn Fn(&str) -> AppActionAccess + Send + Sync>;

/// A resolver under which no application declared anything.
#[must_use]
pub fn nothing_declared() -> AppActionAccessResolver {
    Arc::new(|_| AppActionAccess::default())
}
