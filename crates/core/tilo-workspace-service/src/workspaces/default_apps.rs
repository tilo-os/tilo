//! The apps a workspace made on this device starts with.
//!
//! A new workspace runs the default collection: the apps `dist/apps/app-collections.toml`
//! names, compiled into [`fleet_catalogue::default_apps`] from that one list, so a workspace made on
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
///
/// Private: this reads each registry's catalogue with a client that blocks, so it is only ever
/// reached from [`on_this_device_awaited`]'s thread, never from a task of the app runtime.
fn on_this_device(registries: &RegistriesFile) -> Vec<AppRelease> {
    resolve_in(
        fleet_catalogue::default_apps(),
        &offered(registries),
        package_store::Catalog::from_env().ok().as_ref(),
    )
}

/// [`resolve`] in one [`package_store::Catalog`] for every app, when there is one: a catalog
/// built per lookup re-reads the system store each time, and a founding looks up every default
/// app.
fn resolve_in(
    defaults: &[&str],
    offered: &[Offered],
    catalog: Option<&package_store::Catalog>,
) -> Vec<AppRelease> {
    resolve(defaults, offered, &|app| {
        catalog.and_then(|catalog| cached(catalog, app))
    })
}

/// [`on_this_device`], with the registries asked on a thread of their own and the answer awaited.
///
/// Each catalogue is read with a blocking client, and a workspace is founded from a host
/// operation that runs on the app runtime's one thread: that thread must not hold a catalogue
/// read, and a blocking client refuses to be built on any thread of a runtime at all. The
/// thread here has no runtime, so the read is the request it is and the daemon's tasks go on.
pub(super) async fn on_this_device_awaited(registries: &RegistriesFile) -> Vec<AppRelease> {
    let asking = registries.clone();
    let (answered, heard) = tokio::sync::oneshot::channel();
    let started = std::thread::Builder::new()
        .name("tilo-registries".to_owned())
        .spawn(move || {
            let _ = answered.send(on_this_device(&asking));
        });
    let unasked = || {
        // No thread to ask on: the workspace starts with the releases this device's cache holds.
        resolve_in(
            fleet_catalogue::default_apps(),
            &[],
            package_store::Catalog::from_env().ok().as_ref(),
        )
    };
    match started {
        Ok(_) => heard.await.unwrap_or_else(|_| unasked()),
        Err(error) => {
            tracing::warn!(%error, "the registries are asked on no thread of their own");
            unasked()
        }
    }
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
/// each app's newest version this machine runs, or the release `cached` answers; one neither is
/// left out, and said plainly once.
fn resolve(
    defaults: &[&str],
    offered: &[Offered],
    cached: &dyn Fn(&str) -> Option<AppRelease>,
) -> Vec<AppRelease> {
    let releases: Vec<AppRelease> = defaults
        .iter()
        .filter_map(|app| {
            release_of(app, offered)
                .map(|release| with_known_surface(release, cached))
                .or_else(|| from_cache(app, cached))
        })
        .collect();
    let left_out: Vec<&str> = defaults
        .iter()
        .copied()
        .filter(|app| !releases.iter().any(|release| release.app == *app))
        .collect();
    // A finished founding no repository answered for is not one to complain about: a device
    // offline at first use records the apps it holds and leaves the rest until one answers.
    if !left_out.is_empty() && !offered.is_empty() {
        tracing::warn!(
            apps = left_out.join(", "),
            "no build a repository offers of these default apps runs here, and this device holds \
             none: left out"
        );
    }
    releases
}

/// `release` with the surface the device's cache knows of it, when the offer named none and the
/// cache holds that same package: its signed payload says what it publishes. An offer that names
/// none otherwise leaves it unknown, for the device that installs it to read from the bytes.
fn with_known_surface(
    mut release: AppRelease,
    cached: &dyn Fn(&str) -> Option<AppRelease>,
) -> AppRelease {
    if release.surface.is_empty()
        && let Some(held) = cached(&release.app)
        && held.package == release.package
    {
        release.surface = held.surface;
    }
    release
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

/// The release of `app` this device's package store holds, as the workspace's `install` records
/// one: what opening an app the device has but the workspace does not run yet installs. `None`
/// when the store holds no signed package of it.
#[must_use]
pub fn held_release(app: &str) -> Option<AppRelease> {
    let catalog = package_store::Catalog::from_env().ok()?;
    from_cache(app, &|app| cached(&catalog, app))
}

/// The release of `app` this device's package cache holds: the version installed, its signed
/// package root and publisher, and the compiled surface its signed payload ships.
fn cached(catalog: &package_store::Catalog, app: &str) -> Option<AppRelease> {
    let package = catalog.find(app).ok()??;
    if package.package_id_hex.is_empty() || package.publisher_key_hex.is_empty() {
        // A development fixture names no signed root: nothing a workspace can bind to.
        return None;
    }
    let surface = match package_store::integrity::signed_surface(&package) {
        Some(bytes) => {
            collab_types::surface::SurfaceFile::compiled(&bytes).ok()?;
            blake3::hash(&bytes).to_hex().to_string()
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
    fn an_offer_naming_no_surface_takes_the_one_the_cache_holds_of_that_package() {
        let root = "a".repeat(64);
        let held = |package: String| {
            move |app: &str| {
                (app == "so.tilo.files").then(|| AppRelease {
                    app: app.into(),
                    version: "2.1.0".into(),
                    surface: "c".repeat(64),
                    package: package.clone(),
                    publisher: "b".repeat(64),
                    file: None,
                    dev: false,
                    pinned: false,
                    payload: AppPayload::Tapp,
                })
            }
        };
        let offered = vec![entry("so.tilo.files", "2.1.0", &root, &"b".repeat(64), "")];
        assert_eq!(
            resolve(&["so.tilo.files"], &offered, &held(root))[0].surface,
            "c".repeat(64),
            "the package's own signed surface, read where the device holds it"
        );
        assert_eq!(
            resolve(&["so.tilo.files"], &offered, &held("d".repeat(64)))[0].surface,
            "",
            "another package's surface is never this one's: it stays unknown"
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

    /// A workspace is founded from a host operation that runs on the app runtime's one thread,
    /// and its default apps are the releases the registries offer now. Each catalogue is read
    /// with a blocking client, which refuses to be built on a thread of any runtime: asked
    /// there, the founding ends on tokio's own panic. Asked on a thread of its own, a registry
    /// that cannot answer leaves the releases the device's cache holds.
    #[test]
    fn the_registries_are_asked_off_the_runtime_that_founds_a_workspace() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let registries = RegistriesFile {
            registries: vec![app_cli::config::RegistryConfig {
                name: "unreachable".to_owned(),
                // A port nothing listens on: the read is answered at once, as no registry.
                base_url: "http://127.0.0.1:1".to_owned(),
                owner_pubkey_hex: "d".repeat(64),
                enabled: true,
                channel: ReleaseChannel::Stable,
            }],
        };
        let asked = runtime.block_on(on_this_device_awaited(&registries));
        assert_eq!(
            asked,
            resolve_in(
                fleet_catalogue::default_apps(),
                &[],
                package_store::Catalog::from_env().ok().as_ref(),
            ),
            "with no catalogue answering, a workspace starts with the releases the cache holds"
        );
    }

    /// A default app is a release this device can run: a repository's newer build for another
    /// machine is no release, so founding takes the one the device holds instead.
    #[test]
    fn a_default_app_is_a_release_this_device_can_run() {
        let (root, publisher) = ("a".repeat(64), "b".repeat(64));
        let (mut offer, registry, channel) = entry("so.tilo.files", "2.1.0", &root, &publisher, "");
        let mut target = offer.entry.versions[0].tapp_mut().unwrap().target.clone();
        target.architecture = "other".into();
        offer.entry.versions[0].tapp_mut().unwrap().target = target;
        assert_eq!(
            release_of("so.tilo.files", &[(offer, registry, channel)]),
            None
        );
    }
}
