//! What the manifest registry knows about each Tilo application: where it
//! runs, its category, and the other names and words people know it by,
//! from the fleet's compiled manifest metadata (`app-fleet`).
//!
//! A desktop offers only the apps it can run. An app that cannot run here is
//! not listed with a reason: offering Terminal in a browser tab, where
//! choosing it can only fail, is noise at best and a broken promise at worst.

use app_fleet::FleetApp;

/// Every application in the manifest registry.
#[must_use]
pub fn catalog() -> &'static [FleetApp] {
    app_fleet::apps()
}

/// The registry's entry for the app `package_id`, if it lists it.
fn entry_for(package_id: &str) -> Option<&'static FleetApp> {
    // A local build publishes the same apps under `dev.tilo.`.
    app_fleet::metadata(package_id)
}

/// The app's name, as the registry knows it.
#[must_use]
pub fn name_for(package_id: &str) -> Option<&'static str> {
    entry_for(package_id).map(|app| app.name)
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

/// Why the app `package_id` does not open on a desktop that runs none of
/// its platforms (`here`: "the browser"), in words for the window that
/// stands in its place: where it runs instead. A window can name such an
/// app when it comes from someone else's desktop, or one of the person's
/// own on another device.
#[must_use]
pub fn runs_elsewhere(package_id: &str, here: &str) -> String {
    let Some(app) = entry_for(package_id) else {
        return format!("This app isn't part of Tilo in {here}");
    };
    if app
        .platforms
        .iter()
        .any(|platform| matches!(*platform, "linux" | "macos"))
    {
        format!("{} runs in Tilo on a computer, not in {here}", app.name)
    } else if app.platforms.contains(&"tilo-os") {
        format!("{} runs on Tilo OS, not in {here}", app.name)
    } else {
        format!("{} doesn't run in {here}", app.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str) -> &'static FleetApp {
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
    fn an_app_that_does_not_run_here_says_where_it_does() {
        assert_eq!(
            runs_elsewhere("so.tilo.terminal", "the browser"),
            "Terminal runs in Tilo on a computer, not in the browser"
        );
        let os_only = catalog()
            .iter()
            .find(|app| app.platforms == ["tilo-os"])
            .expect("the registry has a Tilo-OS-only app");
        assert_eq!(
            runs_elsewhere(os_only.id, "the browser"),
            format!("{} runs on Tilo OS, not in the browser", os_only.name)
        );
        assert_eq!(
            runs_elsewhere("node-app:7", "the browser"),
            "This app isn't part of Tilo in the browser"
        );
    }

    #[test]
    fn categories_come_from_the_manifest() {
        assert_eq!(category_for("so.tilo.calendar"), Some("Office"));
        assert_eq!(category_for("dev.tilo.calendar"), Some("Office"));
        assert_eq!(category_for("node-app:7"), None);
    }
}
