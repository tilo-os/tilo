//! The applications installed in this browser runtime. Standalone mounts and
//! the canonical desktop resolve app ids through this one catalogue.
//!
//! Apps are compiled in (natively always, in wasm32 with `static-apps`) or
//! installed at run time by a side module linked against the web core
//! (`dylink`), which hands its `definition()` to [`install`].
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};
#[cfg(any(target_arch = "wasm32", test))]
use ui::application::ApplicationCatalog;
use ui::application::ApplicationDefinition;
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

/// Every component application compiled into the build. Registering an app
/// here is all it takes to launch it by id, standalone or on the desktop.
#[cfg(any(feature = "static-apps", not(target_arch = "wasm32")))]
const COMPILED: [fn() -> &'static ApplicationDefinition; 5] = [
    word_processor::definition,
    chat::definition,
    notes::definition,
    calculator::definition,
    ebook::definition,
];
#[cfg(all(not(feature = "static-apps"), target_arch = "wasm32"))]
const COMPILED: [fn() -> &'static ApplicationDefinition; 0] = [];

/// Apps whose room holds one package-primary document; every other app is
/// [`RecordScope::Ordinary`], however it was installed.
const PACKAGE_PRIMARY: [&str; 1] = ["so.tilo.chat"];

/// Short spellings the Cloud frontend passes to a standalone mount. The
/// desktop resolves persisted ids exactly and never consults these.
#[cfg(any(target_arch = "wasm32", test))]
const ALIASES: [(&str, &str); 2] = [
    ("writer", "so.tilo.word-processor"),
    ("chat", "so.tilo.chat"),
];

/// Installed definitions in installation order.
#[derive(Default)]
struct Installed(Vec<&'static ApplicationDefinition>);

impl Installed {
    /// The compiled apps; a duplicate among them keeps the first, as
    /// `catalog()` reports it.
    fn compiled() -> Self {
        let mut installed = Self::default();
        for definition in COMPILED {
            let _ = installed.install(definition());
        }
        installed
    }

    fn install(&mut self, definition: &'static ApplicationDefinition) -> Result<(), LaunchError> {
        if self.0.iter().any(|d| d.app_id == definition.app_id) {
            return Err(LaunchError::DuplicateApplication(definition.app_id.into()));
        }
        self.0.push(definition);
        Ok(())
    }

    fn resolve(&self, app: &str) -> Result<&'static ApplicationDefinition, LaunchError> {
        self.0
            .iter()
            .copied()
            .find(|definition| definition.app_id == app)
            .ok_or_else(|| LaunchError::UnknownApplication(app.into()))
    }

    #[cfg(any(target_arch = "wasm32", test))]
    fn catalog(&self) -> Result<ApplicationCatalog, LaunchError> {
        let mut catalog = ApplicationCatalog::new();
        for definition in COMPILED {
            catalog.register(definition())?;
        }
        for definition in &self.0 {
            if catalog.resolve(definition.app_id).is_err() {
                catalog.register(definition)?;
            }
        }
        Ok(catalog)
    }
}

static INSTALLED: LazyLock<Mutex<Installed>> = LazyLock::new(|| Mutex::new(Installed::compiled()));

fn lock() -> MutexGuard<'static, Installed> {
    INSTALLED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Install an app at run time, as a linked side module does.
///
/// # Errors
/// [`LaunchError::DuplicateApplication`] when an installed app already has its
/// id; the installed one is kept.
#[cfg_attr(
    not(all(target_arch = "wasm32", feature = "dylink")),
    allow(dead_code, reason = "only side modules install apps at run time")
)]
pub(crate) fn install(definition: &'static ApplicationDefinition) -> Result<(), LaunchError> {
    lock().install(definition)
}

/// A fresh catalogue for a host that owns one, such as the desktop's tile
/// resolver.
///
/// # Errors
/// Two compiled definitions claim the same app id.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn catalog() -> Result<ApplicationCatalog, LaunchError> {
    lock().catalog()
}

/// The installed definitions in installation order.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn installed() -> Vec<&'static ApplicationDefinition> {
    lock().0.clone()
}

/// Resolve an exact app id.
///
/// # Errors
/// [`LaunchError::UnknownApplication`] when no installed app has this id.
pub(crate) fn resolve(app: &str) -> Result<&'static ApplicationDefinition, LaunchError> {
    lock().resolve(app)
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
    resolve(app).ok().map(scope_of)
}

fn scope_of(definition: &ApplicationDefinition) -> RecordScope {
    if PACKAGE_PRIMARY.contains(&definition.app_id) {
        RecordScope::PackagePrimary
    } else {
        RecordScope::Ordinary
    }
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
    fn a_side_module_installs_a_new_app_once_and_never_replaces_one() {
        // A catalogue of its own: other tests read the process-wide one.
        let mut installed = Installed::compiled();
        let books = ebook::definition();
        assert!(matches!(
            installed.install(books),
            Err(LaunchError::DuplicateApplication(id)) if id == "so.tilo.ebook"
        ));
        assert!(std::ptr::eq(
            installed.resolve("so.tilo.ebook").ok().unwrap(),
            books
        ));

        let linked: &'static ApplicationDefinition = Box::leak(Box::new(ApplicationDefinition {
            app_id: "so.tilo.side-module-probe",
            title: "Side module probe",
            icon: books.icon.clone(),
            record_kind: books.record_kind.clone(),
            required: Vec::new(),
            optional: Vec::new(),
            create: books.create,
        }));
        assert!(matches!(
            installed.resolve(linked.app_id),
            Err(LaunchError::UnknownApplication(_))
        ));
        installed.install(linked).ok().unwrap();
        assert!(std::ptr::eq(
            installed.resolve(linked.app_id).ok().unwrap(),
            linked
        ));
        assert_eq!(scope_of(linked), RecordScope::Ordinary);
        let fresh = installed.catalog().ok().unwrap();
        assert!(std::ptr::eq(
            fresh.resolve(linked.app_id).ok().unwrap(),
            linked
        ));
        assert!(installed.install(linked).is_err());
        assert!(
            resolve(linked.app_id).is_err(),
            "the process-wide catalogue is untouched"
        );
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
