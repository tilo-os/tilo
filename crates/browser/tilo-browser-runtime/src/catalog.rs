//! The applications compiled into this browser build. Standalone mounts and the
//! canonical desktop resolve app ids through this one catalogue.
use std::sync::LazyLock;
use ui::application::{ApplicationCatalog, ApplicationDefinition};
use ui::launch::LaunchError;

/// Where a launch finds its document when the host names no record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecordScope {
    /// Each launch opens a named record or creates a new one.
    Ordinary,
    /// The room holds one package-primary document, which every launch
    /// opens. It travels as the empty record id on the wire.
    PackagePrimary,
}

type Installed = (fn() -> &'static ApplicationDefinition, RecordScope);

/// Every component application in the browser build. Registering an app here
/// is all it takes to launch it by id, standalone or on the desktop.
const INSTALLED: [Installed; 5] = [
    (word_processor::definition, RecordScope::Ordinary),
    (chat::definition, RecordScope::PackagePrimary),
    (notes::definition, RecordScope::Ordinary),
    (calculator::definition, RecordScope::Ordinary),
    (ebook::definition, RecordScope::Ordinary),
];

/// Short spellings the Cloud frontend passes to a standalone mount. The
/// desktop resolves persisted ids exactly and never consults these.
#[cfg(any(target_arch = "wasm32", test))]
const ALIASES: [(&str, &str); 2] = [
    ("writer", "so.tilo.word-processor"),
    ("chat", "so.tilo.chat"),
];

static CATALOG: LazyLock<ApplicationCatalog> = LazyLock::new(|| {
    let mut catalog = ApplicationCatalog::new();
    for (definition, _) in INSTALLED {
        // `catalog()` reports a duplicate; the first registration wins here.
        let _ = catalog.register(definition());
    }
    catalog
});

/// A fresh catalogue for a host that owns one, such as the desktop's tile
/// resolver.
///
/// # Errors
/// Two installed definitions claim the same app id.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn catalog() -> Result<ApplicationCatalog, LaunchError> {
    let mut catalog = ApplicationCatalog::new();
    for (definition, _) in INSTALLED {
        catalog.register(definition())?;
    }
    Ok(catalog)
}

/// The installed definitions in registration order.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn installed() -> Vec<&'static ApplicationDefinition> {
    INSTALLED
        .iter()
        .map(|(definition, _)| definition())
        .collect()
}

/// Resolve an exact app id.
///
/// # Errors
/// [`LaunchError::UnknownApplication`] when no installed app has this id.
pub(crate) fn resolve(app: &str) -> Result<&'static ApplicationDefinition, LaunchError> {
    CATALOG.resolve(app)
}

/// Resolve what a standalone mount names: an app id or one of its aliases.
///
/// # Errors
/// [`LaunchError::UnknownApplication`] when neither names an installed app.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn resolve_mount(app: &str) -> Result<&'static ApplicationDefinition, LaunchError> {
    let app = ALIASES
        .iter()
        .find_map(|(alias, id)| (*alias == app).then_some(*id))
        .unwrap_or(app);
    resolve(app)
}

/// The record scope of an installed app, `None` when it is not installed.
pub(crate) fn scope(app: &str) -> Option<RecordScope> {
    INSTALLED
        .iter()
        .find_map(|(definition, scope)| (definition().app_id == app).then_some(*scope))
}

/// An installed application as the page host sees it (`host.d.ts`'s
/// `ApplicationInfo`).
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn describe(definition: &ApplicationDefinition) -> serde_json::Value {
    let scope = match scope(definition.app_id) {
        Some(RecordScope::PackagePrimary) => "package-primary",
        _ => "ordinary",
    };
    serde_json::json!({"id": definition.app_id, "title": definition.title, "scope": scope})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_installed_app_resolves_by_its_own_id_once() {
        let Ok(catalog) = catalog() else {
            panic!("installed app ids are unique");
        };
        for definition in installed() {
            let resolved = catalog.resolve(definition.app_id).ok().unwrap();
            assert!(std::ptr::eq(resolved, definition));
            assert!(std::ptr::eq(
                resolve(definition.app_id).ok().unwrap(),
                definition
            ));
        }
    }

    #[test]
    fn standalone_aliases_name_the_full_ids_but_the_desktop_ignores_them() {
        let mounted = |app| resolve_mount(app).ok().unwrap().app_id;
        assert_eq!(mounted("writer"), "so.tilo.word-processor");
        assert_eq!(mounted("chat"), "so.tilo.chat");
        assert_eq!(mounted("so.tilo.notes"), "so.tilo.notes");
        assert_eq!(mounted("so.tilo.ebook"), "so.tilo.ebook");
        assert!(matches!(
            resolve("writer"),
            Err(LaunchError::UnknownApplication(id)) if id == "writer"
        ));
    }

    #[test]
    fn unknown_ids_fail_with_the_catalog_error_instead_of_a_placeholder() {
        for app in ["so.tilo.nope", "desktop", ""] {
            let error = resolve_mount(app).err().expect("not installed");
            assert!(matches!(&error, LaunchError::UnknownApplication(id) if id == app));
            assert_eq!(
                error.to_string(),
                format!("application {app:?} is not available")
            );
        }
    }

    #[test]
    fn only_chat_opens_its_package_primary_room_document() {
        assert_eq!(scope("so.tilo.chat"), Some(RecordScope::PackagePrimary));
        for definition in installed() {
            if definition.app_id != "so.tilo.chat" {
                assert_eq!(scope(definition.app_id), Some(RecordScope::Ordinary));
            }
        }
        assert_eq!(scope("chat"), None);
    }

    #[test]
    fn the_page_host_sees_the_resolved_id_title_and_scope() {
        let chat = resolve_mount("chat").ok().unwrap();
        assert_eq!(
            describe(chat),
            serde_json::json!({"id":"so.tilo.chat","title":"Chat","scope":"package-primary"})
        );
        let writer = resolve_mount("writer").ok().unwrap();
        assert_eq!(describe(writer)["id"], "so.tilo.word-processor");
        assert_eq!(describe(writer)["scope"], "ordinary");
    }
}
