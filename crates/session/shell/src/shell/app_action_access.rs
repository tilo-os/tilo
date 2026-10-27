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
        .map(|app| {
            let declared = app.declared_actions();
            ActionAccess {
                query: declared.query,
                invoke: declared.invoke,
            }
        })
        .unwrap_or_default()
}

#[must_use]
pub fn installed_resolver() -> AccessResolver {
    Arc::new(installed)
}
