//! The apps a workspace made on this device starts with.
//!
//! A new workspace runs the default collection: the apps `dist/apps/app-collections.toml`
//! names, compiled into [`app_fleet::default_apps`] from that one list, so a workspace made on
//! any host, in the OS image or not, starts with the same apps. Each is recorded as the release
//! the configured registries offer now — its version, the surface its changes are decided
//! against, the package root and the publisher — and never as a file the workspace keeps. A
//! device whose image already carries that package opens it without fetching; one that does not
//! fetches it by root from the workspace's registries.
//!
//! Each registry is asked for its catalogue now, verified and read without changing what this
//! device accepted; one that cannot answer stands in with the catalogue it last answered with. A
//! default app no catalogue offers — a device fresh from its install, or one offline at first use
//! — is recorded as the release the device's package cache already holds of it: its version,
//! surface, package root and publisher are all known there. Creating a workspace downloads no
//! package. A default app neither offers is left out until a registry does.

use app_cli::config::RegistriesFile;
use tilo_app_package::version::ReleaseChannel;
use tilo_peer_state::PackageRegistry;
use tilo_peer_state::workspace_membership::{AppPayload, AppRelease};

/// One accepted registry entry, the registry it came from and that registry's channel.
type Offered = (app_cli::VerifiedEntry, String, ReleaseChannel);

/// The registries a new workspace fetches apps from: the official one, pinned to the owner key
/// this device's OS image carries. An image that carries no such key starts the workspace with
/// none, and a member adds one through `workspace/registries`; no key is ever guessed.
pub(super) fn registries() -> RegistriesFile {
    RegistriesFile::seeded()
}

/// `registries` as the rows `workspace/registries` keeps.
pub(super) fn registry_rows(registries: &RegistriesFile) -> Vec<PackageRegistry> {
    registries
        .registries
        .iter()
        .map(|registry| PackageRegistry {
            name: registry.name.clone(),
            base_url: registry.base_url.clone(),
            owner_pubkey_hex: registry.owner_pubkey_hex.clone(),
            enabled: registry.enabled,
            channel: match registry.channel {
                ReleaseChannel::Stable => "stable",
                ReleaseChannel::Development => "development",
            }
            .into(),
        })
        .collect()
}

/// The apps a workspace created on this device with `registries` starts with.
pub(super) fn on_this_device(registries: &RegistriesFile) -> Vec<AppRelease> {
    resolve(app_fleet::default_apps(), &offered(registries), &cached)
}

/// Every entry the `registries`' catalogues carry, in their order: each asked now, or the one it
/// last answered with when it cannot answer.
fn offered(registries: &RegistriesFile) -> Vec<Offered> {
    let mut offered = Vec::new();
    for registry in registries.enabled() {
        let entries = app_cli::client::fresh_entries(registry)
            .or_else(|_| app_cli::client::last_verified_entries(registry));
        let Ok(entries) = entries else {
            // A registry that cannot be reached and was never listed, or whose catalogue no
            // longer verifies, offers nothing here; the device's cache may still hold the app.
            continue;
        };
        for entry in entries {
            offered.push((entry, registry.name.clone(), registry.channel));
        }
    }
    offered
}

/// The releases of `defaults` the `offered` entries resolve to, in the order `defaults` names:
/// each app's newest version this machine runs, as the Store would offer it, or the release
/// `cached` answers for one no entry offers.
fn resolve(
    defaults: &[&str],
    offered: &[Offered],
    cached: &dyn Fn(&str) -> Option<AppRelease>,
) -> Vec<AppRelease> {
    defaults
        .iter()
        .filter_map(|app| release_of(app, offered).or_else(|| from_cache(app, cached)))
        .collect()
}

/// The release of `app` the device's package cache holds, when `cached` has one that validates.
fn from_cache(app: &str, cached: &dyn Fn(&str) -> Option<AppRelease>) -> Option<AppRelease> {
    let release = cached(app)?;
    match release.validate() {
        Ok(()) => Some(release),
        Err(error) => {
            tracing::warn!(app, %error, "the cached release cannot be a default app");
            None
        }
    }
}

/// The release of `app` this device's package cache holds: the version installed, its signed
/// package root and publisher, and the surface its signed payload ships, in the form it is kept by.
fn cached(app: &str) -> Option<AppRelease> {
    let package = package_store::find_installed(app).ok()??;
    if package.package_id_hex.is_empty() || package.publisher_key_hex.is_empty() {
        // A development fixture names no signed root: nothing a workspace can bind to.
        return None;
    }
    let surface = match package_store::integrity::signed_surface(&package) {
        Some(bytes) => {
            let (_, kept) = collab_types::surface::SurfaceFile::kept(&bytes).ok()?;
            blake3::hash(kept.as_bytes()).to_hex().to_string()
        }
        None => String::new(),
    };
    Some(AppRelease {
        app: package.id.to_ascii_lowercase(),
        version: package.version,
        surface,
        package: package.package_id_hex.to_ascii_lowercase(),
        publisher: package.publisher_key_hex.to_ascii_lowercase(),
        file: None,
        dev: false,
        pinned: false,
        payload: AppPayload::Tapp,
    })
}

/// The release `app` resolves to among `offered`, or `None` when no registry offers it.
fn release_of(app: &str, offered: &[Offered]) -> Option<AppRelease> {
    for (entry, registry, channel) in offered {
        if entry.entry.package_id != app {
            continue;
        }
        let Some(running) = app_cli::entry_to_app_for_channel(entry, registry, *channel) else {
            continue;
        };
        let release = AppRelease {
            app: running.package_id.to_ascii_lowercase(),
            version: running.version,
            surface: running.tapp_surface_hex.to_ascii_lowercase(),
            package: running.tapp_root_hash_hex.to_ascii_lowercase(),
            publisher: running.publisher_pubkey_hex.to_ascii_lowercase(),
            file: None,
            dev: false,
            pinned: false,
            payload: AppPayload::Tapp,
        };
        if let Err(error) = release.validate() {
            tracing::warn!(app, %error, "the registry's release cannot be a default app");
            continue;
        }
        return Some(release);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The verified entry `id` at `version`, running the package root `root` and signed by
    /// `publisher`, its version's surface `surface` (empty for a package that publishes none).
    fn entry(id: &str, version: &str, root: &str, publisher: &str, surface: &str) -> Offered {
        let json = format!(
            r#"{{"entry":{{"package_id":"{id}","name":{{"en":"An app"}},
            "description":{{"en":"d"}},"tagline":{{"en":"t"}},"images":{{}},
            "supported_languages":["en"],"categories":[],"source_url":null,"homepage":null,
            "publisher_pubkey_hex":"{publisher}","updated_at_unix":2,"signature_hex":"",
            "versions":[{{"version":"{version}","tapp_url":"https://registry.test/{id}.tapp",
            "tapp_root_hash_hex":"{root}","tapp_size_bytes":1,"released_at_unix":1,
            "tapp_blake3_hex":"{root}","tapp_surface_hex":"{surface}",
            "target":{{"platform":"tilo-os","architecture":"any","isa":"baseline"}}}}]}},
            "trust":"PublisherSigned","version_index":null}}"#
        );
        let verified: app_cli::VerifiedEntry = serde_json::from_str(&json).unwrap();
        (verified, "Tilo main".into(), ReleaseChannel::Stable)
    }

    #[test]
    fn a_default_app_resolves_to_the_release_a_registry_offers() {
        let root = "a".repeat(64);
        let publisher = "b".repeat(64);
        let surface = "c".repeat(64);
        let offered = vec![entry("so.tilo.files", "2.1.0", &root, &publisher, &surface)];
        let releases = resolve(&["so.tilo.files"], &offered, &|_| None);
        assert_eq!(
            releases,
            vec![AppRelease {
                app: "so.tilo.files".into(),
                version: "2.1.0".into(),
                surface,
                package: root,
                publisher,
                file: None,
                dev: false,
                pinned: false,
                payload: AppPayload::Tapp,
            }]
        );
    }

    #[test]
    fn a_default_app_no_registry_offers_yet_is_left_out() {
        let root = "a".repeat(64);
        let surface = "c".repeat(64);
        let offered = vec![entry(
            "so.tilo.files",
            "2.1.0",
            &root,
            &"b".repeat(64),
            &surface,
        )];
        assert!(
            resolve(&["so.tilo.notes"], &offered, &|_| None).is_empty(),
            "a default app the catalogue does not carry yet is not recorded"
        );
    }

    #[test]
    fn a_release_with_no_surface_is_still_a_default_app() {
        let root = "a".repeat(64);
        let offered = vec![entry("so.tilo.files", "2.1.0", &root, &"b".repeat(64), "")];
        let releases = resolve(&["so.tilo.files"], &offered, &|_| None);
        assert_eq!(releases.len(), 1);
        assert_eq!(
            releases[0].surface, "",
            "a package that publishes no surface names none"
        );
    }
    #[test]
    fn with_no_catalogue_a_default_app_is_the_release_the_cache_holds() {
        let cached_release = AppRelease {
            app: "so.tilo.files".into(),
            version: "2.0.0".into(),
            surface: "c".repeat(64),
            package: "a".repeat(64),
            publisher: "b".repeat(64),
            file: None,
            dev: false,
            pinned: false,
            payload: AppPayload::Tapp,
        };
        let cache = |app: &str| (app == "so.tilo.files").then(|| cached_release.clone());
        assert_eq!(
            resolve(&["so.tilo.files", "so.tilo.notes"], &[], &cache),
            vec![cached_release.clone()],
            "a device no registry answered yet records the apps its cache holds, and only those"
        );
        let newer = entry(
            "so.tilo.files",
            "2.1.0",
            &"d".repeat(64),
            &"b".repeat(64),
            "",
        );
        assert_eq!(
            resolve(&["so.tilo.files"], &[newer], &cache)[0].version,
            "2.1.0",
            "a catalogue that answers decides over the cache"
        );
    }
}
