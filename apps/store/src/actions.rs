//! The Store's effects that need nothing from the workspace's tree or this computer's package
//! cache: opening a Flatpak the device has. Every change is the worker's
//! ([`crate::worker`]), through the session's action API: a Tilo app's version and a Flatpak are
//! named on the workspace's tree (`workspace/apps`), which every device of the workspace follows,
//! and a runtime is fetched into this computer's package cache (`devices/<device>/packages`).

use crate::model::{ActionKind, StoreItem, StoreSource};

#[derive(Clone, Debug)]
pub struct ActionOutcome {
    pub ok: bool,
    pub status_line: String,
    pub launch: Option<ApplicationLaunch>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationLaunch {
    pub app_id: String,
    pub package_root: Option<String>,
}

impl ActionOutcome {
    pub(crate) fn ok(msg: impl Into<String>) -> Self {
        Self {
            ok: true,
            status_line: msg.into(),
            launch: None,
        }
    }

    pub(crate) fn fail(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            status_line: msg.into(),
            launch: None,
        }
    }
}

/// Carry out `action` on `item` when it changes nothing: opening a Flatpak, or saying why the
/// action cannot run here. Every change goes through the worker.
pub fn execute(item: &StoreItem, action: ActionKind) -> ActionOutcome {
    if let Some((_, reason)) = item.unavailable_action(action) {
        return ActionOutcome::fail(reason);
    }
    match (action, &item.source) {
        (ActionKind::FlatpakOpen, StoreSource::Flathub | StoreSource::FlatpakRemote { .. }) => {
            ActionOutcome {
                ok: true,
                status_line: format!("Opening {}…", item.name),
                launch: Some(ApplicationLaunch {
                    app_id: format!("host:{}", item.id),
                    package_root: None,
                }),
            }
        }
        (ActionKind::NeedsFlatpakHost, _) => {
            ActionOutcome::fail("Install flatpak on the host to use this action.")
        }
        // A change, or a nonsensical (source, action) pairing: nothing runs here.
        _ => ActionOutcome::fail("unsupported action for this item"),
    }
}

/// The record of a Flatpak on the workspace's tree: the app id (its application id, lowercased)
/// and the `install-app` arguments that name its reference, for the device's daemon to install
/// into the system Flatpak installation.
#[must_use]
pub fn flatpak_release(item: &StoreItem) -> (String, serde_json::Map<String, serde_json::Value>) {
    let app = item.id.to_ascii_lowercase();
    let branch = match &item.source {
        StoreSource::FlatpakRemote { branch, .. } => branch.clone(),
        _ => "stable".to_owned(),
    };
    let reference = format!("app/{}/{}/{branch}", item.id, std::env::consts::ARCH);
    let mut arguments = serde_json::Map::new();
    arguments.insert("app".into(), app.clone().into());
    arguments.insert("version".into(), branch.into());
    arguments.insert("flatpak".into(), reference.into());
    (app, arguments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InstalledState, StoreItem, StoreItemVersion, StoreSource, StoreTrust};

    fn tilo_item(name: &str, installed: InstalledState) -> StoreItem {
        StoreItem {
            bot: None,
            awaiting_developer: false,
            listing: crate::model::Listing::default(),
            id: format!("so.tilo.{}", name.to_ascii_lowercase()),
            name: name.into(),
            description: String::new(),
            tagline: String::new(),
            supported_languages: Vec::new(),
            categories: vec![],
            icon_url: None,
            screenshot_urls: Vec::new(),
            homepage_url: None,
            source_url: None,
            install_ref_path: None,
            source: StoreSource::Tilo {
                registry_name: "Tilo".into(),
            },
            trust: StoreTrust::PublisherSigned,
            versions: vec![StoreItemVersion {
                version: "0.1.0".into(),
                released_at_unix: 0,
                download_url: Some("https://x/y.tapp".into()),
                root_hash_hex: Some("de".repeat(32)),
                surface_hex: None,
                size_bytes: 0,
                runtime: None,
            }],
            publisher_pubkey_hex: Some("aa".repeat(32)),
            installed,
        }
    }

    fn flatpak_item(name: &str, installed: InstalledState) -> StoreItem {
        StoreItem {
            bot: None,
            awaiting_developer: false,
            listing: crate::model::Listing::default(),
            id: format!("org.example.{name}"),
            name: name.into(),
            description: String::new(),
            tagline: String::new(),
            supported_languages: Vec::new(),
            categories: vec![],
            icon_url: None,
            screenshot_urls: Vec::new(),
            homepage_url: None,
            source_url: None,
            install_ref_path: None,
            source: StoreSource::Flathub,
            trust: StoreTrust::Flathub,
            versions: vec![StoreItemVersion {
                version: "1.0".into(),
                released_at_unix: 0,
                download_url: None,
                root_hash_hex: None,
                surface_hex: None,
                size_bytes: 0,
                runtime: None,
            }],
            publisher_pubkey_hex: None,
            installed,
        }
    }

    /// Without Flatpak on the host, and for a Flatpak install, nothing runs here.
    #[test]
    fn actions_without_a_flatpak_host_fail_without_spawning() {
        let item = flatpak_item("Chess", InstalledState::Not);
        let out = execute(&item, ActionKind::NeedsFlatpakHost);
        assert!(!out.ok);
        assert!(out.launch.is_none());
        for action in [
            ActionKind::FlatpakInstall,
            ActionKind::FlatpakUpdate,
            ActionKind::FlatpakRemove,
        ] {
            let out = execute(&item, action);
            assert!(!out.ok, "{action:?} is the worker's change");
            assert!(out.launch.is_none());
        }
    }

    /// An installed Flatpak opens through the host, and its record names its reference.
    #[test]
    fn an_installed_flatpak_opens_on_the_host_and_names_its_reference() {
        let installed = InstalledState::Installed {
            version: "1.0".into(),
            available: "1.0".into(),
            pinned: false,
            user_managed: false,
        };
        let item = flatpak_item("Chess", installed);
        let out = execute(&item, ActionKind::FlatpakOpen);
        assert!(out.ok, "{}", out.status_line);
        assert_eq!(
            out.launch.map(|launch| launch.app_id).as_deref(),
            Some("host:org.example.Chess")
        );
        let (app, release) = flatpak_release(&item);
        assert_eq!(app, "org.example.chess");
        assert_eq!(release["version"], "stable");
        assert_eq!(
            release["flatpak"],
            format!("app/org.example.Chess/{}/stable", std::env::consts::ARCH)
        );
    }

    /// A Tilo app's version is chosen by the workspace's tree, never here.
    #[test]
    fn a_tilo_app_is_not_installed_here() {
        let item = tilo_item("Notes", InstalledState::Not);
        for action in [
            ActionKind::Get,
            ActionKind::Open,
            ActionKind::Update,
            ActionKind::Remove,
            ActionKind::MarkInstalled,
            ActionKind::Unmark,
            ActionKind::RuntimeInstall,
        ] {
            let out = execute(&item, action);
            assert!(!out.ok, "{action:?} is the worker's");
        }
    }
}
