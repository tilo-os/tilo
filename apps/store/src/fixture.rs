//! The catalogue screenshots and layout tests show: hermetic, so a capture
//! never reaches for the network.

use crate::app::StoreApp;
use crate::model::{InstalledState, StoreItem, StoreItemVersion, StoreSource, StoreTrust};

/// A hand-rolled fixture of items that shows every card variant:
///
///   * a Tilo item that's not installed  → **Open**
///   * a Tilo item that's cached + pinned → **Open** + **Installed** + Remove
///   * a Tilo item that's cached but out-of-date → **Update** + Keep on device
///   * a Flatpak item (flatpak available) → **Install**
///   * a Flatpak item, flatpak unavailable path is covered by a
///     `set_flatpak_available(false)` variant used by the
///     "no-flatpak" scenario.
#[allow(
    clippy::too_many_lines,
    reason = "the deterministic visual fixture keeps every app/action state together so screenshot review can see the complete catalogue vocabulary"
)]
pub fn fixture_catalog() -> Vec<StoreItem> {
    let tilo =
        |name: &str, desc: &str, cat: &str, installed: InstalledState, ver: &str| StoreItem {
            id: format!("so.tilo.{}", name.to_ascii_lowercase()),
            name: name.into(),
            description: desc.into(),
            tagline: String::new(),
            supported_languages: Vec::new(),
            categories: vec![cat.into()],
            icon_url: Some(format!("https://artwork.invalid/{name}.png")),
            screenshot_urls: vec![format!("https://artwork.invalid/{name}-preview.png")],
            homepage_url: None,
            source_url: None,
            install_ref_path: None,
            source: StoreSource::Tilo {
                registry_name: "Tilo main".into(),
            },
            trust: StoreTrust::PublisherSigned,
            versions: vec![StoreItemVersion {
                version: ver.into(),
                released_at_unix: 0,
                download_url: Some(format!("https://apps.tilo.so/{name}/{ver}.tapp")),
                root_hash_hex: Some("de".repeat(32)),
                size_bytes: 4 * 1024 * 1024,
            }],
            publisher_pubkey_hex: Some("aa".repeat(32)),
            installed,
        };
    let fh = |name: &str, desc: &str, cat: &str, installed: InstalledState| StoreItem {
        id: format!("org.example.{name}"),
        name: name.into(),
        description: desc.into(),
        tagline: String::new(),
        supported_languages: Vec::new(),
        categories: vec![cat.into()],
        icon_url: Some(format!("https://artwork.invalid/{name}.png")),
        screenshot_urls: vec![format!("https://artwork.invalid/{name}-preview.png")],
        homepage_url: None,
        source_url: None,
        install_ref_path: None,
        source: StoreSource::Flathub,
        trust: StoreTrust::Flathub,
        versions: vec![StoreItemVersion {
            version: "3.2.1".into(),
            released_at_unix: 0,
            download_url: None,
            root_hash_hex: None,
            size_bytes: 0,
        }],
        publisher_pubkey_hex: None,
        installed,
    };
    vec![
        tilo(
            "Notes",
            "Capture ideas, organise projects, and work together in real time.",
            "Productivity",
            InstalledState::Not,
            "0.1.0",
        ),
        tilo(
            "Terminal",
            "A fast terminal for commands, scripts, and development tools.",
            "System",
            // Cached + marked installed (pinned): Open + Installed + Remove.
            InstalledState::Installed {
                version: "0.2.0".into(),
                available: "0.2.0".into(),
                pinned: true,
                user_managed: true,
            },
            "0.2.0",
        ),
        tilo(
            "Files",
            "Browse, organise, and share files across your workspaces.",
            "System",
            // Cached, not pinned, update available: Update + Keep on device.
            InstalledState::Installed {
                version: "0.3.0".into(),
                available: "0.4.0".into(),
                pinned: false,
                user_managed: true,
            },
            "0.4.0",
        ),
        fh(
            "Chess",
            "Play a classic game of chess against a person or the computer.",
            "Games",
            InstalledState::Not,
        ),
        fh(
            "Inkscape",
            "Vector graphics editor from Flathub.",
            "Utilities",
            InstalledState::Not,
        ),
        fh(
            "VLC",
            "The famous media player — already installed here.",
            "AudioVideo",
            InstalledState::Installed {
                version: "3.2.1".into(),
                available: "3.2.1".into(),
                pinned: false,
                user_managed: true,
            },
        ),
    ]
}

/// A Store surface showing the fixture catalogue, outside any window: for
/// tests of its layout.
pub fn seeded_app() -> StoreApp {
    let mut app = StoreApp::new();
    app.set_flatpak_available(true);
    app.set_items(fixture_catalog());
    app.set_status_line("6 apps available.");
    app
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture catalog covers every action, and the seeded app lists it.
    #[test]
    fn fixture_covers_every_action_and_seeds_the_app() {
        // Case `fixture_catalog_covers_every_action_variant`.
        {
            use crate::model::ActionKind;
            let cat = fixture_catalog();
            let mut saw_open = false;
            let mut saw_update = false;
            let mut saw_fp_install = false;
            let mut saw_fp_open = false;
            let mut saw_fp_remove = false;
            let mut saw_pin_toggle = false;
            for item in &cat {
                match item.action_kind(true) {
                    ActionKind::Open => saw_open = true,
                    ActionKind::Update => saw_update = true,
                    ActionKind::FlatpakInstall => saw_fp_install = true,
                    ActionKind::FlatpakOpen => saw_fp_open = true,
                    _ => {}
                }
                if item.pin_action().is_some() {
                    saw_pin_toggle = true;
                }
                // Flatpak removal is a *secondary* action — the primary on
                // an installed app is to open it — so it is covered where
                // it now lives rather than dropped from the fixture.
                if item.remove_action() == Some(ActionKind::FlatpakRemove) {
                    saw_fp_remove = true;
                }
            }
            assert!(
                saw_open
                    && saw_update
                    && saw_fp_install
                    && saw_fp_open
                    && saw_fp_remove
                    && saw_pin_toggle,
                "fixture must exercise Open, Update, Flatpak install/open/remove, \
             and the pin toggle (open={saw_fp_open}, remove={saw_fp_remove})"
            );
        }

        // Case `seeded_app_lists_items`.
        {
            let app = seeded_app();
            assert_eq!(app.items().len(), fixture_catalog().len());
        }
    }
}
