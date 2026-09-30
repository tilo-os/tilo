//! Which Tilo applications a desktop cannot run here, and why. The catalog
//! is generated from every app's `tilo-app.toml`, so a desktop lists an app
//! it cannot open with the same reason the Cloud console shows.

use crate::providers::AppEntry;

/// One application of the manifest registry and the platforms it declares
/// (`linux`, `macos`, `web`, `tilo-os`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogApp {
    pub id: &'static str,
    pub name: &'static str,
    pub category: &'static str,
    pub platforms: &'static [&'static str],
}

#[path = "catalog.rs"]
mod catalog;

/// Every application in the manifest registry.
#[must_use]
pub fn catalog() -> &'static [CatalogApp] {
    catalog::APPS
}

/// Why an app declaring `platforms` is not offered: "Tilo OS only",
/// "Needs Tilo OS", or the platforms it runs on ("Linux and macOS only").
#[must_use]
pub fn unavailable_reason(platforms: &[&str]) -> String {
    if platforms == ["tilo-os"] {
        return "Tilo OS only".into();
    }
    if platforms.contains(&"tilo-os") {
        return "Needs Tilo OS".into();
    }
    let names: Vec<&str> = platforms
        .iter()
        .map(|platform| match *platform {
            "linux" => "Linux",
            "macos" => "macOS",
            "web" => "the web",
            other => other,
        })
        .collect();
    match names.as_slice() {
        [] => "Not available here".into(),
        [only] => format!("{only} only"),
        [rest @ .., last] => format!("{} and {last} only", rest.join(", ")),
    }
}

/// The catalog's apps that run on none of `host` (the platforms this
/// desktop provides), as search entries carrying their reason.
#[must_use]
pub fn unavailable_on(host: &[&str]) -> Vec<AppEntry> {
    catalog()
        .iter()
        .filter(|app| !app.platforms.iter().any(|platform| host.contains(platform)))
        .map(|app| AppEntry {
            id: app.id.into(),
            title: app.name.into(),
            category: (!app.category.is_empty()).then(|| app.category.into()),
            description: None,
            unavailable: Some(unavailable_reason(app.platforms)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_match_the_cloud_console() {
        assert_eq!(unavailable_reason(&["tilo-os"]), "Tilo OS only");
        assert_eq!(unavailable_reason(&["tilo-os", "linux"]), "Needs Tilo OS");
        assert_eq!(
            unavailable_reason(&["linux", "macos"]),
            "Linux and macOS only"
        );
        assert_eq!(unavailable_reason(&["linux"]), "Linux only");
        assert_eq!(
            unavailable_reason(&["linux", "macos", "web"]),
            "Linux, macOS and the web only"
        );
    }

    #[test]
    fn each_desktop_lists_what_it_cannot_run() {
        let web = unavailable_on(&["web"]);
        let desktop_only = catalog()
            .iter()
            .find(|app| app.platforms == ["linux", "macos"])
            .expect("the registry has a desktop-only app");
        let os_only = catalog()
            .iter()
            .find(|app| app.platforms == ["tilo-os"])
            .expect("the registry has a Tilo-OS-only app");
        let reason = |entries: &[AppEntry], id: &str| {
            entries
                .iter()
                .find(|entry| entry.id == id)
                .and_then(|entry| entry.unavailable.clone())
        };
        assert_eq!(
            reason(&web, desktop_only.id).as_deref(),
            Some("Linux and macOS only")
        );
        assert_eq!(reason(&web, os_only.id).as_deref(), Some("Tilo OS only"));
        assert!(web.iter().all(|entry| {
            catalog()
                .iter()
                .any(|app| app.id == entry.id && !app.platforms.contains(&"web"))
        }));
        let mac = unavailable_on(&["macos"]);
        assert_eq!(reason(&mac, desktop_only.id), None);
        assert_eq!(reason(&mac, os_only.id).as_deref(), Some("Tilo OS only"));
    }
}
