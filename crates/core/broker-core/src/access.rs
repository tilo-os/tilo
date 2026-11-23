//! What an application declared it may do on the action API under its own identity.
//!
//! The declaration is a ceiling, never a grant: invoking another app's action still needs the
//! workspace to grant it to the app. A host reads the declaration from the app's signed manifest
//! ([`AppActionAccessResolver`]) and the broker holds both sides to it.

use std::sync::Arc;

/// What an application declared it may do on the API under its own identity.
pub use tilo_app_package::manifest::ActionAccess as AppActionAccess;

/// Where declared access is read from, by package id.
pub type AppActionAccessResolver = Arc<dyn Fn(&str) -> AppActionAccess + Send + Sync>;

/// A resolver under which no application declared anything.
#[must_use]
pub fn nothing_declared() -> AppActionAccessResolver {
    Arc::new(|_| AppActionAccess::default())
}

/// The resource kinds an application declared in its signed manifest, by
/// package id: what it may reach in its own node.
pub type AppResourceKindsResolver = Arc<dyn Fn(&str) -> Vec<String> + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_declared_returns_empty_manifest_action_access() {
        let access: tilo_app_package::manifest::ActionAccess = nothing_declared()("so.tilo.notes");
        assert_eq!(
            access,
            AppActionAccess {
                query: false,
                invoke: false,
            }
        );
        assert!(access.is_empty());
    }
}
