//! Reclaiming what no installed package needs: app payloads past the
//! store's size policy, payload trees nothing names, and per-user runtime
//! bundles no installed package runs on.
//!
//! It runs when something changed — after every `tilo-install install`,
//! `update` and `uninstall`, and on `tilo-install gc` — never on a timer.
//! What a running app has mounted stays ([`package_store::mounts`]), and so
//! does what the user keeps: pinned packages, and pinned runtimes. The size
//! cap never evicts an app a launch is opening, nor the apps the change it
//! follows just wrote ([`Request::keep`]): the person installed it to open
//! it, and until it opens nothing else marks it as wanted.
//!
//! A runtime is needed while any installed package names it: as its
//! `runtime`, or among its manifest's `optional-runtimes`. Both the user's
//! store and the image's count, and so does every payload tree an update
//! left behind because a launch held it or a running app had it mounted:
//! that launch runs on the old build's runtime. A manifest that cannot be
//! read makes the whole runtime collection stop: its runtimes cannot be
//! told apart.

use std::collections::BTreeSet;
use std::io;
use std::path::Path;

use package_store::mounts::InUse;
use package_store::{Catalog, InstalledPackage, Orphans, PackageStore};
use runtime_bundle::gc::{BundleRoot, Collected, Holds, Options};

/// Where the launcher mounts a runtime inside an app's sandbox
/// (`/run/tilo-runtime/<version>`); a process with that mount is running on
/// that runtime build, whichever copy of it was bound.
pub const SANDBOX_RUNTIME_ROOT: &str = "/run/tilo-runtime";

/// One collection's settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The store's size cap; `0` evicts nothing for size.
    pub max_store_bytes: u64,
    /// How long a freshly installed runtime stays unreferenced.
    pub grace_secs: u64,
    /// Unix seconds now.
    pub now: u64,
    /// Report the runtimes that would go; touch nothing.
    pub dry_run: bool,
    /// Apps the size cap never evicts: what the change just installed.
    pub keep: Vec<String>,
}

impl Request {
    /// What runs after an install, update or uninstall: the store's cap
    /// `max_store_bytes`, the default grace for fresh runtimes, now.
    #[must_use]
    pub fn after_change(max_store_bytes: u64) -> Self {
        Self {
            max_store_bytes,
            grace_secs: runtime_bundle::gc::DEFAULT_GRACE_SECS,
            now: tilo_clock::unix_now(),
            dry_run: false,
            keep: Vec::new(),
        }
    }

    /// This request, never evicting the apps `keep` names for size.
    #[must_use]
    pub fn keeping(mut self, keep: impl IntoIterator<Item = String>) -> Self {
        self.keep.extend(keep);
        self
    }
}

/// The apps in `store` a change installed or updated at or after `since`
/// (Unix seconds): what a collection after that change keeps.
///
/// # Errors
///
/// When the registry cannot be read.
pub fn written_since(store: &PackageStore, since: u64) -> Result<Vec<String>, String> {
    let registry = store
        .registry()
        .map_err(|error| format!("registry: {error}"))?;
    Ok(registry
        .entries()
        .iter()
        .filter(|row| row.installed_at >= since)
        .map(|row| row.id.clone())
        .collect())
}

/// What one collection did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Apps evicted under the size cap.
    pub evicted: Vec<String>,
    /// Payload trees nothing named.
    pub orphans: Orphans,
    /// The runtime bundles.
    pub runtimes: Collected,
}

impl Report {
    /// Bytes freed, apps evicted for size not counted (their blobs are the
    /// content store's to count).
    #[must_use]
    pub fn freed_bytes(&self) -> u64 {
        self.runtimes
            .freed_bytes()
            .saturating_add(self.orphans.removed_bytes)
            .saturating_add(self.orphans.staging_bytes)
    }
}

/// Apply the size cap, remove orphaned payload trees, then collect the
/// runtime bundles under `runtimes` that nothing in `catalog` names.
///
/// A dry run only reports the runtimes; payloads are not simulated.
///
/// # Errors
///
/// Returns the first step that failed, as a message; the steps before it
/// took effect.
pub fn collect(
    store: &PackageStore,
    catalog: &Catalog,
    runtimes: &BundleRoot,
    request: Request,
) -> Result<Report, String> {
    let Request {
        max_store_bytes,
        grace_secs,
        now,
        dry_run,
        keep,
    } = request;
    let mut report = Report::default();
    // A device with no app store yet has no payloads; collecting must not
    // create one.
    if !dry_run && store.layout().registry().is_file() {
        let keep: Vec<&str> = keep.iter().map(String::as_str).collect();
        report.evicted = store
            .sweep_keeping(max_store_bytes, &keep)
            .map_err(|error| format!("size sweep: {error}"))?;
        report.orphans = store
            .collect_orphans(&InUse::now())
            .map_err(|error| format!("orphaned payloads: {error}"))?;
    }
    let references = || -> io::Result<BTreeSet<String>> {
        let rows = catalog
            .list()
            .map_err(|error| io::Error::other(error.to_string()))?;
        let user = store
            .registry()
            .map_err(|error| io::Error::other(error.to_string()))?;
        let mut named = referenced_runtimes(rows.iter().chain(user.entries()))?;
        named.extend(retained_runtimes(&store.layout().packages())?);
        Ok(named)
    };
    report.runtimes = runtimes
        .collect(
            &references,
            &in_use_now,
            Options {
                grace_secs,
                now,
                dry_run,
            },
        )
        .map_err(|error| format!("runtime bundles: {error}"))?;
    Ok(report)
}

/// The runtime versions `rows` run on or may mount.
pub fn referenced_runtimes<'a>(
    rows: impl IntoIterator<Item = &'a InstalledPackage>,
) -> io::Result<BTreeSet<String>> {
    let mut versions = BTreeSet::new();
    for row in rows {
        versions.extend(version_of(&row.runtime));
        let text = match std::fs::read_to_string(&row.manifest_path) {
            Ok(text) => text,
            // Without its manifest a row cannot launch, so it mounts nothing
            // beyond the runtime its row records.
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("{}: {}: {error}", row.id, row.manifest_path.display()),
                ));
            }
        };
        versions.extend(manifest_runtimes(&text, &row.id)?);
    }
    Ok(versions)
}

/// The runtime versions the package records still under `packages`
/// (`<store>/pkgs`) name, rows or not. Run after the orphan collection, the
/// records without a row are the trees a launch or a running app holds.
pub fn retained_runtimes(packages: &Path) -> io::Result<BTreeSet<String>> {
    let mut versions = BTreeSet::new();
    let entries = match std::fs::read_dir(packages) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(versions),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        // Only record directories directly under `pkgs/`; a link is never
        // followed.
        let record = Path::new(&name).extension().is_some_and(|ext| ext == "pkg");
        if !entry.file_type()?.is_dir() || !record {
            continue;
        }
        let path = entry.path().join(package_store::layout::MANIFEST_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("{}: {error}", path.display()),
                ));
            }
        };
        versions.extend(manifest_runtimes(&text, &path.display().to_string())?);
    }
    Ok(versions)
}

/// The runtime and optional runtimes the manifest `text` of `who` names.
fn manifest_runtimes(text: &str, who: &str) -> io::Result<Vec<String>> {
    let manifest = tilo_app_package::Manifest::from_toml_str(text)
        .map_err(|error| io::Error::other(format!("{who}: manifest: {error}")))?;
    let mut versions: Vec<String> = manifest
        .package
        .runtime
        .as_deref()
        .and_then(version_of)
        .into_iter()
        .collect();
    versions.extend(
        manifest
            .package
            .optional_runtimes
            .iter()
            .filter_map(|raw| version_of(raw)),
    );
    Ok(versions)
}

/// What running apps hold now: a bundle directory a sandbox has mounted,
/// or a runtime build a sandbox runs on.
#[must_use]
pub fn in_use_now() -> Holds {
    let in_use = InUse::now();
    Box::new(move |path: &Path, version: &str| {
        in_use.holds(path) || in_use.has_mount_point(&Path::new(SANDBOX_RUNTIME_ROOT).join(version))
    })
}

/// `<name>/<version>` → `<version>`.
fn version_of(raw: &str) -> Option<String> {
    raw.split_once('/')
        .map(|(_, version)| version.to_string())
        .filter(|version| !version.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The laptop's case: the image (or a swapped userspace) ships the
    /// Browser, which pins one engine build, while the per-user runtime
    /// directory still holds the engines of every earlier Browser. Nothing
    /// installed through `tilo-install` changed, so only the system store's
    /// rows say which build is still wanted.
    #[test]
    fn engine_builds_no_installed_app_pins_are_collected() {
        const ENGINE: &str = "2026.10.9-chromium";
        const NATIVE: &str = "2026.07+native.00000000000000000000000000000000";
        let tmp = tempfile::tempdir().unwrap();
        let system = tmp.path().join("system");
        let payload = system.join("payloads/so.tilo.browser");
        std::fs::create_dir_all(&payload).unwrap();
        let manifest = payload.join("manifest.toml");
        std::fs::write(
            &manifest,
            format!(
                "[package]\nid = \"so.tilo.browser\"\nversion = \"1.0.0\"\nkind = \"app\"\n\
                 runtime = \"tilo-runtime/{NATIVE}\"\n\
                 optional-runtimes = [\"tilo-runtime/{ENGINE}\"]\n\
                 [publisher]\nidentity = \"ed25519:{key}\"\nsignature = \"\"\n\
                 [sandbox]\nnamespaces = [\"user\"]\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
                 [sandbox.net]\nmode = \"off\"\n[entrypoint]\nbin = \"bin/app\"\n",
                key = "00".repeat(32)
            ),
        )
        .unwrap();
        std::fs::write(
            system.join("registry.toml"),
            format!(
                "[[entry]]\nid = 'so.tilo.browser'\nruntime = 'tilo-runtime/{NATIVE}'\n\
                 payload_root = {:?}\nmanifest_path = {:?}\n",
                payload.display().to_string(),
                manifest.display().to_string()
            ),
        )
        .unwrap();
        let user = tmp.path().join("user");
        let catalog = Catalog::at(
            &user,
            Some(package_store::SystemStore {
                data_dir: system.clone(),
                payload_dir: system.join("payloads"),
                interfaces: None,
            }),
        );
        let store = PackageStore::at(&user);
        let bundles = tmp.path().join("runtimes");
        let versions = [
            "2026.10.1-chromium",
            "2026.10.2-chromium",
            "2026.10.4-chromium",
            ENGINE,
            NATIVE,
        ];
        for version in versions {
            std::fs::create_dir_all(bundles.join(version)).unwrap();
            std::fs::write(bundles.join(version).join("bundle.toml"), "").unwrap();
        }
        let report = collect(
            &store,
            &catalog,
            &BundleRoot::new(&bundles),
            Request {
                max_store_bytes: 0,
                grace_secs: 0,
                now: tilo_clock::unix_now(),
                dry_run: false,
                keep: Vec::new(),
            },
        )
        .unwrap();
        let removed: Vec<&str> = report
            .runtimes
            .removed
            .iter()
            .map(|(version, _)| version.as_str())
            .collect();
        assert_eq!(
            removed,
            [
                "2026.10.1-chromium",
                "2026.10.2-chromium",
                "2026.10.4-chromium"
            ]
        );
        assert!(bundles.join(ENGINE).join("bundle.toml").exists());
        assert!(bundles.join(NATIVE).join("bundle.toml").exists());
        assert!(!bundles.join("2026.10.1-chromium").exists());
    }

    #[test]
    fn a_runtime_reference_names_its_version() {
        assert_eq!(
            version_of("tilo-runtime/2026.07+native.ab").as_deref(),
            Some("2026.07+native.ab")
        );
        assert_eq!(version_of("tilo-runtime/"), None);
        assert_eq!(version_of(""), None);
    }
}
