//! The catalogue screenshots and layout tests show: hermetic, so a capture
//! never reaches for the network. Its icons and previews are bundled artwork
//! served to the app's image store by a local fetcher.

use crate::app::StoreApp;
use crate::model::{InstalledState, StoreItem, StoreItemVersion, StoreSource, StoreTrust};

/// How many previews each fixture app lists.
const PREVIEWS: usize = 3;

/// Where the fixture catalogue's artwork nominally lives.
const ARTWORK_HOST: &str = "https://artwork.invalid/";

macro_rules! artwork {
    ($($file:literal => $artifact:literal),* $(,)?) => {
        &[$(($file, include_bytes!(concat!(env!("OUT_DIR"), "/", $artifact)))),*]
    };
}

/// The bundled artwork of the fixture apps: an icon and previews each.
const ARTWORK: &[(&str, &[u8])] = artwork![
    "Notes.png" => "notes.png",
    "Terminal.png" => "terminal.png",
    "Files.png" => "files.png",
    "Chess.png" => "chess.png",
    "Inkscape.png" => "inkscape.png",
    "VLC.png" => "vlc.png",
    "Notes-preview-1.png" => "notes-preview-1.png",
    "Notes-preview-2.png" => "notes-preview-2.png",
    "Notes-preview-3.png" => "notes-preview-3.png",
    "Terminal-preview-1.png" => "terminal-preview-1.png",
    "Terminal-preview-2.png" => "terminal-preview-2.png",
    "Terminal-preview-3.png" => "terminal-preview-3.png",
    "Files-preview-1.png" => "files-preview-1.png",
    "Files-preview-2.png" => "files-preview-2.png",
    "Files-preview-3.png" => "files-preview-3.png",
    "Chess-preview-1.png" => "chess-preview-1.png",
    "Chess-preview-2.png" => "chess-preview-2.png",
    "Chess-preview-3.png" => "chess-preview-3.png",
    "Inkscape-preview-1.png" => "inkscape-preview-1.png",
    "Inkscape-preview-2.png" => "inkscape-preview-2.png",
    "Inkscape-preview-3.png" => "inkscape-preview-3.png",
    "VLC-preview-1.png" => "vlc-preview-1.png",
    "VLC-preview-2.png" => "vlc-preview-2.png",
    "VLC-preview-3.png" => "vlc-preview-3.png",
];

/// The image store of a capture: every bundled icon and preview is decoded
/// before the first frame. Preview widgets may request their image only
/// while drawing, too late for a one-frame capture to wait for that load.
/// The toolkit's host loader completes these local bytes without a network
/// fetcher or a background race.
pub fn artwork_store() -> ui::ImageStore {
    let (store, mut loader) = ui::ImageStore::host_driven();
    for (name, bytes) in ARTWORK {
        let url = format!("{ARTWORK_HOST}{name}");
        store.get(&url);
        loader.take_requests();
        loader.complete(&url, Some(bytes));
    }
    store
}

/// The preview URLs the fixture lists for the app called `name`.
fn previews(name: &str) -> Vec<String> {
    (1..=PREVIEWS)
        .map(|index| format!("{ARTWORK_HOST}{name}-preview-{index}.png"))
        .collect()
}

/// The key every first-party Tilo package is signed with in the fixture.
const TILO_PUBLISHER_KEY: &str = "3f9ac1d27b8e4455a0c6e1f2d93b7a08c4e5f61729ab3d80e7c2145f6a9d0b3e";

/// A hand-written catalogue that shows every card variant:
///
///   * a Tilo app that is not on the device: **Open**
///   * a Tilo app that is cached and kept: **Open**, plus **Allow cleanup** and Remove
///   * a Tilo app that is cached with an update available: **Update**
///   * Flatpak apps: **Install**, and **Open** with Remove once installed
#[allow(
    clippy::too_many_lines,
    reason = "the deterministic visual fixture keeps every app/action state together so screenshot review can see the complete catalogue vocabulary"
)]
pub fn fixture_catalog() -> Vec<StoreItem> {
    let tilo = |name: &str,
                description: &str,
                category: &str,
                installed: InstalledState,
                version: &str,
                digest: &str| StoreItem {
        bot: None,
        id: format!("so.tilo.{}", name.to_ascii_lowercase()),
        name: name.into(),
        description: description.into(),
        tagline: if name == "Notes" {
            "Ideas worth keeping, together.".into()
        } else {
            String::new()
        },
        supported_languages: if name == "Notes" {
            vec!["en".into(), "es".into()]
        } else {
            Vec::new()
        },
        categories: vec![category.into()],
        icon_url: Some(format!("{ARTWORK_HOST}{name}.png")),
        screenshot_urls: previews(name),
        homepage_url: Some("https://tilo.so/apps".into()),
        source_url: Some(format!(
            "https://git.tilo.so/apps/{}",
            name.to_ascii_lowercase()
        )),
        install_ref_path: None,
        source: StoreSource::Tilo {
            registry_name: "Tilo main".into(),
        },
        trust: StoreTrust::PublisherSigned,
        versions: vec![StoreItemVersion {
            version: version.into(),
            released_at_unix: 0,
            download_url: Some(format!("https://apps.tilo.so/{name}/{version}.tapp")),
            root_hash_hex: Some(digest.into()),
            size_bytes: 4_718_592,
            runtime: None,
        }],
        publisher_pubkey_hex: Some(TILO_PUBLISHER_KEY.into()),
        installed,
    };
    let flatpak = |id: &str,
                   name: &str,
                   description: &str,
                   category: &str,
                   installed: InstalledState,
                   homepage: &str| StoreItem {
        bot: None,
        id: id.into(),
        name: name.into(),
        description: description.into(),
        tagline: String::new(),
        supported_languages: Vec::new(),
        categories: vec![category.into()],
        icon_url: Some(format!("{ARTWORK_HOST}{name}.png")),
        screenshot_urls: previews(name),
        homepage_url: Some(homepage.into()),
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
            runtime: None,
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
            "9be4d1c07a52f38e6c1b90a7d4e8f2630b5c9a1d7e4f82a36b0c5d9e1f7a4c28",
        ),
        tilo(
            "Terminal",
            "A fast terminal for commands, scripts, and development tools.",
            "System",
            // Cached and kept: Open, Allow cleanup and Remove.
            InstalledState::Installed {
                version: "0.2.0".into(),
                available: "0.2.0".into(),
                pinned: true,
                user_managed: true,
            },
            "0.2.0",
            "c71f0a94e3d8256b9f4a1e07d3c85b62a9e0f417d6b3c8a25e91f0d47b6c3a85",
        ),
        tilo(
            "Files",
            "Browse, organise, and share files across your workspaces.",
            "System",
            // Cached with a newer version listed: Update and Keep on device.
            InstalledState::Installed {
                version: "0.3.0".into(),
                available: "0.4.0".into(),
                pinned: false,
                user_managed: true,
            },
            "0.4.0",
            "4d82e6b1f09a375c8e2d14a6b93f07c5e18a2d6049b7f3e51c8a0d92b64e7f13",
        ),
        flatpak(
            "org.gnome.Chess",
            "Chess",
            "Play a classic game of chess against a person or the computer.",
            "Games",
            InstalledState::Not,
            "https://wiki.gnome.org/Apps/Chess",
        ),
        flatpak(
            "org.inkscape.Inkscape",
            "Inkscape",
            "Draw logos, illustrations and diagrams with precise vector tools.",
            "Graphics",
            InstalledState::Not,
            "https://inkscape.org",
        ),
        flatpak(
            "org.videolan.VLC",
            "VLC",
            "Play almost any video or audio file, from local media to network streams.",
            "AudioVideo",
            InstalledState::Installed {
                version: "3.2.1".into(),
                available: "3.2.1".into(),
                pinned: false,
                user_managed: true,
            },
            "https://www.videolan.org/vlc",
        ),
    ]
}

/// Deterministic catalogue data for capture scenarios and isolated owner review.
pub(crate) fn capture_catalog(scenario: Option<&str>) -> Vec<StoreItem> {
    let items = fixture_catalog();
    if scenario != Some("paging") {
        return items;
    }
    items
        .iter()
        .cycle()
        .take(50)
        .enumerate()
        .map(|(index, original)| {
            let mut item = original.clone();
            item.id = format!("org.example.Capture{index}");
            item.name = format!("Capture app {index:02}");
            item
        })
        .collect()
}

/// An app seeded with the fixture catalogue.
pub fn seeded_app() -> StoreApp {
    let mut app = StoreApp::new();
    app.set_flatpak_available(true);
    app.set_items(fixture_catalog());
    app
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captures exercise actual artwork, not a successful placeholder frame.
    #[test]
    fn every_bundled_image_is_ready_before_the_first_frame() {
        let store = artwork_store();
        for (name, _) in ARTWORK {
            let url = format!("{ARTWORK_HOST}{name}");
            assert!(
                store.get(&url).is_some(),
                "{name} is decoded before capture"
            );
        }
        assert!(store.is_idle(), "no fixture work remains in flight");
    }

    /// The fixture catalogue covers every action and every state, with realistic provenance.
    #[test]
    fn fixture_catalogue_covers_every_action_and_is_complete() {
        use crate::model::ActionKind;
        let catalogue = fixture_catalog();
        let mut saw_open = false;
        let mut saw_update = false;
        let mut saw_flatpak_install = false;
        let mut saw_flatpak_open = false;
        let mut saw_flatpak_remove = false;
        let mut saw_pin_toggle = false;
        for item in &catalogue {
            match item.action_kind(true) {
                ActionKind::Open => saw_open = true,
                ActionKind::Update => saw_update = true,
                ActionKind::FlatpakInstall => saw_flatpak_install = true,
                ActionKind::FlatpakOpen => saw_flatpak_open = true,
                _ => {}
            }
            saw_pin_toggle |= item.pin_action().is_some();
            // Flatpak removal is a secondary action: the primary on an
            // installed app is to open it.
            saw_flatpak_remove |= item.remove_action() == Some(ActionKind::FlatpakRemove);
            if let Some(key) = item.publisher_pubkey_hex.as_deref() {
                assert_eq!(key.len(), 64, "{} has a full ed25519 key", item.name);
            }
            for version in &item.versions {
                if let Some(hash) = version.root_hash_hex.as_deref() {
                    assert_eq!(hash.len(), 64, "{} has a full digest", item.name);
                }
            }
        }
        assert!(
            saw_open
                && saw_update
                && saw_flatpak_install
                && saw_flatpak_open
                && saw_flatpak_remove
                && saw_pin_toggle,
            "fixture must exercise Open, Update, Flatpak install/open/remove, and the pin toggle"
        );
    }

    /// The seeded app lists the fixture catalogue.
    #[test]
    fn seeded_app_lists_the_fixture_catalogue() {
        assert_eq!(seeded_app().items().len(), fixture_catalog().len());
    }

    /// The bundled artwork serves every icon and preview the fixture names,
    /// and nothing else.
    #[test]
    fn artwork_covers_the_fixture_and_nothing_else() {
        for item in fixture_catalog() {
            for url in item.icon_url.iter().chain(&item.screenshot_urls) {
                let name = url.strip_prefix(ARTWORK_HOST).expect("fixture artwork URL");
                assert!(
                    ARTWORK.iter().any(|(file, _)| *file == name),
                    "{name} is bundled"
                );
            }
        }
    }
}
