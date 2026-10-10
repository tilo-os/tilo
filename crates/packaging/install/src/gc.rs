//! Reclaiming what no installed package needs: app payloads past the
//! store's size policy, payload trees nothing names, and per-user runtime
//! bundles no installed package runs on.
//!
//! It runs when something changed — after every `tilo-install install`,
//! `update` and `uninstall`, and on `tilo-install gc` — never on a timer.
//! What a running app has mounted stays ([`package_store::mounts`]), and so
//! does what the user keeps: pinned packages, and pinned runtimes.
//!
//! A runtime is needed while any installed package names it: as its
//! `runtime`, or among its manifest's `optional-runtimes`. Both the user's
//! store and the image's count. A manifest that cannot be read makes the
//! whole runtime collection stop: its runtimes cannot be told apart.

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    /// The store's size cap; `0` evicts nothing for size.
    pub max_store_bytes: u64,
    /// How long a freshly installed runtime stays unreferenced.
    pub grace_secs: u64,
    /// Unix seconds now.
    pub now: u64,
    /// Report the runtimes that would go; touch nothing.
    pub dry_run: bool,
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
    let mut report = Report::default();
    // A device with no app store yet has no payloads; collecting must not
    // create one.
    if !request.dry_run && store.layout().registry().is_file() {
        report.evicted = store
            .sweep(request.max_store_bytes)
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
        referenced_runtimes(rows.iter().chain(user.entries()))
    };
    report.runtimes = runtimes
        .collect(
            &references,
            &in_use_now,
            Options {
                grace_secs: request.grace_secs,
                now: request.now,
                dry_run: request.dry_run,
            },
        )
        .map_err(|error| format!("runtime bundles: {error}"))?;
    Ok(report)
}

/// The runtime versions `rows` run on or may mount.
///
/// # Errors
///
/// Returns the error reading or parsing a row's manifest.
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
        let manifest = tilo_app_package::Manifest::from_toml_str(&text)
            .map_err(|error| io::Error::other(format!("{}: manifest: {error}", row.id)))?;
        versions.extend(manifest.package.runtime.as_deref().and_then(version_of));
        versions.extend(
            manifest
                .package
                .optional_runtimes
                .iter()
                .filter_map(|raw| version_of(raw)),
        );
    }
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
