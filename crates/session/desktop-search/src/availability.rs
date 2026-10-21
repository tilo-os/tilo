//! What the manifest registry knows about each Tilo application: where it
//! runs, its category, and the other names and words people know it by. The
//! catalog is generated from every app's `tilo-app.toml`.
//!
//! A desktop offers only the apps it can run. An app that cannot run here is
//! not listed with a reason: offering Terminal in a browser tab, where
//! choosing it can only fail, is noise at best and a broken promise at worst.

/// One application of the manifest registry and the platforms it declares
/// (`linux`, `macos`, `web`, `tilo-os`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogApp {
    pub id: &'static str,
    pub name: &'static str,
    pub category: &'static str,
    pub platforms: &'static [&'static str],
    /// Its name in the other languages its manifest gives one
    /// (`[package.names]`): "Dateien" for Files. Searched as the name.
    pub names: &'static [&'static str],
    /// The other words people know it by, every language's together
    /// (`[package.keywords]`): "youtube" for a video app.
    pub keywords: &'static [&'static str],
}

#[path = "catalog.rs"]
mod catalog;

/// Every application in the manifest registry.
#[must_use]
pub fn catalog() -> &'static [CatalogApp] {
    catalog::APPS
}

/// The registry's entry for the app `package_id`, if it lists it.
fn entry_for(package_id: &str) -> Option<&'static CatalogApp> {
    // A local build publishes the same apps under `dev.tilo.`.
    let canonical = package_id.strip_prefix("dev.tilo.");
    catalog().iter().find(|app| {
        app.id == package_id
            || canonical.is_some_and(|name| app.id.strip_prefix("so.tilo.") == Some(name))
    })
}

/// The keywords the registry knows for the app `package_id` — empty for an
/// app it does not list.
#[must_use]
pub fn keywords_for(package_id: &str) -> &'static [&'static str] {
    entry_for(package_id).map_or(&[], |app| app.keywords)
}

/// The app's name in the other languages the registry knows ("Dateien",
/// "Rechner") — empty for an app it does not list.
#[must_use]
pub fn names_for(package_id: &str) -> &'static [&'static str] {
    entry_for(package_id).map_or(&[], |app| app.names)
}

/// The category the app's manifest declares ("Office"), if the registry
/// lists it with one.
#[must_use]
pub fn category_for(package_id: &str) -> Option<&'static str> {
    entry_for(package_id)
        .map(|app| app.category)
        .filter(|category| !category.is_empty())
}

/// Whether the app `package_id` runs on any of `host`, the platforms a
/// desktop provides. An app the registry does not list is one the desktop
/// found to run itself (a node's app, an installed package), so it runs.
#[must_use]
pub fn runs_on(package_id: &str, host: &[&str]) -> bool {
    entry_for(package_id).is_none_or(|app| app.platforms.iter().any(|p| host.contains(p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str) -> &'static CatalogApp {
        entry_for(id).unwrap_or_else(|| panic!("the registry lists {id}"))
    }

    #[test]
    fn each_platform_runs_only_the_apps_that_declare_it() {
        let terminal = app("so.tilo.terminal");
        assert!(!terminal.platforms.contains(&"web"));
        assert!(!runs_on(terminal.id, &["web"]), "no terminal in a browser");
        assert!(runs_on(terminal.id, &["macos"]));
        assert!(runs_on(terminal.id, &["tilo-os", "linux"]));
        assert!(runs_on("dev.tilo.terminal", &["linux"]), "a local build");
        assert!(!runs_on("dev.tilo.terminal", &["web"]));

        let os_only = catalog()
            .iter()
            .find(|app| app.platforms == ["tilo-os"])
            .expect("the registry has a Tilo-OS-only app");
        assert!(!runs_on(os_only.id, &["macos"]));
        assert!(!runs_on(os_only.id, &["web"]));
        assert!(runs_on(os_only.id, &["tilo-os", "linux"]));

        assert!(runs_on("so.tilo.notes", &["web"]));
        assert!(
            runs_on("node-app:7", &["web"]),
            "not the registry's to judge"
        );

        let web: Vec<_> = catalog()
            .iter()
            .filter(|app| runs_on(app.id, &["web"]))
            .collect();
        assert!(!web.is_empty());
        assert!(web.iter().all(|app| app.platforms.contains(&"web")));
    }

    #[test]
    fn categories_come_from_the_manifest() {
        assert_eq!(category_for("so.tilo.calendar"), Some("Office"));
        assert_eq!(category_for("dev.tilo.calendar"), Some("Office"));
        assert_eq!(category_for("node-app:7"), None);
    }
}
