//! What an application declared it may do on the action door under its
//! own identity.
//!
//! The declaration is the signed manifest's `[actions]` table
//! ([`ActionAccess`]). It is a ceiling, never a grant: invoking another
//! application's action still needs the workspace to grant it to the app.

use std::sync::Arc;

pub use tilo_app_package::manifest::ActionAccess;

/// Where declared access is read from, by package id.
pub type AccessResolver = Arc<dyn Fn(&str) -> ActionAccess + Send + Sync>;

/// The access declared by the installed package `app_id`; nothing for an
/// unknown package or an unreadable manifest.
#[must_use]
pub fn installed(app_id: &str) -> ActionAccess {
    crate::shell::launcher::installed_apps()
        .unwrap_or_default()
        .into_iter()
        .find(|app| app.package_id == app_id)
        .and_then(|app| std::fs::read_to_string(&app.manifest_path).ok())
        .and_then(|source| tilo_app_package::manifest::Manifest::from_toml_str(&source).ok())
        .map(|manifest| manifest.actions)
        .unwrap_or_default()
}

/// [`installed`] as a resolver.
#[must_use]
pub fn installed_resolver() -> AccessResolver {
    Arc::new(installed)
}
