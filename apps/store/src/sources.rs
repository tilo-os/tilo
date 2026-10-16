//! Merger: Tilo registry entries + Flathub records + local install
//! snapshot → one canonical [`StoreItem`] list.
//!
//! The merge rule the task calls out: **a Tilo entry with the same
//! `name` as a Flathub app wins.** Duplicates on the Flathub side
//! are dropped. Case-insensitive matching, whitespace-trimmed.

use registry_types::{RegistryImageKind, RegistryVersion, TappArtifact};

use crate::flathub_client::AppStreamRecord;
use crate::model::{
    BotListing, InstalledState, StoreItem, StoreItemVersion, StoreSource, StoreTrust,
};
use crate::registry_client::VerifiedEntry;
use package_store::InstalledPackage;

/// Convert a Tilo registry entry into the unified store item shape.
///
/// `registry_name` is the human label the store card shows (from
/// the user's `registries.toml`, not the wire — the registry
/// doesn't publish its own name to a fetched entry).
///
/// `installed_index` is the app host's install snapshot — passed by-ref so the merge can cheaply
/// look up install state per package id.
pub fn tilo_to_store_item(
    verified: &VerifiedEntry,
    registry_name: &str,
    installed_index: &[InstalledPackage],
) -> StoreItem {
    tilo_to_store_item_on_channel(
        verified,
        registry_name,
        installed_index,
        app_cli::ReleaseChannel::Stable,
    )
}

/// Convert only versions the device can run within the configured channel.
#[allow(
    clippy::unreachable,
    reason = "VerifiedEntry is constructed only after the registry verifier rejects Untrusted; reaching this arm would violate that security boundary"
)]
pub fn tilo_to_store_item_on_channel(
    verified: &VerifiedEntry,
    registry_name: &str,
    installed_index: &[InstalledPackage],
    channel: app_cli::ReleaseChannel,
) -> StoreItem {
    if let Some(bot) = app_cli::bots::bot_offer(verified, registry_name) {
        return tilo_bot_to_store_item(&bot);
    }
    let entry = &verified.entry;
    // A build that needs system interfaces this OS does not serve is never
    // offered: it could not run here.
    let mut versions: Vec<StoreItemVersion> = app_cli::installable_versions(entry, channel)
        .filter_map(|v| v.tapp().map(|tapp| tilo_version_to_store(v, tapp)))
        .collect();
    versions.sort_by(|left, right| app_cli::compare_versions(&right.version, &left.version));
    let latest = versions
        .first()
        .map(|v| v.version.clone())
        .unwrap_or_default();
    let installed = installed_index
        .iter()
        .find(|e| e.id == entry.package_id)
        .map_or(InstalledState::Not, |e| InstalledState::Installed {
            version: e.version.clone(),
            available: latest.clone(),
            pinned: e.pinned,
            user_managed: e.user_managed(),
        });
    let locale = preferred_locale();
    let localized_images = entry.images.resolve(&locale);
    StoreItem {
        bot: None,
        id: entry.package_id.clone(),
        name: entry
            .name
            .resolve(&locale)
            .unwrap_or(&entry.package_id)
            .to_string(),
        description: entry
            .description
            .resolve(&locale)
            .unwrap_or_default()
            .to_string(),
        tagline: entry
            .tagline
            .resolve(&locale)
            .unwrap_or_default()
            .to_string(),
        supported_languages: entry.supported_languages.clone(),
        categories: entry.categories.clone(),
        icon_url: localized_images
            .iter()
            .find(|image| image.kind == RegistryImageKind::Icon)
            .map(|image| image.url.clone()),
        screenshot_urls: localized_images
            .iter()
            .filter(|image| image.kind == RegistryImageKind::Screenshot)
            .map(|image| image.url.clone())
            .collect(),
        homepage_url: entry.homepage.clone(),
        source_url: entry.source_url.clone(),
        install_ref_path: None,
        source: StoreSource::Tilo {
            registry_name: registry_name.to_string(),
        },
        trust: match verified.trust {
            crate::registry_client::TrustLevel::PublisherSigned => StoreTrust::PublisherSigned,
            crate::registry_client::TrustLevel::Untrusted => {
                unreachable!("untrusted registry entry reached Store conversion")
            }
        },
        versions,
        publisher_pubkey_hex: Some(entry.publisher_pubkey_hex.clone()),
        installed,
    }
}

/// A directory bot's card: what it does and needs, never an app version to
/// download, because a bot runs in a room rather than on this device.
pub fn tilo_bot_to_store_item(bot: &app_cli::bots::CatalogueBot) -> StoreItem {
    let requires = &bot.artifact.requires;
    StoreItem {
        id: bot.package_id.clone(),
        name: bot.name.clone(),
        description: bot.description.clone(),
        tagline: bot.tagline.clone(),
        supported_languages: Vec::new(),
        categories: vec!["Bots".into()],
        icon_url: bot.icon_url.clone(),
        screenshot_urls: Vec::new(),
        homepage_url: None,
        source_url: None,
        install_ref_path: None,
        source: StoreSource::Tilo {
            registry_name: bot.registry_name.clone(),
        },
        // `bot_offer` lists only publisher-signed entries.
        trust: StoreTrust::PublisherSigned,
        versions: Vec::new(),
        publisher_pubkey_hex: Some(bot.publisher_pubkey_hex.clone()),
        installed: InstalledState::Not,
        bot: Some(BotListing {
            version: bot.version.clone(),
            triggers: requires.triggers.clone(),
            apps: requires.apps.clone(),
            connectors: requires.connectors.clone(),
            appearance: requires.appearance,
            artifact: bot.artifact.clone(),
        }),
    }
}

fn preferred_locale() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok())
        .and_then(|value| {
            let tag = value.split('.').next()?.replace('_', "-");
            registry_types::valid_language_tag(&tag).then_some(tag)
        })
        .unwrap_or_else(|| "en".into())
}

/// The source label of an installed Tilo app no repository currently
/// describes; the details page explains why, once.
pub const INSTALLED_ON_DEVICE: &str = "Installed on this device";

/// Turn an installed Tilo package into a local-only Store row.
///
/// A verified catalogue entry normally supplies the description, artwork and
/// update metadata. That entry is not guaranteed to be reachable forever,
/// though: a user must still be able to launch, pin, or remove software that
/// is already on their device while its registry is offline or has withdrawn
/// it. The package's own signed manifest (`about`) still names and files the
/// app, and the installed tree has a size, so the row says what the app is.
/// It publishes no download URL or claimed latest version, so the UI cannot
/// offer an update based on stale or invented metadata.
pub fn installed_tilo_to_store_item(
    entry: &InstalledPackage,
    about: Option<&app_host::client::PackageAbout>,
) -> StoreItem {
    let locale = preferred_locale();
    StoreItem {
        bot: None,
        id: entry.id.clone(),
        name: about.map_or_else(
            || package_store::display_name_for_package_id(&entry.id),
            |about| about.name_for(&locale).to_owned(),
        ),
        // The manifest has no description. Saying so on every row is noise;
        // the details page names the repository that has not answered.
        description: String::new(),
        tagline: String::new(),
        supported_languages: Vec::new(),
        categories: about
            .and_then(|about| about.category.clone())
            .into_iter()
            .collect(),
        icon_url: None,
        screenshot_urls: Vec::new(),
        homepage_url: None,
        source_url: None,
        install_ref_path: None,
        source: StoreSource::Tilo {
            registry_name: INSTALLED_ON_DEVICE.into(),
        },
        // app-host accepts only packages whose signature and declared package
        // identity verified at installation. This is not a current registry
        // review claim; the detail sheet spells that distinction out.
        trust: StoreTrust::PublisherSigned,
        versions: vec![StoreItemVersion {
            version: entry.version.clone(),
            released_at_unix: 0,
            download_url: None,
            root_hash_hex: None,
            size_bytes: entry.payload_bytes,
            runtime: None,
        }],
        publisher_pubkey_hex: (!entry.publisher_key_hex.trim().is_empty())
            .then(|| entry.publisher_key_hex.clone()),
        installed: InstalledState::Installed {
            version: entry.version.clone(),
            // The only truthful available version while offline is unknown.
            // Keeping it equal to the installed version prevents a fake
            // "Update" affordance.
            available: entry.version.clone(),
            pinned: entry.pinned,
            user_managed: entry.user_managed(),
        },
    }
}

fn tilo_version_to_store(v: &RegistryVersion, tapp: &TappArtifact) -> StoreItemVersion {
    StoreItemVersion {
        version: v.version.clone(),
        released_at_unix: v.released_at_unix,
        download_url: Some(tapp.tapp_url.clone()),
        root_hash_hex: Some(tapp.tapp_root_hash_hex.clone()),
        size_bytes: tapp.tapp_size_bytes,
        runtime: tapp.target.runtime.clone(),
    }
}

/// Return a local-only row for an installed Flatpak absent from the current
/// popular/search result. As with [`installed_tilo_to_store_item`], action
/// availability comes from the host; update metadata remains unknown rather
/// than being guessed from an unrelated network response.
pub fn installed_flatpak_to_item(app_id: &str, version: &str) -> StoreItem {
    StoreItem {
        bot: None,
        id: app_id.to_string(),
        name: app_id.to_string(),
        description: "Installed Flatpak. Catalogue details are unavailable.".into(),
        tagline: String::new(),
        supported_languages: Vec::new(),
        categories: Vec::new(),
        icon_url: None,
        screenshot_urls: Vec::new(),
        homepage_url: None,
        source_url: None,
        install_ref_path: None,
        source: StoreSource::Flathub,
        trust: StoreTrust::Flathub,
        versions: vec![StoreItemVersion {
            version: version.to_string(),
            released_at_unix: 0,
            download_url: None,
            root_hash_hex: None,
            size_bytes: 0,
            runtime: None,
        }],
        publisher_pubkey_hex: None,
        installed: InstalledState::Installed {
            version: version.to_string(),
            available: version.to_string(),
            pinned: false,
            user_managed: true,
        },
    }
}

/// Decode cached `AppStream` metadata from an actual configured repository.
/// No unavailable release dates, artwork or publisher identity are invented.
pub fn remote_catalogue_items(
    rows: &str,
    remote: &crate::flathub_client::Remote,
    installed: &[(String, String)],
) -> Vec<StoreItem> {
    rows.lines()
        .filter_map(|line| {
            let columns: Vec<_> = line.split('\t').collect();
            let id = columns.first()?.trim();
            if id.is_empty() {
                return None;
            }
            let version = columns.get(3).copied().unwrap_or("").trim();
            let mut item = installed_flatpak_to_item(id, version);
            item.name = columns
                .get(1)
                .filter(|name| !name.trim().is_empty())
                .map_or(id, |name| name.trim())
                .to_string();
            item.description = columns.get(2).copied().unwrap_or("").trim().to_string();
            item.installed = installed.iter().find(|(app, _)| app == id).map_or(
                InstalledState::Not,
                |(_, installed)| InstalledState::Installed {
                    version: installed.clone(),
                    available: version.to_string(),
                    pinned: false,
                    user_managed: remote.user_managed,
                },
            );
            item.source = StoreSource::FlatpakRemote {
                name: remote.name.clone(),
                title: remote.title.clone(),
                branch: columns.get(4).copied().unwrap_or("stable").to_string(),
                user_managed: remote.user_managed,
            };
            item.trust = StoreTrust::ConfiguredRemote;
            if remote.name == "flathub"
                && columns.get(4).is_none_or(|branch| *branch == "stable")
                && matches!(
                    remote.url.trim_end_matches('/'),
                    "https://dl.flathub.org/repo" | "https://flathub.org/repo"
                )
            {
                item.source = StoreSource::Flathub;
                item.trust = StoreTrust::Flathub;
            }
            Some(item)
        })
        .collect()
}

/// Preserve the scope of apps already supplied by the device.
pub fn apply_flatpak_origin<S: std::hash::BuildHasher>(
    item: &mut StoreItem,
    origins: &std::collections::HashMap<String, (String, bool), S>,
) {
    if let Some((origin, user_managed)) = origins.get(&item.id) {
        if let InstalledState::Installed {
            user_managed: mutable,
            ..
        } = &mut item.installed
        {
            *mutable = *user_managed;
        }
        if origin != "flathub" {
            item.source = StoreSource::FlatpakRemote {
                name: origin.clone(),
                title: origin.clone(),
                branch: "stable".into(),
                user_managed: *user_managed,
            };
            item.trust = StoreTrust::ConfiguredRemote;
        }
    }
}

/// Turn a full appstream record into a store item (the enriched
/// form used once we've fetched details).
pub fn flathub_record_to_item(
    r: &AppStreamRecord,
    installed_flatpaks: &[(String, String)],
) -> StoreItem {
    let categories: Vec<String> = r
        .categories
        .iter()
        .filter(|c| !c.name.trim().is_empty())
        .map(|c| c.name.clone())
        .collect();
    let latest = r.current_release_version.clone().unwrap_or_default();
    let installed = installed_flatpaks
        .iter()
        .find(|(id, _)| id == &r.app_id)
        .map_or(InstalledState::Not, |(_, v)| InstalledState::Installed {
            version: v.clone(),
            available: latest.clone(),
            pinned: false,
            user_managed: true,
        });
    let homepage_url = r
        .homepage
        .clone()
        .or_else(|| r.urls.as_ref().and_then(|u| u.homepage.clone()));
    StoreItem {
        bot: None,
        id: r.app_id.clone(),
        name: if r.name.is_empty() {
            r.app_id.clone()
        } else {
            r.name.clone()
        },
        description: if r.description.is_empty() {
            r.summary.clone()
        } else {
            plain_text(&r.description)
        },
        tagline: r.summary.clone(),
        supported_languages: Vec::new(),
        categories,
        icon_url: r.icon.clone(),
        screenshot_urls: r
            .screenshots
            .iter()
            .filter_map(|shot| {
                shot.img_desktop_url
                    .clone()
                    .or_else(|| shot.img_mobile_url.clone())
            })
            .take(6)
            .collect(),
        homepage_url,
        source_url: None,
        install_ref_path: None,
        source: StoreSource::Flathub,
        trust: StoreTrust::Flathub,
        versions: vec![StoreItemVersion {
            version: latest,
            released_at_unix: 0,
            download_url: None,
            root_hash_hex: None,
            size_bytes: 0,
            runtime: None,
        }],
        publisher_pubkey_hex: None,
        installed,
    }
}

fn plain_text(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_tag = false;
    for character in input.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    output.trim().to_string()
}

/// Merge a Tilo pile and a Flathub pile into one deduped list.
/// Prefer native offers on name collisions, while retaining every installed app.
pub fn merge(tilo_items: Vec<StoreItem>, flathub_items: Vec<StoreItem>) -> Vec<StoreItem> {
    let mut out = tilo_items;
    let tilo_names: std::collections::HashSet<String> = out
        .iter()
        .map(|i| i.name.trim().to_ascii_lowercase())
        .collect();
    for fh in flathub_items {
        if fh.installed.is_installed()
            || fh.install_ref_path.is_some()
            || !tilo_names.contains(&fh.name.trim().to_ascii_lowercase())
        {
            out.push(fh);
        }
    }
    out
}

/// Distinct categories across a set of items, alpha-sorted, useful
/// for driving the derived category chip row.
pub fn distinct_categories(items: &[StoreItem]) -> Vec<String> {
    let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for item in items {
        for c in &item.categories {
            let t = c.trim();
            if !t.is_empty() {
                set.insert(t.to_string());
            }
        }
    }
    set.into_iter().collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::registry_client::TrustLevel;
    use ed25519_dalek::SigningKey;
    use registry_types::{RegistryEntry, sign_entry};

    #[test]
    fn configured_remote_catalogue_preserves_metadata_and_device_scope() {
        let mut remote = crate::flathub_client::Remote {
            name: "tools".into(),
            title: "Example tools".into(),
            url: "https://tools.example/repo".into(),
            disabled: false,
            user_managed: true,
            catalogue_error: None,
        };
        let rows = "org.example.Editor\tEditor\tEdit local documents\t2.0\tbeta\ttools\n";
        let mut items = super::remote_catalogue_items(rows, &remote, &[]);
        let item = &mut items[0];
        assert_eq!(item.name, "Editor");
        assert_eq!(item.description, "Edit local documents");
        assert_eq!(item.source.distribution_name(), "Example tools");
        assert_eq!(item.trust, StoreTrust::ConfiguredRemote);
        assert!(
            matches!(&item.source, StoreSource::FlatpakRemote { branch, .. } if branch == "beta")
        );
        assert!(item.allows_action(crate::model::ActionKind::FlatpakInstall, true));
        remote.user_managed = false;
        let item = &super::remote_catalogue_items(rows, &remote, &[])[0];
        assert!(item.managed_by_device(crate::model::ActionKind::FlatpakInstall));
        assert!(!item.allows_action(crate::model::ActionKind::FlatpakInstall, true));
        let mut installed = super::remote_catalogue_items(
            rows,
            &remote,
            &[("org.example.Editor".into(), "1.0".into())],
        )
        .remove(0);
        let origins =
            crate::flathub_client::installed_origins("org.example.Editor\t1.0\ttools\tsystem\n");
        super::apply_flatpak_origin(&mut installed, &origins);
        assert_eq!(
            installed.action_kind(true),
            crate::model::ActionKind::FlatpakOpen
        );
        assert!(installed.remove_action().is_none());
        assert!(!installed.allows_action(crate::model::ActionKind::FlatpakUpdate, true));
        assert!(crate::model::SourceFilter::Flatpak.accepts(&installed));
        assert!(installed.icon_url.is_none());
        assert!(installed.publisher_pubkey_hex.is_none());
    }

    #[test]
    fn official_system_flathub_stable_items_use_the_account_install_route() {
        let remote = crate::flathub_client::Remote {
            name: "flathub".into(),
            title: "Flathub".into(),
            url: "https://dl.flathub.org/repo/".into(),
            disabled: false,
            user_managed: false,
            catalogue_error: None,
        };
        let rows = "org.example.Editor\tEditor\tEdit documents\t2.0\tstable\tflathub\n";
        let item = super::remote_catalogue_items(rows, &remote, &[]).remove(0);
        assert!(matches!(item.source, StoreSource::Flathub));
        assert!(item.allows_action(crate::ActionKind::FlatpakInstall, true));
        let beta = rows.replace("stable", "beta");
        let item = super::remote_catalogue_items(&beta, &remote, &[]).remove(0);
        assert!(matches!(
            item.source,
            StoreSource::FlatpakRemote {
                user_managed: false,
                ..
            }
        ));
        assert!(!item.allows_action(crate::ActionKind::FlatpakInstall, true));
    }

    #[test]
    fn tapp_versions_keep_their_runtime_pin_and_runtime_packages_offer_install() {
        let mut entry = tilo_entry("so.tilo.runtime.chromium", "Chromium", "1.0.0", [1; 32]);
        let runtime = "tilo-runtime/2026.10.10-chromium";
        let version = &mut entry.versions[0];
        version.tapp_mut().unwrap().target.runtime = Some(runtime.into());
        let stored = tilo_version_to_store(version, version.tapp().unwrap());
        assert_eq!(stored.runtime.as_deref(), Some(runtime));
        let item = tilo_to_store_item(&verified(entry), "Tilo main", &[]);
        assert!(item.bot.is_none());
        assert_eq!(
            item.action_kind(true),
            crate::model::ActionKind::RuntimeInstall
        );
    }

    #[test]
    fn native_catalogue_matches_cli_target_channel_and_version_selection() {
        let mut entry = tilo_entry("so.tilo.notes", "Notes", "1.0.0", [1; 32]);
        let mut foreign = entry.versions[0].clone();
        foreign.version = "9.0.0".into();
        foreign.tapp_mut().unwrap().target.architecture = "different-device".into();
        let mut newest = entry.versions[0].clone();
        newest.version = "2.0.0".into();
        let mut prerelease = entry.versions[0].clone();
        prerelease.version = "3.0.0-beta.1".into();
        entry.versions = vec![foreign, entry.versions[0].clone(), prerelease, newest];
        let verified = verified(entry);
        let item = tilo_to_store_item(&verified, "Tilo main", &[]);
        assert_eq!(item.latest_version(), "2.0.0");
        assert_eq!(item.versions.len(), 2);
        assert_eq!(
            app_cli::entry_to_app(&verified, "Tilo main")
                .unwrap()
                .version,
            item.latest_version()
        );
    }

    pub(crate) fn tilo_entry(
        id: &str,
        name: &str,
        version: &str,
        publisher: [u8; 32],
    ) -> RegistryEntry {
        let mut e = RegistryEntry {
            package_id: id.into(),
            kind: registry_types::EntryKind::App,
            name: name.into(),
            description: format!("Description of {name}").into(),
            tagline: format!("{name} for Tilo OS").into(),
            images: registry_types::LocalizedImages::default(),
            supported_languages: vec!["en".into()],
            categories: vec!["Productivity".into()],
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: hex::encode(publisher),
            versions: vec![RegistryVersion {
                version: version.into(),
                released_at_unix: 200,
                artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                    tapp_url: format!("https://apps.tilo.so/{id}/{version}.tapp"),
                    tapp_root_hash_hex: "cd".repeat(32),
                    tapp_size_bytes: 100,
                    tapp_blake3_hex: "ef".repeat(32),
                    target: registry_types::RegistryTarget {
                        platform: registry_types::TILO_OS_PLATFORM.into(),
                        architecture: std::env::consts::ARCH.into(),
                        isa: std::env::consts::ARCH.into(),
                        runtime: None,
                        interfaces: Vec::new(),
                    },
                }),
            }],
            updated_at_unix: 300,
            signature_hex: String::new(),
        };
        let key = SigningKey::from_bytes(&publisher);
        // Update the pubkey hex to match the SigningKey's public half.
        e.publisher_pubkey_hex = hex::encode(key.verifying_key().to_bytes());
        sign_entry(&mut e, &key);
        e
    }

    pub(crate) fn installed_package(
        id: impl Into<String>,
        version: impl Into<String>,
        publisher_key_hex: impl Into<String>,
        pinned: bool,
        user_managed: bool,
    ) -> InstalledPackage {
        InstalledPackage {
            id: id.into(),
            version: version.into(),
            package_id_hex: String::new(),
            payload_root: std::path::PathBuf::new(),
            manifest_path: std::path::PathBuf::new(),
            publisher_key_hex: publisher_key_hex.into(),
            pinned,
            installed_at: 0,
            last_used: 0,
            attested_by: String::new(),
            cli_commands: Vec::new(),
            runtime: String::new(),
            source: String::new(),
            held: false,
            payload_bytes: 0,
            system: !user_managed,
        }
    }

    #[test]
    fn a_bot_entry_becomes_a_bot_card_and_never_an_app() {
        let bot = app_cli::bots::CatalogueBot {
            package_id: "so.tilo.bots.bridge".into(),
            name: "Discord bridge".into(),
            tagline: "Mirror a Discord server".into(),
            description: "Bridges channels".into(),
            icon_url: None,
            publisher_pubkey_hex: "aa".repeat(32),
            registry_name: "Tilo".into(),
            version: "1.0.0".into(),
            artifact: registry_types::BotArtifact {
                program_url: format!("https://apps.tilo.so/blobs/{}", "ab".repeat(32)),
                program_blake3_hex: "ab".repeat(32),
                program_size_bytes: 10,
                requires: registry_types::BotRequirements {
                    connectors: vec![registry_types::PackageRequirement {
                        package_id: "so.tilo.discord".into(),
                        min_version: "0.1.0".into(),
                    }],
                    apps: vec!["so.tilo.chat".into()],
                    appearance: true,
                    triggers: vec!["connector".into(), "record_changed".into()],
                },
            },
        };
        let item = tilo_bot_to_store_item(&bot);
        let listing = item.bot.as_ref().unwrap();
        assert!(listing.appearance);
        assert_eq!(listing.connectors[0].package_id, "so.tilo.discord");
        assert!(
            item.versions.is_empty(),
            "a bot has no downloadable app version"
        );
        assert_eq!(item.action_kind(true), crate::model::ActionKind::AddBot);
        assert!(
            item.unavailable_action(crate::model::ActionKind::AddBot)
                .is_some()
        );
    }

    pub(crate) fn verified(entry: RegistryEntry) -> VerifiedEntry {
        VerifiedEntry {
            entry,
            trust: TrustLevel::PublisherSigned,
            version_index: None,
        }
    }

    /// Registry entries resolve installed, update, immutable and locally-installed states.
    #[test]
    fn catalogue_entries_resolve_install_and_update_state() {
        // Case `tilo_entry_deserializes_installed_state`.
        {
            let entry = tilo_entry("so.tilo.notes", "Notes", "0.1.0", [1u8; 32]);
            let installed = vec![installed_package(
                "so.tilo.notes",
                "0.1.0",
                entry.publisher_pubkey_hex.clone(),
                true,
                true,
            )];
            let item = tilo_to_store_item(&verified(entry), "Tilo main", &installed);
            assert!(matches!(item.installed, InstalledState::Installed { .. }));
            // The pin flag from tilo-install's registry must survive the
            // merge so the store can show "marked installed".
            assert!(item.installed.is_pinned());
            assert_eq!(item.latest_version(), "0.1.0");
            assert!(matches!(item.source, StoreSource::Tilo { .. }));
        }

        // Case `tilo_entry_with_newer_registry_version_flags_update`.
        {
            let entry = tilo_entry("so.tilo.notes", "Notes", "0.2.0", [1u8; 32]);
            let installed = vec![installed_package(
                "so.tilo.notes",
                "0.1.0",
                entry.publisher_pubkey_hex.clone(),
                false,
                true,
            )];
            let item = tilo_to_store_item(&verified(entry), "Tilo main", &installed);
            assert!(item.installed.update_available());
        }

        // Case `immutable_system_entry_is_installed_but_not_mutable`.
        {
            let entry = tilo_entry("so.tilo.settings", "Settings", "1.0.0", [2u8; 32]);
            let installed = vec![installed_package(
                entry.package_id.clone(),
                "1.0.0",
                entry.publisher_pubkey_hex.clone(),
                false,
                false,
            )];
            let item = tilo_to_store_item(&verified(entry), "Tilo system", &installed);
            assert!(item.installed.is_installed());
            assert_eq!(item.action_kind(true), crate::model::ActionKind::Open);
            assert!(item.pin_action().is_none());
            assert!(item.remove_action().is_none());
        }

        // Case `locally_installed_tilo_item_stays_actionable_without_catalogue_metadata`.
        {
            let item = installed_tilo_to_store_item(
                &installed_package("so.tilo.offline", "1.2.3", "ab".repeat(32), true, true),
                None,
            );
            // Without a manifest the id still yields a readable name, never
            // the raw reverse-DNS id or a sentence about missing metadata.
            assert_eq!(item.name, "Offline");
            assert!(item.description.is_empty());
            assert!(item.installed.is_installed());
            assert!(item.installed.is_pinned());
            assert!(!item.installed.update_available());
            assert_eq!(item.action_kind(true), crate::model::ActionKind::Open);
            assert_eq!(item.remove_action(), Some(crate::model::ActionKind::Remove));
            assert!(item.versions[0].download_url.is_none());
        }

        // Case `locally_installed_flatpak_stays_actionable_without_a_popular_feed_row`.
        {
            let item = installed_flatpak_to_item("org.example.Offline", "4.0");
            assert_eq!(item.name, "org.example.Offline");
            assert!(item.installed.is_installed());
            assert!(!item.installed.update_available());
            assert_eq!(
                item.action_kind(true),
                crate::model::ActionKind::FlatpakOpen
            );
            assert_eq!(
                item.remove_action(),
                Some(crate::model::ActionKind::FlatpakRemove)
            );
        }
    }

    /// Merging, category dedup and `AppStream` descriptions.
    #[test]
    fn source_merging_dedups_and_describes() {
        let flathub_item = |app_id: &str, name: &str| {
            let record = AppStreamRecord {
                app_id: app_id.into(),
                name: name.into(),
                summary: String::new(),
                description: String::new(),
                icon: None,
                current_release_version: None,
                categories: Vec::new(),
                homepage: None,
                urls: None,
                screenshots: Vec::new(),
            };
            flathub_record_to_item(&record, &[])
        };
        // Case `merge_dedups_by_name_tilo_wins`.
        {
            let tilo_entry = tilo_entry("so.tilo.notes", "Notes", "0.1.0", [1u8; 32]);
            let tilo = tilo_to_store_item(&verified(tilo_entry), "Tilo main", &[]);
            // Flathub carries a same-name item; it must be dropped.
            let fh_item = flathub_item("com.other.Notes", "Notes");
            let merged = merge(vec![tilo], vec![fh_item]);
            assert_eq!(merged.len(), 1);
            assert!(matches!(merged[0].source, StoreSource::Tilo { .. }));
        }

        // Case `merge_preserves_distinct_flathub_items`.
        {
            let q = tilo_to_store_item(
                &verified(tilo_entry("so.tilo.notes", "Notes", "0.1.0", [1u8; 32])),
                "Tilo main",
                &[],
            );
            let f = flathub_item("org.videolan.VLC", "VLC");
            let merged = merge(vec![q], vec![f]);
            assert_eq!(merged.len(), 2);
        }

        // Case `distinct_categories_sorted_and_deduped`.
        {
            let a =
                tilo_to_store_item(&verified(tilo_entry("a", "A", "1", [2u8; 32])), "Tilo", &[]);
            let mut b =
                tilo_to_store_item(&verified(tilo_entry("b", "B", "1", [3u8; 32])), "Tilo", &[]);
            b.categories = vec!["Games".into(), "Productivity".into()];
            let cats = distinct_categories(&[a, b]);
            assert_eq!(cats, vec!["Games".to_string(), "Productivity".to_string()]);
        }

        // Case `appstream_detail_prefers_plain_long_description`.
        {
            let record = AppStreamRecord {
                app_id: "org.example.App".into(),
                name: "Example".into(),
                summary: "Short".into(),
                description: "<p>Long description</p>".into(),
                icon: None,
                current_release_version: Some("2".into()),
                categories: Vec::new(),
                homepage: None,
                urls: None,
                screenshots: Vec::new(),
            };
            assert_eq!(
                flathub_record_to_item(&record, &[]).description,
                "Long description"
            );
        }
    }
}
