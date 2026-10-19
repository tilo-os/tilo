//! `app-cli` — what software this machine can open but has not
//! installed yet, and how to get it.
//!
//! The desktop's applications list is "what is in the `tilo-install`
//! registry" — already installed. This crate is the other half: what
//! the machine's configured registries (the user's `registries.toml`, seeded
//! from the pinned official source on first use) describe,
//! minus what's already installed. No registries configured means an
//! empty catalogue, correctly.
//!
//! [`main`](../src/main.rs) builds `app`, and the compositor
//! shells out to it rather than linking this crate — listing means
//! HTTP, installing means HTTP plus signature verification plus
//! archive extraction, none of which belongs in the most privileged
//! process (same precedent as the desktop execing `tilo-launcher`
//! instead of building sandboxes itself).
//!
//! This crate never decides whether bytes are safe to extract:
//! [`install::install`] downloads to a staging file and hands the
//! path to `tilo-install`, which checks the publisher signature
//! fail-closed before extracting anything; `--allow-self-signed` is never passed.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

/// Release selection shared by catalogue clients and the verified installer.
pub use tilo_app_package::version::{ReleaseChannel, compare as compare_versions};

pub mod bots;
pub mod client;
pub mod config;
pub mod install;

use serde::{Deserialize, Serialize};

pub use client::{TrustLevel, VerifiedEntry};
pub use config::{RegistriesFile, RegistryConfig};
pub use registry_types::catalogue::{CatalogueApp, CatalogueListing, UnreachableRegistry};

/// Turn a verified registry entry into a catalogue row, or `None` if
/// it can't be offered: its metadata signature didn't verify, or it
/// advertises no version to download.
pub fn entry_to_app(v: &VerifiedEntry, registry_name: &str) -> Option<CatalogueApp> {
    if !v.trust.is_offerable() {
        return None;
    }
    entry_to_app_for_channel(
        v,
        registry_name,
        tilo_app_package::version::ReleaseChannel::Stable,
    )
}

/// Select versions within the user's explicit release channel.
pub fn entry_to_app_for_channel(
    v: &VerifiedEntry,
    registry_name: &str,
    channel: tilo_app_package::version::ReleaseChannel,
) -> Option<CatalogueApp> {
    if !v.trust.is_offerable() || registry_types::is_runtime_package_id(&v.entry.package_id) {
        return None;
    }
    // Artifact version orders releases. A later publication timestamp must
    // not make an older semantic/CalVer artifact the offered update.
    let newest = installable_versions(&v.entry, channel)
        .max_by(|a, b| tilo_app_package::version::compare(&a.version, &b.version))?;
    app_for_version(v, newest, registry_name)
}

/// The versions of `entry` this machine can run on `channel`.
pub fn installable_versions(
    entry: &registry_types::RegistryEntry,
    channel: tilo_app_package::version::ReleaseChannel,
) -> impl Iterator<Item = &registry_types::RegistryVersion> {
    installable_versions_on(entry, channel, os_interfaces())
}

/// The system interfaces this OS serves, read once per process.
fn os_interfaces() -> Option<&'static registry_types::ProvidedInterfaces> {
    static PROVIDED: std::sync::OnceLock<Option<registry_types::ProvidedInterfaces>> =
        std::sync::OnceLock::new();
    PROVIDED
        .get_or_init(package_store::interfaces::system_interfaces)
        .as_ref()
}

/// [`installable_versions`] on an OS serving `provided`: a version that
/// requires an interface version the OS does not serve is not offered, so
/// an update never takes an app past what its system can run.
fn installable_versions_on<'a>(
    entry: &'a registry_types::RegistryEntry,
    channel: tilo_app_package::version::ReleaseChannel,
    provided: Option<&'a registry_types::ProvidedInterfaces>,
) -> impl Iterator<Item = &'a registry_types::RegistryVersion> {
    let runtime_package = registry_types::is_runtime_package_id(&entry.package_id);
    // Only versions that bind a tapp: a bot entry's program is never an app.
    entry.versions.iter().filter(move |version| {
        version.tapp().is_some_and(|tapp| {
            !tapp.tapp_url.trim().is_empty()
                && target_supported(tapp)
                && (runtime_package || runtime_named(tapp))
                && provided.is_none_or(|provided| provided.meets_wire(&tapp.target.interfaces))
        }) && channel.accepts(&version.version)
    })
}

/// Whether `entry` is waiting for an update from its developer on `channel`:
/// it publishes builds for this machine, none of them fits the interface
/// versions this OS serves, and the newest is behind them (built against
/// older versions), so only a new build from its developer can fit
/// (docs/architecture/compatibility.md).
pub fn awaiting_developer(
    entry: &registry_types::RegistryEntry,
    channel: tilo_app_package::version::ReleaseChannel,
) -> bool {
    awaiting_developer_on(entry, channel, os_interfaces())
}

/// [`awaiting_developer`] on an OS serving `provided`. A host that declares
/// no interfaces (a development machine) judges nothing, so nothing waits.
fn awaiting_developer_on(
    entry: &registry_types::RegistryEntry,
    channel: tilo_app_package::version::ReleaseChannel,
    provided: Option<&registry_types::ProvidedInterfaces>,
) -> bool {
    let Some(provided) = provided else {
        return false;
    };
    if installable_versions_on(entry, channel, Some(provided))
        .next()
        .is_some()
    {
        return false;
    }
    let runtime_package = registry_types::is_runtime_package_id(&entry.package_id);
    entry
        .versions
        .iter()
        .filter(|version| channel.accepts(&version.version))
        .filter_map(|version| version.tapp().map(|tapp| (version, tapp)))
        .filter(|(_, tapp)| {
            !tapp.tapp_url.trim().is_empty()
                && target_supported(tapp)
                && (runtime_package || runtime_named(tapp))
        })
        .max_by(|(a, _), (b, _)| tilo_app_package::version::compare(&a.version, &b.version))
        .is_some_and(|(_, tapp)| {
            registry_types::interfaces::package_is_behind(
                &provided.unmet_wire(&tapp.target.interfaces),
            )
        })
}

/// Whether an app version says which runtime build it needs, when it needs
/// one. A thin app links the runtime's Rust dylibs, which have no stable
/// ABI, so a version that names only a release (`tilo-runtime/2026.07`)
/// cannot be matched to a runtime this device can provide; installing it is
/// how every system app once stopped starting.
fn runtime_named(tapp: &registry_types::TappArtifact) -> bool {
    tapp.target.runtime.is_none() || tapp.target.native_runtime().is_some()
}

/// Whether the runtime `runtime` (`<name>/<version>`) is installed where the
/// launcher looks for it: the user's runtime directory or the system image.
pub fn runtime_installed(runtime: &str) -> bool {
    runtime
        .split_once('/')
        .is_some_and(|(_, version)| runtime_bundle::resolve_bundle_path(version).is_some())
}

/// The version of an app to install when it is opened for the first time:
/// `newest` when it runs on a runtime this device already has, else the
/// newest offered version that does, else `newest`.
///
/// A thin app runs only against the exact runtime build it names, and the
/// runtime is rebuilt with every release, so the newest version usually names
/// a runtime no device has yet: opening it would first download the whole
/// runtime (hundreds of megabytes) for an app of a few. Every candidate comes
/// from the same publisher-signed entry and is installed through the same
/// verified pipeline; this chooses among signed versions, it does not relax
/// which runtime a version may run on. An update (`app update`) still moves
/// the app to the newest version and its runtime.
#[must_use]
pub fn first_open_offer(newest: &CatalogueApp) -> CatalogueApp {
    if newest.runtime.as_deref().is_none_or(runtime_installed) {
        return newest.clone();
    }
    // Without the version index the newest version is still a correct
    // answer, only a slower one.
    version_offers(&newest.package_id, &newest.registry)
        .ok()
        .and_then(|offers| {
            prefer_installed_runtime(&offers, &newest.version, runtime_installed).cloned()
        })
        .unwrap_or_else(|| newest.clone())
}

/// The newest of `offers`, no newer than `ceiling`, that needs no runtime or
/// one `installed` reports present.
pub fn prefer_installed_runtime<'a>(
    offers: impl IntoIterator<Item = &'a CatalogueApp>,
    ceiling: &str,
    installed: impl Fn(&str) -> bool,
) -> Option<&'a CatalogueApp> {
    offers
        .into_iter()
        .filter(|offer| tilo_app_package::version::compare(&offer.version, ceiling).is_le())
        .filter(|offer| offer.runtime.as_deref().is_none_or(&installed))
        .max_by(|a, b| tilo_app_package::version::compare(&a.version, &b.version))
}

/// The registry `registry_name`'s offer of the runtime `runtime`: a version
/// of one of its runtime packages that provides exactly that runtime for
/// this machine.
///
/// # Errors
///
/// Returns an error when no enabled registry has that name or it cannot be
/// read or fails a proof.
pub fn runtime_offer(runtime: &str, registry_name: &str) -> Result<Option<CatalogueApp>, String> {
    let registries = config::load_registries();
    let registry = registries
        .enabled()
        .find(|registry| registry.name == registry_name)
        .ok_or_else(|| format!("no enabled repository is named '{registry_name}'"))?;
    let snapshot = client::fetch_snapshot(registry).map_err(|error| error.to_string())?;
    let runtime_packages: Vec<String> = snapshot
        .entries()
        .into_iter()
        .map(|listed| listed.entry.package_id.clone())
        .filter(|id| registry_types::is_runtime_package_id(id))
        .collect();
    // One verified snapshot answers for every runtime package; only the
    // per-package version indexes differ, and those are independent requests.
    let verified: Vec<Result<VerifiedEntry, String>> = std::thread::scope(|scope| {
        // Every request is started before the first is awaited: iterating the
        // joins lazily would run them one after another.
        #[expect(
            clippy::needless_collect,
            reason = "collecting starts every worker before the first join"
        )]
        let workers: Vec<_> = runtime_packages
            .iter()
            .map(|package_id| {
                let snapshot = &snapshot;
                scope.spawn(move || {
                    client::all_versions_in(registry, snapshot, package_id)
                        .map_err(|error| error.to_string())
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| {
                worker
                    .join()
                    .unwrap_or_else(|_| Err("version index fetch panicked".to_string()))
            })
            .collect()
    });
    for verified in verified {
        let verified = verified?;
        if let Some(offer) = installable_versions(&verified.entry, registry.channel)
            .filter(|version| {
                version
                    .target()
                    .is_some_and(|target| target.runtime.as_deref() == Some(runtime))
            })
            .max_by(|a, b| tilo_app_package::version::compare(&a.version, &b.version))
            .and_then(|version| app_for_version(&verified, version, registry_name))
        {
            return Ok(Some(offer));
        }
    }
    Ok(None)
}

/// The catalogue row offering exactly `version` of a verified entry.
fn app_for_version(
    v: &VerifiedEntry,
    version: &registry_types::RegistryVersion,
    registry_name: &str,
) -> Option<CatalogueApp> {
    if !v.trust.is_offerable() {
        return None;
    }
    let tapp = version.tapp()?;
    let target = &tapp.target;
    let locale = preferred_locale();
    let display_name = if v.entry.name.resolve(&locale).is_none() {
        package_store::display_name_for_package_id(&v.entry.package_id)
    } else {
        v.entry.name.resolve(&locale)?.to_string()
    };
    Some(CatalogueApp {
        package_id: v.entry.package_id.clone(),
        display_name,
        description: v.entry.description.resolve(&locale)?.to_string(),
        tagline: v.entry.tagline.resolve(&locale)?.to_string(),
        images: v.entry.images.resolve(&locale).to_vec(),
        supported_languages: v.entry.supported_languages.clone(),
        category: v
            .entry
            .categories
            .iter()
            .find(|c| !c.trim().is_empty())
            .cloned(),
        version: version.version.clone(),
        registry: registry_name.to_string(),
        download_url: tapp.tapp_url.clone(),
        tapp_root_hash_hex: tapp.tapp_root_hash_hex.clone(),
        tapp_size_bytes: tapp.tapp_size_bytes,
        tapp_blake3_hex: tapp.tapp_blake3_hex.clone(),
        architecture: target.architecture.clone(),
        isa: target.isa.clone(),
        runtime: target.runtime.clone(),
        publisher_pubkey_hex: v.entry.publisher_pubkey_hex.clone(),
        trust: match v.trust {
            TrustLevel::PublisherSigned => "publisher-signed".into(),
            // Unreachable — filtered above. Kept total rather than panicking on a shape change.
            TrustLevel::Untrusted => "untrusted".into(),
        },
    })
}

/// Every version of `package_id` the registry named `registry_name` offers
/// this machine on its channel, newest first. Older versions come from the
/// entry's signed version index when the registry serves compact snapshots.
///
/// # Errors
///
/// Returns an error when no enabled registry has that name, or it cannot be
/// read, does not list the package, or fails a proof.
pub fn version_offers(package_id: &str, registry_name: &str) -> Result<Vec<CatalogueApp>, String> {
    let registries = config::load_registries();
    let registry = registries
        .enabled()
        .find(|registry| registry.name == registry_name)
        .ok_or_else(|| format!("no enabled repository is named '{registry_name}'"))?;
    let verified =
        client::fetch_all_versions(registry, package_id).map_err(|error| error.to_string())?;
    let mut versions: Vec<_> = installable_versions(&verified.entry, registry.channel).collect();
    versions.sort_by(|a, b| tilo_app_package::version::compare(&b.version, &a.version));
    Ok(versions
        .into_iter()
        .filter_map(|version| app_for_version(&verified, version, registry_name))
        .collect())
}

fn target_supported(tapp: &registry_types::TappArtifact) -> bool {
    let target = &tapp.target;
    if target.platform != registry_types::TILO_OS_PLATFORM
        || target.architecture != std::env::consts::ARCH
    {
        return false;
    }
    #[cfg(target_arch = "x86_64")]
    {
        match target.isa.as_str() {
            "x86_64" => true,
            "x86_64-v2" => is_x86_feature_detected!("sse4.2") && is_x86_feature_detected!("popcnt"),
            "x86_64-v3" => {
                is_x86_feature_detected!("avx2")
                    && is_x86_feature_detected!("bmi2")
                    && is_x86_feature_detected!("fma")
            }
            "x86_64-v4" => {
                is_x86_feature_detected!("avx512f") && is_x86_feature_detected!("avx512bw")
            }
            _ => false,
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        #[cfg(target_arch = "aarch64")]
        {
            match target.isa.as_str() {
                "aarch64" => true,
                "aarch64-v8.2" => std::arch::is_aarch64_feature_detected!("dotprod"),
                "aarch64-v8.4" => std::arch::is_aarch64_feature_detected!("i8mm"),
                "aarch64-v9" => std::arch::is_aarch64_feature_detected!("sve2"),
                _ => false,
            }
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            false
        }
    }
}

fn preferred_locale() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok())
        .and_then(|value| {
            let base = value.split('.').next()?.replace('_', "-");
            registry_types::valid_language_tag(&base).then_some(base)
        })
        .unwrap_or_else(|| "en".into())
}

/// The package ids `tilo-install` has already installed here, read
/// through the launcher's registry reader (same file, same
/// `TILO_APPS_DATA_DIR` override) so the two can't disagree about what's present.
pub fn installed_ids() -> Vec<String> {
    package_store::list_installed()
        .map(|v| v.into_iter().map(|e| e.id).collect())
        .unwrap_or_default()
}

/// One installed app as `app list` shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledApp {
    pub package_id: String,
    pub version: String,
    /// Where the running copy came from: `image` (the system image installed
    /// it), `user` (installed or updated since), or `held` (rolled back
    /// below the image's version on purpose).
    pub from: String,
}

/// Every installed app, the system image's included, as the copy that runs.
///
/// # Errors
///
/// Returns an error if the installed-app registry cannot be read.
pub fn installed_apps() -> Result<Vec<InstalledApp>, String> {
    let entries = package_store::list_installed()
        .map_err(|error| format!("could not read the installed app registry: {error}"))?;
    Ok(entries
        .into_iter()
        .map(|entry| InstalledApp {
            version: installed_manifest_version(&entry.manifest_path)
                .unwrap_or_else(|| entry.version.clone()),
            from: if entry.system {
                "image"
            } else if entry.held {
                "held"
            } else {
                "user"
            }
            .into(),
            package_id: entry.id,
        })
        .collect())
}

/// The version of `package_id` the system image ships, when the user's copy
/// shadows it — the version removing that copy goes back to.
///
/// # Errors
///
/// Returns an error if either store's registry cannot be read.
pub fn image_version_beneath(package_id: &str) -> Result<Option<String>, String> {
    let layers = package_store::Catalog::from_env()
        .and_then(|catalog| catalog.layers(package_id))
        .map_err(|error| format!("could not read the installed app registry: {error}"))?;
    Ok(match (layers.user, layers.image) {
        (Some(_), Some(image)) => Some(image.version),
        _ => None,
    })
}

/// Version recorded by an installed app's manifest, when readable.
///
/// # Errors
///
/// Returns an error if the installed-app registry or the package's manifest
/// cannot be read.
pub fn installed_version(package_id: &str) -> Result<Option<String>, String> {
    let Some(entry) = package_store::find_installed(package_id)
        .map_err(|error| format!("could not read the installed app registry: {error}"))?
    else {
        return Ok(None);
    };
    installed_manifest_version(&entry.manifest_path)
        .map(Some)
        .ok_or_else(|| format!("could not read the installed manifest for {package_id}"))
}

/// Whether the installed `package_id` is recorded at the package format this
/// build reads. One installed before the format changed is behind whatever
/// its version, and the next update replaces it
/// ([`package_store::integrity::record_current`]). A package that is not
/// installed, or a registry that cannot be read, is not judged here.
#[must_use]
pub fn installed_record_current(package_id: &str) -> bool {
    package_store::find_installed(package_id)
        .ok()
        .flatten()
        .is_none_or(|entry| package_store::integrity::record_current(&entry))
}

/// Installed package versions, parsed once for bulk update checks.
///
/// # Errors
///
/// Returns an error if the installed-app registry or any installed package
/// manifest cannot be read.
pub fn installed_versions() -> Result<std::collections::HashMap<String, String>, String> {
    let entries = package_store::list_installed()
        .map_err(|error| format!("could not read the installed app registry: {error}"))?;
    entries
        .into_iter()
        .map(|entry| {
            installed_manifest_version(&entry.manifest_path)
                .map(|version| (entry.id.clone(), version))
                .ok_or_else(|| format!("could not read the installed manifest for {}", entry.id))
        })
        .collect()
}

fn installed_manifest_version(path: &std::path::Path) -> Option<String> {
    // Runtime packages may have no executable entrypoint. Parse their normal
    // package manifest rather than requiring the launcher's app-only view.
    if let Ok(text) = std::fs::read_to_string(path)
        && let Ok(manifest) = tilo_app_package::Manifest::from_toml_str(&text)
        && !manifest.package.version.is_empty()
    {
        return Some(manifest.package.version);
    }
    tilo_app_package::view::read_manifest(path)
        .ok()
        .map(|manifest| manifest.version)
        .filter(|version| !version.is_empty())
}

/// Subtract what is installed, and collapse duplicate ids. Registries
/// are consulted in file order, so the first that offers an id wins —
/// the user's ordering is the tie-break.
pub fn dedupe_and_subtract(apps: Vec<CatalogueApp>, installed: &[String]) -> Vec<CatalogueApp> {
    let installed = installed
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for app in apps {
        if installed.contains(app.package_id.as_str()) {
            continue;
        }
        if !seen.insert(app.package_id.clone()) {
            continue;
        }
        out.push(app);
    }
    out.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    out
}

/// Everything the configured registries offer that is not installed.
/// Blocking, but enabled registries are fetched concurrently so one slow
/// mirror does not serially delay every healthy source. Results are still
/// folded in configuration order, preserving the user's registry priority.
pub fn all_offers() -> CatalogueListing {
    // Invoking the public app/store client is an explicit request to use the
    // store, so materialize the signed official source on first use.
    let registries = config::load_registries();
    let enabled: Vec<RegistryConfig> = registries.enabled().cloned().collect();
    #[allow(
        clippy::needless_collect,
        reason = "all registry workers must be spawned before the first join; joining the mapped iterator directly would serialize the HTTP requests"
    )]
    let fetched = std::thread::scope(|scope| {
        let handles: Vec<_> = enabled
            .iter()
            .map(|registry| scope.spawn(move || client::fetch_entries(registry)))
            .collect();
        handles
            .into_iter()
            .map(std::thread::ScopedJoinHandle::join)
            .collect::<Vec<_>>()
    });
    let mut apps = Vec::new();
    let mut unreachable = Vec::new();
    for (reg, result) in enabled.iter().zip(fetched) {
        match result {
            Err(_) => unreachable.push(UnreachableRegistry {
                name: reg.name.clone(),
                detail: "registry worker stopped unexpectedly".to_string(),
            }),
            Ok(result) => match result {
                Ok(entries) => {
                    for v in &entries {
                        if let Some(app) = entry_to_app_for_channel(v, &reg.name, reg.channel) {
                            apps.push(app);
                        }
                    }
                }
                Err(e) => unreachable.push(UnreachableRegistry {
                    name: reg.name.clone(),
                    detail: e.to_string(),
                }),
            },
        }
    }
    CatalogueListing {
        apps: dedupe_and_subtract(apps, &[]),
        unreachable,
    }
}

/// Everything the configured registries offer that is not installed.
pub fn available() -> CatalogueListing {
    let mut listing = all_offers();
    listing.apps = dedupe_and_subtract(listing.apps, &installed_ids());
    listing
}

/// Resolve a human-friendly name without guessing across ambiguous matches.
///
/// # Errors
///
/// Returns an error when no offered app matches `query`, or when the
/// human-friendly name matches more than one offer.
pub fn resolve_offer<'a>(
    apps: &'a [CatalogueApp],
    query: &str,
) -> Result<&'a CatalogueApp, String> {
    let wanted = query.trim().to_ascii_lowercase();
    let exact = apps
        .iter()
        .find(|app| app.package_id.eq_ignore_ascii_case(&wanted));
    if let Some(app) = exact {
        return Ok(app);
    }
    let normalized = |value: &str| {
        value
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let wanted_normalized = normalized(&wanted);
    let matches = apps
        .iter()
        .filter(|app| {
            app.package_id
                .rsplit('.')
                .next()
                .is_some_and(|slug| normalized(slug) == wanted_normalized)
                || normalized(&app.display_name) == wanted_normalized
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [app] => Ok(*app),
        [] => Err(format!("no app named '{query}' was found")),
        _ => Err(format!("'{query}' is ambiguous; use the full package id")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use registry_types::{RegistryEntry, RegistryVersion, sign_entry};

    fn native_baseline_isa() -> &'static str {
        #[cfg(target_arch = "x86_64")]
        {
            "x86_64"
        }
        #[cfg(target_arch = "aarch64")]
        {
            "aarch64"
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            "unsupported"
        }
    }

    fn entry(id: &str, name: &str, categories: Vec<&str>) -> RegistryEntry {
        let key = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let mut e = RegistryEntry {
            package_id: id.into(),
            kind: registry_types::EntryKind::App,
            name: name.into(),
            description: "does a thing".into(),
            tagline: "Native Tilo OS app".into(),
            images: registry_types::LocalizedImages::english_icon(
                "https://apps.tilo.so/images/test-icon.svg",
                "Test icon",
            ),
            supported_languages: vec!["en".into()],
            categories: categories.into_iter().map(str::to_string).collect(),
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: hex::encode(key.verifying_key().to_bytes()),
            versions: vec![RegistryVersion {
                version: "2.0.0".into(),
                released_at_unix: 5,
                artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                    tapp_url: format!("https://apps.tilo.so/artifacts/{id}.tapp"),
                    tapp_root_hash_hex: "ab".repeat(32),
                    tapp_size_bytes: 10,
                    tapp_blake3_hex: "ef".repeat(32),
                    target: registry_types::RegistryTarget {
                        platform: registry_types::TILO_OS_PLATFORM.into(),
                        architecture: std::env::consts::ARCH.into(),
                        isa: native_baseline_isa().into(),
                        runtime: None,
                        interfaces: Vec::new(),
                    },
                }),
            }],
            updated_at_unix: 7,
            signature_hex: String::new(),
        };
        sign_entry(&mut e, &key);
        e
    }

    fn verified(e: RegistryEntry) -> VerifiedEntry {
        let owner = ed25519_dalek::SigningKey::from_bytes(&[10; 32]);
        let mut snapshot = registry_types::SignedRegistrySnapshot {
            protocol: registry_types::PACKAGE_FORMAT_VERSION,
            sequence: 1,
            issued_at_unix: 10,
            expires_at_unix: 20,
            previous_snapshot_hash_hex: String::new(),
            moderation_head_hex: String::new(),
            publishers: vec![registry_types::PublisherAuthorization {
                pubkey_hex: e.publisher_pubkey_hex.clone(),
                can_add_new: true,
            }],
            entries: vec![e.clone()],
            owner_signature_hex: String::new(),
            channel_authorization: None,
        };
        registry_types::sign_snapshot(&mut snapshot, &owner).unwrap();
        client::classify_snapshot(
            &registry_types::CatalogueSnapshot::Full(snapshot),
            &hex::encode(owner.verifying_key().to_bytes()),
            15,
        )
        .map_or(
            VerifiedEntry {
                entry: e,
                trust: TrustLevel::Untrusted,
                version_index: None,
            },
            |mut entries| entries.remove(0),
        )
    }

    #[test]
    fn an_os_is_offered_only_the_builds_it_can_serve() {
        let mut e = entry("so.tilo.settings", "Settings", vec!["System"]);
        let mut newer = e.versions[0].clone();
        newer.version = "3.0.0".into();
        newer.tapp_mut().unwrap().target.interfaces = vec!["system@2".into()];
        e.versions[0].tapp_mut().unwrap().target.interfaces = vec!["system@1".into()];
        e.versions.push(newer);
        let stable = tilo_app_package::version::ReleaseChannel::Stable;
        let offered = |provided: Option<&registry_types::ProvidedInterfaces>| {
            installable_versions_on(&e, stable, provided)
                .map(|version| version.version.clone())
                .collect::<Vec<_>>()
        };
        let old_os = registry_types::ProvidedInterfaces::new([("system", 1)], []);
        let new_os = registry_types::ProvidedInterfaces::new([("system", 2)], []);
        let elsewhere = registry_types::ProvidedInterfaces::new([("runtime", 1)], ["collab"]);
        assert_eq!(offered(Some(&old_os)), ["2.0.0"]);
        assert_eq!(
            offered(Some(&new_os)),
            ["3.0.0"],
            "a build for an older version is not offered either"
        );
        assert!(offered(Some(&elsewhere)).is_empty());
        // A host that declares no interfaces judges none.
        assert_eq!(offered(None), ["2.0.0", "3.0.0"]);
    }

    // USER REQUIREMENT (2026-10-18): "it's fine if we break compat with old versions every few days, people just need to update"
    #[test]
    fn an_app_whose_newest_build_is_behind_waits_for_its_developer() {
        let mut e = entry("so.tilo.settings", "Settings", vec!["System"]);
        e.versions[0].tapp_mut().unwrap().target.interfaces = vec!["system@1".into()];
        let stable = tilo_app_package::version::ReleaseChannel::Stable;
        let old_os = registry_types::ProvidedInterfaces::new([("system", 1)], []);
        let new_os = registry_types::ProvidedInterfaces::new([("system", 2)], []);
        let newer_os = registry_types::ProvidedInterfaces::new([("system", 0)], []);
        assert!(
            !awaiting_developer_on(&e, stable, Some(&old_os)),
            "a build that fits is offered"
        );
        assert!(
            awaiting_developer_on(&e, stable, Some(&new_os)),
            "only the developer's next build can fit a newer system"
        );
        assert!(
            !awaiting_developer_on(&e, stable, Some(&newer_os)),
            "a build ahead of this system waits for the system, not the developer"
        );
        assert!(
            !awaiting_developer_on(&e, stable, None),
            "a host that declares no interfaces judges none"
        );
    }

    #[test]
    fn a_bot_entry_is_never_an_app_offer() {
        let hash = "ab".repeat(32);
        let mut bot = entry("so.tilo.bots.echo", "Echo", vec!["Bots"]);
        bot.kind = registry_types::EntryKind::Bot;
        bot.versions = vec![registry_types::RegistryVersion {
            version: "1.0.0".into(),
            released_at_unix: 1,
            artifact: registry_types::VersionArtifact::Bot(registry_types::BotArtifact {
                program_url: registry_types::bot_program_url("https://apps.tilo.so", &hash),
                program_blake3_hex: hash,
                program_size_bytes: 10,
                requires: registry_types::BotRequirements::default(),
            }),
        }];
        assert_eq!(
            installable_versions(&bot, tilo_app_package::version::ReleaseChannel::Stable).count(),
            0
        );
        assert!(entry_to_app(&verified(bot), "main").is_none());
    }

    #[test]
    fn only_valid_signed_downloadable_entries_become_rows() {
        // Phase `a_signed_entry_becomes_a_row_carrying_the_publishers_own_metadata`.
        {
            let v = verified(entry("so.tilo.sheets", "Sheets", vec!["Office"]));
            let app = entry_to_app(&v, "main").expect("an offerable entry");
            assert_eq!(app.package_id, "so.tilo.sheets");
            assert_eq!(app.display_name, "Sheets");
            assert_eq!(app.category.as_deref(), Some("Office"));
            assert_eq!(app.version, "2.0.0");
            assert_eq!(app.registry, "main");
            assert_eq!(
                app.download_url,
                "https://apps.tilo.so/artifacts/so.tilo.sheets.tapp"
            );
            assert_eq!(app.trust, "publisher-signed");
        }
        // Phase `a_tampered_entry_never_becomes_a_row`.
        // An entry whose signature does not cover its own metadata must not
        // become something a user can activate — the name and download URL would be unattributed.
        {
            let mut e = entry("so.tilo.sheets", "Sheets", vec![]);
            e.name = "Totally Legit Banking".into();
            let v = verified(e);
            assert_eq!(v.trust, TrustLevel::Untrusted);
            assert!(entry_to_app(&v, "main").is_none());
        }
        // Phase `an_entry_with_no_downloadable_version_is_not_offered`.
        {
            let mut e = entry("so.tilo.ghost", "Ghost", vec![]);
            e.versions.clear();
            sign_entry(&mut e, &ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]));
            assert!(entry_to_app(&verified(e), "main").is_none());

            let mut blank = entry("so.tilo.blank", "Blank", vec![]);
            blank.versions[0].tapp_mut().unwrap().tapp_url = "  ".into();
            sign_entry(
                &mut blank,
                &ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]),
            );
            assert!(entry_to_app(&verified(blank), "main").is_none());
        }
        // Phase `a_nameless_entry_is_invalid_signed_metadata`.
        {
            let e = entry("so.tilo.hex-editor", "", vec![]);
            assert!(e.validate().is_err());
        }
    }

    #[test]
    fn tilo_os_never_offers_an_android_build() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let mut e = entry("so.tilo.editor", "Editor", vec![]);
        let mut android = e.versions[0].clone();
        android.version = "9.0.0".into();
        android.tapp_mut().unwrap().tapp_url =
            "https://apps.tilo.so/artifacts/editor-android.tapp".into();
        android.tapp_mut().unwrap().target = registry_types::RegistryTarget {
            platform: registry_types::ANDROID_PLATFORM.into(),
            architecture: std::env::consts::ARCH.into(),
            isa: std::env::consts::ARCH.into(),
            runtime: Some("tilo-runtime/2026.07".into()),
            interfaces: Vec::new(),
        };
        assert!(!target_supported(android.tapp().unwrap()));
        e.versions.push(android.clone());
        sign_entry(&mut e, &key);
        let app = entry_to_app(&verified(e), "main").expect("the tilo-os build is offered");
        assert_eq!(app.version, "2.0.0");
        assert_eq!(
            app.download_url,
            "https://apps.tilo.so/artifacts/so.tilo.editor.tapp"
        );

        let mut android_only = entry("so.tilo.editor", "Editor", vec![]);
        android_only.versions = vec![android];
        sign_entry(&mut android_only, &key);
        assert!(entry_to_app(&verified(android_only), "main").is_none());
    }

    #[test]
    fn the_catalogue_selects_per_channel_first_registry_and_uninstalled_offers() {
        // Phase `catalogue_selects_latest_release_independent_of_registry_array_order`.
        {
            let mut e = entry("so.tilo.timeline", "Timeline", vec![]);
            let older = RegistryVersion {
                version: "1.0.0".into(),
                released_at_unix: 100,
                artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                    tapp_url: "https://apps.tilo.so/artifacts/timeline-1.tapp".into(),
                    tapp_root_hash_hex: "11".repeat(32),
                    tapp_size_bytes: 10,
                    tapp_blake3_hex: "ef".repeat(32),
                    target: registry_types::RegistryTarget {
                        platform: registry_types::TILO_OS_PLATFORM.into(),
                        architecture: std::env::consts::ARCH.into(),
                        isa: native_baseline_isa().into(),
                        runtime: None,
                        interfaces: Vec::new(),
                    },
                }),
            };
            let mut newer = older.clone();
            newer.version = "2027.01".into();
            newer.released_at_unix = 200;
            let tapp = newer.tapp_mut().unwrap();
            tapp.tapp_url = "https://apps.tilo.so/artifacts/timeline-new.tapp".into();
            tapp.tapp_root_hash_hex = "22".repeat(32);
            tapp.tapp_size_bytes = 20;
            tapp.tapp_blake3_hex = "fe".repeat(32);
            e.versions = vec![older, newer];
            sign_entry(&mut e, &ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]));

            let app = entry_to_app(&verified(e), "main").expect("offerable latest release");
            assert_eq!(app.version, "2027.01");
            assert_eq!(
                app.download_url,
                "https://apps.tilo.so/artifacts/timeline-new.tapp"
            );
            assert_eq!(app.tapp_size_bytes, 20);
        }
        // Phase `catalogue_channels_separate_stable_and_continuous_updates`.
        {
            use tilo_app_package::version::ReleaseChannel;
            let mut e = entry("so.tilo.channel-test", "Channel", vec![]);
            let template = e.versions[0].clone();
            e.versions = ["0.1.0", "0.1.1-dev.9+gffff", "0.1.1-dev.10+gaaaa"]
                .into_iter()
                .map(|version| RegistryVersion {
                    version: version.into(),
                    ..template.clone()
                })
                .collect();
            sign_entry(&mut e, &ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]));
            let current = verified(e.clone());
            assert_eq!(entry_to_app(&current, "main").unwrap().version, "0.1.0");
            assert_eq!(
                entry_to_app_for_channel(&current, "main", ReleaseChannel::Development)
                    .unwrap()
                    .version,
                "0.1.1-dev.10+gaaaa"
            );
            e.versions.push(RegistryVersion {
                version: "0.1.1".into(),
                ..template
            });
            sign_entry(&mut e, &ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]));
            let promoted = verified(e);
            for channel in [ReleaseChannel::Stable, ReleaseChannel::Development] {
                assert_eq!(
                    entry_to_app_for_channel(&promoted, "main", channel)
                        .unwrap()
                        .version,
                    "0.1.1"
                );
            }
        }
        // Phase `installed_packages_are_subtracted`.
        {
            let a = entry_to_app(&verified(entry("so.tilo.a", "A", vec![])), "main").unwrap();
            let b = entry_to_app(&verified(entry("so.tilo.b", "B", vec![])), "main").unwrap();
            let out = dedupe_and_subtract(vec![a, b], &["so.tilo.a".to_string()]);
            assert_eq!(out.len(), 1, "the installed one is gone: {out:?}");
            assert_eq!(out[0].package_id, "so.tilo.b");
        }
        // Phase `the_first_registry_offering_an_id_wins`.
        {
            let first =
                entry_to_app(&verified(entry("so.tilo.a", "A", vec![])), "primary").unwrap();
            let second =
                entry_to_app(&verified(entry("so.tilo.a", "A", vec![])), "mirror").unwrap();
            let out = dedupe_and_subtract(vec![first, second], &[]);
            assert_eq!(out.len(), 1);
            assert_eq!(
                out[0].registry, "primary",
                "the user's registry ordering breaks the tie"
            );
        }
    }

    #[test]
    fn json_contract_installed_versions_and_names_resolve_exactly() {
        // Phase `the_json_envelope_field_names_are_the_contract`.
        // The wire contract with the compositor, pinned by exact field name.
        // A rename here without a matching change there would silently
        // produce an empty catalogue rather than an error — both tests must be edited together.
        {
            let listing = CatalogueListing {
                apps: vec![CatalogueApp {
                    package_id: "so.tilo.sheets".into(),
                    display_name: "Sheets".into(),
                    description: "a spreadsheet".into(),
                    tagline: "Native Tilo OS app".into(),
                    images: Vec::default(),
                    supported_languages: vec!["en".into()],
                    category: Some("Office".into()),
                    version: "2.0.0".into(),
                    registry: "main".into(),
                    download_url: "https://x/y.tapp".into(),
                    tapp_root_hash_hex: "ab".repeat(32),
                    tapp_size_bytes: 10,
                    tapp_blake3_hex: "ef".repeat(32),
                    architecture: "x86_64".into(),
                    isa: "x86_64-v3".into(),
                    runtime: None,
                    publisher_pubkey_hex: "cd".repeat(32),
                    trust: "verified".into(),
                }],
                unreachable: vec![UnreachableRegistry {
                    name: "mirror".into(),
                    detail: "connection refused".into(),
                }],
            };
            let json = serde_json::to_string(&listing).expect("encodes");
            assert_eq!(
                json,
                r#"{"apps":[{"package_id":"so.tilo.sheets","display_name":"Sheets","description":"a spreadsheet","tagline":"Native Tilo OS app","images":[],"supported_languages":["en"],"category":"Office","version":"2.0.0","registry":"main","download_url":"https://x/y.tapp","tapp_root_hash_hex":"abababababababababababababababababababababababababababababababab","tapp_size_bytes":10,"tapp_blake3_hex":"efefefefefefefefefefefefefefefefefefefefefefefefefefefefefefefef","architecture":"x86_64","isa":"x86_64-v3","runtime":null,"publisher_pubkey_hex":"cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd","trust":"verified"}],"unreachable":[{"name":"mirror","detail":"connection refused"}]}"#
            );
        }
        // Phase `installed_version_reads_the_canonical_package_table`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("manifest.toml");
            std::fs::write(
                &path,
                r#"
    [package]
    id = "so.tilo.browser"
    version = "1.2.3"
    kind = "app"

    [publisher]
    identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"

    [sandbox]
    namespaces = []
    seccomp = "tilo-default"
    landlock = "strict"

    [sandbox.net]
    mode = "off"

    [entrypoint]
    bin = "bin/browser"
    "#,
            )
            .unwrap();
            assert_eq!(installed_manifest_version(&path).as_deref(), Some("1.2.3"));
        }
        // Phase `friendly_names_resolve_without_fuzzy_guessing`.
        {
            let browser = entry_to_app(
                &verified(entry("so.tilo.browser", "Tilo Browser", vec![])),
                "main",
            )
            .unwrap();
            let apps = vec![browser];
            assert_eq!(
                resolve_offer(&apps, "browser").unwrap().package_id,
                "so.tilo.browser"
            );
            assert_eq!(
                resolve_offer(&apps, "Tilo Browser").unwrap().package_id,
                "so.tilo.browser"
            );
            assert!(
                resolve_offer(&apps, "browse").is_err(),
                "partial names must not guess"
            );
        }
    }

    fn offer_on(version: &str, runtime: Option<&str>) -> CatalogueApp {
        let mut app = entry_to_app(&verified(entry("so.tilo.notes", "Notes", vec![])), "main")
            .expect("an offerable entry");
        app.version = version.into();
        app.runtime = runtime.map(str::to_string);
        app
    }

    #[test]
    fn first_open_prefers_a_version_on_an_installed_runtime() {
        let r1 = "tilo-runtime/2026.07+native.11111111111111111111111111111111";
        let r2 = "tilo-runtime/2026.07+native.22222222222222222222222222222222";
        let offers = vec![
            offer_on("0.1.3", Some(r2)),
            offer_on("0.1.2", Some(r1)),
            offer_on("0.1.1", Some(r1)),
        ];
        let installed = |runtime: &str| runtime == r1;
        let chosen = prefer_installed_runtime(&offers, "0.1.3", installed).expect("a local fit");
        assert_eq!(chosen.version, "0.1.2");
        assert_eq!(chosen.runtime.as_deref(), Some(r1));
        // Never newer than what was asked for.
        let chosen = prefer_installed_runtime(&offers, "0.1.1", installed).expect("a local fit");
        assert_eq!(chosen.version, "0.1.1");
        // A version that needs no runtime runs anywhere.
        let fat = vec![offer_on("0.1.3", Some(r2)), offer_on("0.1.0", None)];
        let chosen = prefer_installed_runtime(&fat, "0.1.3", |_| false).expect("needs no runtime");
        assert_eq!(chosen.version, "0.1.0");
    }

    #[test]
    fn first_open_falls_back_to_newest_without_a_local_runtime() {
        let r2 = "tilo-runtime/2026.07+native.22222222222222222222222222222222";
        let r1 = "tilo-runtime/2026.07+native.11111111111111111111111111111111";
        let offers = vec![offer_on("0.1.3", Some(r2)), offer_on("0.1.2", Some(r1))];
        assert!(prefer_installed_runtime(&offers, "0.1.3", |_| false).is_none());
        // A version that needs no runtime is taken as is, without a lookup.
        let fat = offer_on("0.1.3", None);
        assert_eq!(first_open_offer(&fat), fat);
    }
}
