//! What an application declared it may do on the action door under its
//! own identity.
//!
//! The declaration is the signed manifest's `[actions]` table. It is a
//! ceiling, never a grant: invoking another application's action still
//! needs the workspace to grant it to the app. The broker decides with it
//! ([`tilo_action_broker::app_actions`]); this module reads it from the
//! installed manifests.

use std::sync::Arc;

pub use tilo_action_broker::app_actions::{
    AppActionAccess as ActionAccess, AppActionAccessResolver as AccessResolver,
};

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
        .map(|manifest| ActionAccess {
            query: manifest.actions.query,
            invoke: manifest.actions.invoke,
        })
        .unwrap_or_default()
}

/// [`installed`] as a resolver.
#[must_use]
pub fn installed_resolver() -> AccessResolver {
    Arc::new(installed)
}
