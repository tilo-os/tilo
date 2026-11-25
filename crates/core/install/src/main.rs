//! tilo-install — the person's command line for Tilo packages (.tapp): install, update, remove,
//! pin, collect, trust, images and runtime bundles, and the reads beside them.
//!
//! While the device runs, every change is an action asked of this computer's daemon: an app's
//! install, update, pin or removal is the workspace tree's, which each of its devices follows,
//! and a runtime, an image, a collection or a check is the device cache's. So a change from a
//! terminal gets the same permissions and record as one from the Store, an agent or an
//! automation. An OS image build has no device, identity or daemon, so it names the store it
//! writes (`--offline-store DIR`) and the same change runs in this process. Reads (list,
//! inspect, run, the trust lists) read the store directly.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use clap::{Args, Parser, Subcommand};

mod trust_cmd;
mod workspace;
use package_actions::{Change, SavedData, Uploaded};
use package_store::installer::{
    self, Error, data_home_root, decode_hex_32, runtime_bundle_present,
};
use package_store::{InstalledPackage, PackageStore};

#[derive(Debug, Parser)]
#[command(
    name = "tilo-install",
    about = "Install, launch, list, or remove Tilo packages."
)]
struct Cli {
    /// Write the package store at DIR in this process, as an OS image build does: there is no
    /// device or identity to ask. Without it, changes go through this computer's device daemon:
    /// the workspace's app actions and its package cache.
    #[arg(long, global = true, value_name = "DIR")]
    offline_store: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

/// Trust-related flags shared by `install` and `run`.
#[derive(Debug, Args, Clone, Default)]
struct TrustFlags {
    /// Require the parsed tapp package-id/root to equal this 64-character hex hash.
    #[arg(long)]
    require_package_root: Option<String>,
    /// Require the tapp's embedded publisher to equal this 64-character hex key.
    #[arg(long)]
    require_publisher: Option<String>,
    /// Refuse to proceed unless signed by this trusted attestor id.
    #[arg(long)]
    require_attestor: Option<String>,
    /// Permit self-signed packages lacking a trusted-attestor signature.
    /// With --require-attestor, the attestor match is preferred and
    /// self-signed is only the fallback when it fails.
    #[arg(long)]
    allow_self_signed: bool,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Install a .tapp into the local store. Verifies signature first.
    #[command(alias = "add")]
    Install {
        /// Path to the .tapp file.
        tapp: PathBuf,
        /// Install the built app in DIR as this device's development build of it: the directory
        /// holds `tilo-app.toml` and the package's payload. The device marks it, packs and signs
        /// it with its own key, and it runs under its own id with no grants. Only the person on
        /// this device's own daemon connection may ask for one.
        #[arg(long)]
        dev: bool,
        /// Write the id the development build runs under here, as `--dev` installs it: the
        /// device names it, and a tool that installed the build starts the app by it.
        #[arg(long, requires = "dev")]
        id_file: Option<PathBuf>,
        /// Mark the package pinned (exempt from future GC eviction).
        #[arg(long)]
        pinned: bool,
        /// Overwrite an existing installation with the same package id.
        #[arg(long)]
        force: bool,
        /// Preserve an explicitly held user version during a development cohort update.
        #[arg(long, requires = "force")]
        preserve_held: bool,
        #[command(flatten)]
        trust: TrustFlags,
        #[command(flatten)]
        runtime: RuntimeFlags,
        /// Print exactly what would change and exit, touching nothing.
        #[arg(long)]
        dry_run: bool,
        /// Registry server to fetch payload blobs the .tapp does not carry
        /// from (`<url>/blobs/<hash>`). A thin package needs one unless its
        /// payload is already in the content store.
        #[arg(long)]
        payload_from: Option<String>,
    },
    /// Fetch a newer version of an installed package and swap it in,
    /// keeping the user's per-workspace state.
    Update {
        /// Package id (as printed by `tilo-install list`); omit to update all.
        pkg_id: Option<String>,
        /// Update from a local .tapp instead of a configured registry
        /// (requires an explicit package id).
        #[arg(long)]
        from: Option<PathBuf>,
        /// Semver requirement to resolve against, e.g. `^2` (default: newest runnable).
        #[arg(long)]
        version: Option<String>,
        /// Permit downgrading an installed version (refused by default — usually
        /// a mis-resolved registry, not intent).
        #[arg(long)]
        allow_downgrade: bool,
        /// Print exactly what would change and exit, touching nothing.
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        trust: TrustFlags,
        #[command(flatten)]
        runtime: RuntimeFlags,
        /// Registry server to fetch payload blobs a `--from` .tapp does not
        /// carry from (`<url>/blobs/<hash>`).
        #[arg(long, requires = "from")]
        payload_from: Option<String>,
    },
    /// List the authoritative installed selections.
    List {
        /// Emit selected rows for maintained automation.
        #[arg(long)]
        json: bool,
        /// Include resolved per-user runtime locations for coherent updates.
        #[arg(long, requires = "json")]
        runtime_paths: bool,
    },
    /// Uninstall a package: payload, registry entry, PATH shims, and
    /// (opt-in) its saved state.
    #[command(alias = "remove")]
    Uninstall {
        /// Package id (as printed by `tilo-install list`).
        pkg_id: String,
        /// Also delete all saved state for this app — the user's data, not the
        /// publisher's; does not come back on reinstall.
        #[arg(long, conflicts_with = "keep_state")]
        purge: bool,
        /// Keep saved state without asking (default when there's no terminal to ask on).
        #[arg(long)]
        keep_state: bool,
        /// Print exactly what would be removed and exit, deleting
        /// nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Re-verify an installed package's payload against its recorded signature chain.
    Verify {
        /// Package id. Omit to verify every installed package.
        pkg_id: Option<String>,
    },
    /// Pin an installed package: kept, ranked first, never evicted under size pressure.
    Pin {
        /// Package id (as printed by `tilo-install list`).
        pkg_id: String,
    },
    /// Unpin an installed package (drops it back to the eviction pool).
    Unpin {
        /// Package id (as printed by `tilo-install list`).
        pkg_id: String,
    },
    /// Launch an installed package via tilo-launcher.
    Run {
        /// Package id.
        pkg_id: String,
        /// Forwarded to tilo-launcher.
        #[arg(long)]
        no_services: bool,
        /// Forwarded to tilo-launcher. Deprecated — apps with an installed
        /// `tilo-runtime/<v>` bundle don't need this; silently overridden when
        /// a bundle resolves.
        #[arg(long, hide = true)]
        minimal_libs: bool,
        /// Forwarded to tilo-launcher; falls back to --minimal-libs if the
        /// declared runtime bundle isn't installed.
        #[arg(long)]
        allow_missing_runtime: bool,
        #[command(flatten)]
        trust: TrustFlags,
    },
    /// Print manifest + install info for an installed package.
    #[command(alias = "info")]
    Inspect {
        /// Package id.
        pkg_id: String,
    },
    /// Manage the trusted-attestor registry.
    Trust {
        #[command(subcommand)]
        cmd: TrustCmd,
    },
    /// Revoke a publisher key.
    Revoke {
        /// The publisher's Ed25519 public key, hex encoded (64 chars).
        pubkey_hex: String,
        /// Free-form reason recorded next to the entry.
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// List revoked keys.
    Revoked {
        #[command(subcommand)]
        cmd: RevokedCmd,
    },
    /// Verify a .tapp as `install` would, store nothing, and print who
    /// signed it as JSON (`verdict`: `trusted` or `unverified`). Asked by
    /// `app install <file>` before it decides whether to ask the person.
    #[command(hide = true)]
    Check {
        /// Path to the .tapp file.
        tapp: PathBuf,
        #[command(flatten)]
        trust: TrustFlags,
    },
    /// Print the PATH-shim directory and a shell-rc snippet to prepend it.
    /// Internal: the session already puts the directory on PATH.
    #[command(hide = true)]
    Path,
    /// Reclaim disk nothing needs: apps past the store's size cap, payload
    /// trees no package names and runtime builds no installed package runs
    /// on. Pinned packages and runtimes, and whatever a running app has
    /// mounted, stay. Also runs after every install, update and uninstall.
    Gc {
        /// List the runtime builds that would be removed, removing nothing.
        #[arg(long)]
        dry_run: bool,
        /// How long a runtime installed without an app naming it is kept,
        /// in seconds (an app's first open installs its runtime first).
        #[arg(long, default_value_t = runtime_bundle::gc::DEFAULT_GRACE_SECS)]
        grace_secs: u64,
        /// The store's size cap in bytes; `0` evicts no app for size.
        #[arg(long, default_value_t = package_store::DEFAULT_MAX_STORE_BYTES)]
        max_store_bytes: u64,
    },
    /// Manage installed Tilo runtime bundles.
    Runtime {
        #[command(subcommand)]
        cmd: RuntimeCmd,
    },
    /// Manage environment images: `env-runtime` and `tool` packages, the
    /// read-only trees an environment's `/usr` and `/etc` are stacked from.
    /// Images are never launched and have no row in `list`.
    Image {
        #[command(subcommand)]
        cmd: ImageCmd,
    },
}

#[derive(Debug, Subcommand)]
enum ImageCmd {
    /// Add an image .tapp to the store. Its publisher must be a trusted
    /// image publisher, an attested one, or the one --publisher names.
    Add {
        /// Path to the .tapp file.
        tapp: PathBuf,
        /// Also trust this publisher (64 hex characters) for this image.
        #[arg(long)]
        publisher: Option<String>,
        /// Registry server to fetch payload blobs the .tapp does not carry
        /// from (`<url>/blobs/<hash>`).
        #[arg(long)]
        payload_from: Option<String>,
    },
    /// List the images this device holds, system stores first.
    List,
    /// Print the read-only tree of an image (`tapp:<hex>`), fetching it from
    /// the registry when no store holds it.
    Path {
        /// Package id, `tapp:<hex>`.
        id: String,
        /// Registry to fetch a missing image from; defaults to
        /// `$TILO_REGISTRY`, else `https://apps.tilo.so`.
        #[arg(long)]
        registry: Option<String>,
    },
    /// Remove an image from the user's store.
    Remove {
        /// Package id, `tapp:<hex>`.
        id: String,
    },
}

/// What to do when a package's declared runtime bundle isn't installed.
/// Checked at install time (not first launch) so `RuntimeBundleMissing`
/// is caught while the user is still looking, rather than as a mystery later.
#[derive(Debug, Args, Clone, Default)]
struct RuntimeFlags {
    /// Install even though the runtime bundle is missing; the package won't launch until it is.
    #[arg(long)]
    ignore_missing_runtime: bool,
}

#[derive(Debug, Subcommand)]
enum TrustCmd {
    /// Add or replace a trusted attestor.
    Add {
        /// Attestor id (as it appears in `[[trust.attested_by]].attestor`).
        #[arg(long)]
        id: String,
        /// The attestor's Ed25519 public key, hex encoded (64 chars).
        #[arg(long)]
        pubkey: String,
        /// Scope: `all` | `mime:<a>[,<b>...]` | `prefix:<a>[,<b>...]`.
        #[arg(long, default_value = "all")]
        scope: String,
    },
    /// List trusted attestors.
    List,
    /// Remove a trusted attestor by id.
    Remove {
        /// Attestor id to remove.
        id: String,
    },
}

#[derive(Debug, Subcommand)]
enum RevokedCmd {
    /// List revoked keys.
    List,
}

#[derive(Debug, Subcommand)]
enum RuntimeCmd {
    /// Build + install a runtime bundle into
    /// `$XDG_DATA_HOME/tilo-runtime/<version>/`, from the host root or an
    /// explicit prefix.
    Install {
        /// Bundle version tag, written into bundle.toml and used to look it up at launch.
        #[arg(long)]
        version: String,
        /// Copy from the running host root. Equivalent to `--from /`.
        #[arg(long)]
        from_host: bool,
        /// Copy from an explicit host prefix. Wins over `--from-host`.
        #[arg(long)]
        from: Option<PathBuf>,
        /// Target architecture (default: host arch); lets you build a
        /// cross-arch bundle from a fabricated rootfs.
        #[arg(long)]
        arch: Option<String>,
        /// Path to a signing identity JSON (see `tilo-runtime-bundle key
        /// generate`); the launcher refuses unsigned bundles by default, so
        /// most installs should sign.
        #[arg(long)]
        identity: Option<PathBuf>,
        /// Overwrite an existing bundle at the same version.
        #[arg(long)]
        force: bool,
    },
    /// List installed runtime bundle versions.
    List,
    /// Remove an installed runtime bundle by version tag. A bundle a
    /// running app has mounted is taken out of use now and deleted by the
    /// first collection after the app closes.
    Remove { version: String },
    /// Keep a runtime bundle even when no installed package runs on it.
    Pin { version: String },
    /// Let collection remove a runtime bundle again once nothing needs it.
    Unpin { version: String },
    /// Print the resolved path for a version (same lookup order as the launcher).
    Path {
        #[arg(long)]
        version: String,
    },
    /// Verify an installed bundle's on-disk hash matches its manifest.
    Verify { version: String },
}

/// The store `--offline-store` names, set once at start.
static OFFLINE_STORE: OnceLock<PathBuf> = OnceLock::new();

fn data_root() -> Result<PathBuf, Error> {
    match OFFLINE_STORE.get() {
        Some(root) => Ok(root.clone()),
        None => package_store::layout::default_user_root().ok_or(Error::NoDataDir),
    }
}

fn store() -> Result<PackageStore, Error> {
    Ok(PackageStore::at(data_root()?))
}

fn bin_root() -> Result<PathBuf, Error> {
    Ok(store()?.layout().bin())
}

fn catalog() -> Result<package_store::Catalog, Error> {
    Ok(package_store::Catalog::at(
        data_root()?,
        package_store::SystemStore::from_env(),
    ))
}

fn list_catalog() -> Result<Vec<InstalledPackage>, Error> {
    catalog()?
        .list()
        .map_err(|e| Error::RegistryRead(e.to_string()))
}

fn layers(pkg_id: &str) -> Result<package_store::Layers, Error> {
    catalog()?
        .layers(pkg_id)
        .map_err(|e| Error::RegistryRead(e.to_string()))
}

fn find_installed(pkg_id: &str) -> Result<InstalledPackage, Error> {
    layers(pkg_id)?
        .running()
        .cloned()
        .ok_or_else(|| Error::NotInstalled(pkg_id.into()))
}

/// Trust-related flags as the installer takes them, read against the workspace's
/// trust list.
impl From<&TrustFlags> for installer::Trust {
    fn from(flags: &TrustFlags) -> Self {
        let (attestors, revoked) = workspace::trust();
        Self {
            require_package_root: flags.require_package_root.clone(),
            require_publisher: flags.require_publisher.clone(),
            require_attestor: flags.require_attestor.clone(),
            allow_self_signed: flags.allow_self_signed,
            attestors: attestors
                .into_iter()
                .filter_map(|attestor| {
                    Some(tilo_app_package::trust::TrustedAttestor {
                        scope: tilo_app_package::trust::AttestorScope::parse(&attestor.scope)
                            .ok()?,
                        id: attestor.id,
                        pubkey_hex: attestor.pubkey_hex,
                    })
                })
                .collect(),
            revoked: revoked
                .into_iter()
                .map(|key| tilo_app_package::trust::RevokedKey {
                    pubkey_hex: key.pubkey_hex,
                    reason: key.reason,
                })
                .collect(),
        }
    }
}

/// The trust flags as the cache's changes take them, with the workspace's
/// trust list beside them.
fn wire_trust(flags: &TrustFlags) -> package_actions::Trust {
    let (attestors, revoked) = workspace::trust();
    package_actions::Trust {
        require_package_root: flags.require_package_root.clone(),
        require_publisher: flags.require_publisher.clone(),
        require_attestor: flags.require_attestor.clone(),
        allow_self_signed: flags.allow_self_signed,
        attestors: attestors
            .into_iter()
            .filter_map(|attestor| {
                Some(tilo_app_package::trust::TrustedAttestor {
                    scope: tilo_app_package::trust::AttestorScope::parse(&attestor.scope).ok()?,
                    id: attestor.id,
                    pubkey_hex: attestor.pubkey_hex,
                })
            })
            .collect(),
        revoked: revoked
            .into_iter()
            .map(|key| tilo_app_package::trust::RevokedKey {
                pubkey_hex: key.pubkey_hex,
                reason: key.reason,
            })
            .collect(),
    }
}

/// Make `change` and show what it did: in this process for the store `--offline-store` names, else
/// through this computer's daemon.
fn make(change: Change) -> Result<(), String> {
    make_report(change).map(|_| ())
}

/// [`make`], answering what the change did: the id a development build installed under among it.
fn make_report(change: Change) -> Result<package_actions::Report, String> {
    let report = match OFFLINE_STORE.get() {
        Some(root) => {
            let store = PackageStore::at(root);
            let catalog = package_store::Catalog::at(root, package_store::SystemStore::from_env());
            package_actions::apply(
                &package_actions::Target {
                    store: &store,
                    catalog: &catalog,
                    max_store_bytes: package_store::DEFAULT_MAX_STORE_BYTES,
                    device: None,
                },
                change,
                &package_actions::Entry::ImageBuild,
            )?
        }
        None => package_actions::device::make(change)?,
    };
    for warning in &report.warnings {
        eprintln!("tilo-install: {warning}");
    }
    for line in &report.lines {
        println!("{line}");
    }
    Ok(report)
}

fn uploaded(path: &Path) -> Uploaded {
    Uploaded {
        source: path.display().to_string(),
    }
}

/// The manifest a built app directory names itself by.
const BUILD_MANIFEST: &str = "tilo-app.toml";

/// Every file below `dir`, by its path in the package's payload: the build's own manifest is the
/// package's, not one of its files.
fn payload_files(dir: &Path) -> Result<Vec<package_actions::HostFile>, String> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(at) = pending.pop() {
        let entries = fs::read_dir(&at).map_err(|error| format!("{}: {error}", at.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() {
                let relative = path
                    .strip_prefix(dir)
                    .map_err(|error| format!("{}: {error}", path.display()))?
                    .to_string_lossy()
                    .replace('\\', "/");
                if relative == BUILD_MANIFEST {
                    continue;
                }
                files.push(package_actions::HostFile {
                    path: relative,
                    file: uploaded(&path),
                });
            }
        }
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn list_json(include_runtime_paths: bool) -> Result<(), Error> {
    let rows = list_catalog()?;
    if rows.len() > 512 {
        return Err(Error::RegistryRead(
            "selected package count exceeds automation bound".into(),
        ));
    }
    let value = if include_runtime_paths {
        let mut paths = std::collections::BTreeMap::new();
        for row in &rows {
            if let Some(version) = row.runtime.strip_prefix("tilo-runtime/") {
                let path = runtime_bundle::default_bundle_path(version)
                    .map_err(|error| Error::RuntimeBundle(error.to_string()))?;
                if path
                    .join(runtime_bundle::BUNDLE_MANIFEST_FILENAME)
                    .is_file()
                {
                    paths.insert(version, path);
                }
            }
        }
        serde_json::json!({"apps": rows, "runtime_paths": paths})
    } else {
        serde_json::json!(rows)
    };
    let json =
        serde_json::to_string(&value).map_err(|error| Error::RegistryRead(error.to_string()))?;
    if json.len() > 8 * 1024 * 1024 {
        return Err(Error::RegistryRead(
            "selected package metadata exceeds automation bound".into(),
        ));
    }
    println!("{json}");
    Ok(())
}

fn list() -> Result<(), Error> {
    let entries = list_catalog()?;
    if entries.is_empty() {
        println!("(no packages installed)");
        return Ok(());
    }
    println!(
        "{:<32}  {:<8}  {:<6}  {:<6}  {:<18}  {:<18}  installed",
        "id", "version", "from", "pinned", "attested-by", "runtime"
    );
    for e in &entries {
        let ago = age(e.installed_at);
        let attested = if e.attested_by.is_empty() {
            "-"
        } else {
            &e.attested_by
        };
        // A package whose runtime bundle has gone missing still lists, flagged: a bare list would
        // hide that it cannot launch.
        let runtime = match e.runtime.as_str() {
            "" => "-".to_string(),
            r => {
                let version = r.split_once('/').map_or("", |(_, v)| v);
                if runtime_bundle_present(version) {
                    r.to_string()
                } else {
                    format!("{r} (MISSING)")
                }
            }
        };
        println!(
            "{:<32}  {:<8}  {:<6}  {:<6}  {:<18}  {:<18}  {}",
            e.id,
            e.version,
            installed_from(e),
            if e.pinned { "yes" } else { "no" },
            attested,
            runtime,
            ago
        );
    }
    Ok(())
}

/// Which store a listed package runs from: the image's, or the user's (`held` when rolled back
/// below the image's version).
fn installed_from(entry: &InstalledPackage) -> &'static str {
    if entry.system {
        "image"
    } else if entry.held {
        "held"
    } else {
        "user"
    }
}

/// Uninstall a package, asking about its saved data first when no flag decided: the payload is the
/// publisher's, reproducible by reinstalling; the per-workspace data is the person's and does not
/// come back, so "no" leaves everything as it was.
fn uninstall(pkg_id: &str, purge: bool, keep_state: bool, dry_run: bool) -> Result<(), String> {
    let saved_data = if purge {
        SavedData::Delete
    } else if keep_state {
        SavedData::Keep
    } else if dry_run {
        SavedData::Ask
    } else {
        let workspaces = package_store::state::find(
            &data_home_root().map_err(|error| error.to_string())?,
            pkg_id,
        );
        if decide_state(&workspaces) {
            SavedData::Delete
        } else {
            SavedData::Keep
        }
    };
    make(Change::Uninstall {
        app: pkg_id.to_owned(),
        saved_data,
        dry_run,
    })
}

fn run(
    pkg_id: &str,
    no_services: bool,
    minimal_libs: bool,
    allow_missing_runtime: bool,
    trust: &TrustFlags,
) -> Result<(), Error> {
    let entry = find_installed(pkg_id)?;

    if let Some(required) = trust.require_package_root.as_deref() {
        decode_hex_32(required).map_err(Error::InvalidRequiredPackageRoot)?;
        if !required.eq_ignore_ascii_case(&entry.package_id_hex) {
            return Err(Error::PackageRootMismatch {
                required: required.to_ascii_lowercase(),
                found: entry.package_id_hex,
            });
        }
    }
    if let Some(required) = trust.require_publisher.as_deref() {
        decode_hex_32(required).map_err(Error::InvalidRequiredPublisher)?;
        if !required.eq_ignore_ascii_case(&entry.publisher_key_hex) {
            return Err(Error::PublisherMismatch {
                required: required.to_ascii_lowercase(),
                found: entry.publisher_key_hex,
            });
        }
    }

    // Check trust policy again at launch: a revocation may postdate the install, and
    // `--require-attestor` is checked against the recorded `attested_by`.
    let (attestors, revoked) = workspace::trust();
    if let Some(rev) = revoked.iter().find(|key| {
        key.pubkey_hex
            .eq_ignore_ascii_case(&entry.publisher_key_hex)
    }) {
        eprintln!(
            "tilo-install: REFUSED — installed package's publisher is revoked: {}",
            rev.reason
        );
        return Err(Error::Revoked {
            publisher: entry.publisher_key_hex.clone(),
            reason: rev.reason.clone(),
        });
    }
    if let Some(required) = &trust.require_attestor {
        // The recorded `attested_by` is fixed at install time and goes stale when
        // the workspace's trust list changes, so check it is still trusted.
        let still_trusted = attestors.iter().any(|a| a.id == entry.attested_by);
        if entry.attested_by != *required || !still_trusted {
            if !trust.allow_self_signed {
                return Err(Error::AttestorMismatch {
                    required: required.clone(),
                    found: if entry.attested_by.is_empty() {
                        "self-signed".into()
                    } else {
                        entry.attested_by
                    },
                });
            }
            eprintln!(
                "tilo-install: --require-attestor {required:?} unmatched; --allow-self-signed permits install-time attested-by {:?}",
                entry.attested_by
            );
        }
    }
    // `tilo-launcher` on PATH, else a sibling of this binary.
    let launcher = which_launcher();
    let mut cmd = Command::new(&launcher);
    cmd.arg("run")
        .arg("--manifest")
        .arg(&entry.manifest_path)
        .arg("--payload-root")
        .arg(&entry.payload_root);
    if no_services {
        cmd.arg("--no-services");
    }
    if minimal_libs {
        cmd.arg("--minimal-libs");
    }
    if allow_missing_runtime {
        cmd.arg("--allow-missing-runtime");
    }
    // exec() replaces this process, so its exit code is the launcher's.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        Err(Error::Io(cmd.exec()))
    }
    // Windows replaces no process: the launcher runs to its end, and its failure is this one's.
    #[cfg(not(unix))]
    {
        let status = cmd.status().map_err(Error::Io)?;
        if status.success() {
            Ok(())
        } else {
            Err(Error::Io(std::io::Error::other(format!(
                "{} {status}",
                launcher.display()
            ))))
        }
    }
}

fn inspect(pkg_id: &str) -> Result<(), Error> {
    let entry = find_installed(pkg_id)?;
    println!("id            : {}", entry.id);
    println!("version       : {}", entry.version);
    println!("package-id    : tapp:{}", entry.package_id_hex);
    println!("publisher     : ed25519:{}", entry.publisher_key_hex);
    println!("payload-root  : {}", entry.payload_root.display());
    println!("manifest      : {}", entry.manifest_path.display());
    println!("pinned        : {}", entry.pinned);
    println!(
        "attested-by   : {}",
        if entry.attested_by.is_empty() {
            "(self-signed)"
        } else {
            &entry.attested_by
        }
    );
    println!(
        "installed     : {} seconds ago",
        tilo_clock::unix_now().saturating_sub(entry.installed_at)
    );
    if entry.runtime.is_empty() {
        println!("runtime       : (none declared)");
    } else {
        let version = entry.runtime.split_once('/').map_or("", |(_, v)| v);
        println!(
            "runtime       : {} ({})",
            entry.runtime,
            if runtime_bundle_present(version) {
                "installed"
            } else {
                "MISSING — this package cannot launch"
            }
        );
    }
    if !entry.source.is_empty() {
        println!("source        : {}", entry.source);
    }
    let record = package_store::integrity::record_dir(&entry);
    println!(
        "integrity     : {}",
        if record
            .join(package_store::layout::SIGNED_ROOT_FILE)
            .is_file()
        {
            "recorded (run `tilo-install verify` to replay it)"
        } else {
            "no record — installed before integrity records, or the record was deleted"
        }
    );
    let workspaces = package_store::state::find(&data_home_root()?, pkg_id);
    if workspaces.is_empty() {
        println!("saved data    : none");
    } else {
        println!(
            "saved data    : {} workspace(s), {}",
            workspaces.len(),
            package_store::state::human_bytes(package_store::state::total_bytes(&workspaces))
        );
        for saved in &workspaces {
            println!(
                "                {}  {}",
                saved.workspace,
                saved.path.display()
            );
        }
    }
    Ok(())
}

/// Whether to delete the app's saved data, asked when there is any: with no terminal to ask on the
/// answer is keep, as silent, unattended data deletion is the one failure that cannot be undone.
fn decide_state(workspaces: &[package_store::state::WorkspaceState]) -> bool {
    if workspaces.is_empty() {
        return false;
    }
    for line in package_store::state::summary(workspaces) {
        println!("{line}");
    }
    if !stdin_is_a_terminal() {
        println!(
            "  (no terminal to ask on — keeping it. Re-run with --purge to delete, \
             or --keep-state to silence this.)"
        );
        return false;
    }
    print!("Delete this app's saved data too? [y/N] ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let mut answer = String::new();
    if std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn stdin_is_a_terminal() -> bool {
    // SAFETY: isatty(3) takes a file descriptor and returns a flag; it
    // cannot fail in a way that matters here.
    unsafe { libc_isatty(0) == 1 }
}

unsafe extern "C" {
    #[link_name = "isatty"]
    fn libc_isatty(fd: i32) -> i32;
}

fn print_path() -> Result<(), Error> {
    let bin_dir = bin_root()?;
    println!("{}", bin_dir.display());
    println!();
    println!("# Prepend the above to your shell's PATH so CLI commands");
    println!("# installed via `tilo-install install <pkg.tapp>` are picked up.");
    println!("#");
    println!("# bash / zsh (~/.bashrc, ~/.zshrc):");
    println!("export PATH=\"{}:$PATH\"", bin_dir.display());
    println!("#");
    println!("# fish (~/.config/fish/config.fish):");
    println!("fish_add_path -pg {}", bin_dir.display());
    Ok(())
}

fn runtime_list() -> Result<(), Error> {
    let base = tilo_paths::data_dir()
        .ok_or(Error::NoDataDir)?
        .join("tilo-runtime");
    if !base.exists() {
        println!("(no runtime bundles installed)");
        return Ok(());
    }
    let mut versions: Vec<String> = Vec::new();
    for entry in fs::read_dir(&base)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if entry
            .path()
            .join(runtime_bundle::BUNDLE_MANIFEST_FILENAME)
            .exists()
        {
            versions.push(name);
        }
    }
    if versions.is_empty() {
        println!("(no runtime bundles installed)");
        return Ok(());
    }
    versions.sort();
    println!("{:<20}  path", "version");
    for v in &versions {
        println!("{:<20}  {}", v, base.join(v).display());
    }
    Ok(())
}

fn runtime_path(version: &str) -> Result<(), Error> {
    if let Some(p) = runtime_bundle::resolve_bundle_path(version) {
        println!("{}", p.display());
        return Ok(());
    }
    let per_user = runtime_bundle::default_bundle_path(version)
        .map_err(|e| Error::RuntimeBundle(e.to_string()))?;
    println!("{}", per_user.display());
    Ok(())
}

fn runtime_verify(version: &str) -> Result<(), Error> {
    let path = runtime_bundle::resolve_bundle_path(version)
        .ok_or_else(|| Error::RuntimeBundleNotInstalled(version.to_string()))?;
    let manifest =
        runtime_bundle::verify_bundle(&path).map_err(|e| Error::RuntimeBundle(e.to_string()))?;
    println!("verified: tilo-runtime/{version}");
    println!("  path     : {}", path.display());
    println!("  files    : {}", manifest.files.len());
    println!("  root-hash: blake3:{}", manifest.root_hash);
    Ok(())
}

fn runtime(cmd: RuntimeCmd) -> Result<(), String> {
    match cmd {
        RuntimeCmd::Install {
            version,
            from_host,
            from,
            arch,
            identity,
            force,
        } => {
            // An explicit prefix wins over the host root; neither builds from the host's
            // defaults.
            let from = from.or_else(|| from_host.then(|| PathBuf::from("/")));
            make(Change::InstallRuntime {
                from: from
                    .map(|root| {
                        let arch = arch
                            .as_deref()
                            .unwrap_or_else(|| runtime_bundle::host_arch());
                        runtime_bundle::host_root_files(&root, arch).map(|files| {
                            files
                                .into_iter()
                                .map(|(path, at)| package_actions::HostFile {
                                    path: path.to_owned(),
                                    file: uploaded(&at),
                                })
                                .collect()
                        })
                    })
                    .transpose()
                    .map_err(|error| error.to_string())?,
                version,
                arch,
                identity: identity.as_deref().map(uploaded),
                force,
            })
        }
        RuntimeCmd::List => runtime_list().map_err(|error| error.to_string()),
        RuntimeCmd::Remove { version } => make(Change::RemoveRuntime { version }),
        RuntimeCmd::Pin { version } => make(Change::PinRuntime {
            version,
            pinned: true,
        }),
        RuntimeCmd::Unpin { version } => make(Change::PinRuntime {
            version,
            pinned: false,
        }),
        RuntimeCmd::Path { version } => runtime_path(&version).map_err(|error| error.to_string()),
        RuntimeCmd::Verify { version } => {
            runtime_verify(&version).map_err(|error| error.to_string())
        }
    }
}

fn main() {
    let cli = Cli::parse();
    if let Some(root) = cli.offline_store {
        let _ = OFFLINE_STORE.set(root);
    }
    if let Err(e) = dispatch(cli.cmd) {
        eprintln!("tilo-install: {e}");
        std::process::exit(1);
    }
}

fn read(result: Result<(), Error>) -> Result<(), String> {
    result.map_err(|error| error.to_string())
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per command, each a read or a change"
)]
fn dispatch(cmd: Cmd) -> Result<(), String> {
    match cmd {
        Cmd::Install {
            tapp,
            dev,
            id_file,
            pinned,
            force,
            preserve_held,
            trust,
            runtime,
            dry_run,
            payload_from,
        } => {
            if dev {
                if pinned || force || preserve_held || dry_run || payload_from.is_some() {
                    return Err(
                        "a development build is installed as it is: no --pinned, --force, \
                         --preserve-held, --dry-run or --payload-from"
                            .into(),
                    );
                }
                let report = make_report(Change::InstallDev {
                    manifest: uploaded(&tapp.join(BUILD_MANIFEST)),
                    payload: payload_files(&tapp)?,
                })?;
                if let Some(path) = id_file {
                    let id = report.installed.ok_or_else(|| {
                        format!(
                            "the device installed {} but named no id it runs under",
                            tapp.display()
                        )
                    })?;
                    fs::write(&path, format!("{id}\n"))
                        .map_err(|error| format!("{}: {error}", path.display()))?;
                }
                return Ok(());
            }
            make(Change::Install {
                file: uploaded(&tapp),
                trust: wire_trust(&trust),
                pinned,
                replacement: match (force, preserve_held) {
                    (false, _) => package_actions::Replacement::Refuse,
                    (true, false) => package_actions::Replacement::Replace,
                    (true, true) => package_actions::Replacement::PreserveHeld,
                },
                ignore_missing_runtime: runtime.ignore_missing_runtime,
                dry_run,
                payload_from,
            })
        }
        Cmd::Update {
            pkg_id,
            from,
            version,
            allow_downgrade,
            dry_run,
            trust,
            runtime,
            payload_from,
        } => make(Change::Update {
            app: pkg_id,
            file: from.as_deref().map(uploaded),
            version,
            allow_downgrade,
            dry_run,
            trust: wire_trust(&trust),
            ignore_missing_runtime: runtime.ignore_missing_runtime,
            payload_from,
        }),
        Cmd::List {
            json: true,
            runtime_paths,
        } => read(list_json(runtime_paths)),
        Cmd::List { json: false, .. } => read(list()),
        Cmd::Uninstall {
            pkg_id,
            purge,
            keep_state,
            dry_run,
        } => uninstall(&pkg_id, purge, keep_state, dry_run),
        Cmd::Verify { pkg_id } => make(Change::Verify { app: pkg_id }),
        Cmd::Pin { pkg_id } => make(Change::SetPinned {
            app: pkg_id,
            pinned: true,
        }),
        Cmd::Unpin { pkg_id } => make(Change::SetPinned {
            app: pkg_id,
            pinned: false,
        }),
        Cmd::Run {
            pkg_id,
            no_services,
            minimal_libs,
            allow_missing_runtime,
            trust,
        } => read(run(
            &pkg_id,
            no_services,
            minimal_libs,
            allow_missing_runtime,
            &trust,
        )),
        Cmd::Inspect { pkg_id } => read(inspect(&pkg_id)),
        Cmd::Trust { cmd } => match cmd {
            TrustCmd::Add { id, pubkey, scope } => make(Change::AddAttestor { id, pubkey, scope }),
            TrustCmd::List => {
                trust_cmd::trust_list();
                Ok(())
            }
            TrustCmd::Remove { id } => make(Change::RemoveAttestor { id }),
        },
        Cmd::Revoke { pubkey_hex, reason } => make(Change::Revoke {
            pubkey: pubkey_hex,
            reason,
        }),
        Cmd::Revoked { cmd } => match cmd {
            RevokedCmd::List => {
                trust_cmd::revoked_list();
                Ok(())
            }
        },
        Cmd::Check { tapp, trust } => read(trust_cmd::check_verdict(&tapp, &trust)),
        Cmd::Path => read(print_path()),
        Cmd::Gc {
            dry_run,
            grace_secs,
            max_store_bytes,
        } => make(Change::Collect {
            dry_run,
            grace_secs,
            max_store_bytes,
        }),
        Cmd::Runtime { cmd } => runtime(cmd),
        Cmd::Image { cmd } => image(cmd),
    }
}

fn image(cmd: ImageCmd) -> Result<(), String> {
    match cmd {
        ImageCmd::Add {
            tapp,
            publisher,
            payload_from,
        } => make(Change::AddImage {
            file: uploaded(&tapp),
            publisher,
            payload_from,
            trust: wire_trust(&TrustFlags::default()),
        }),
        ImageCmd::List => {
            for meta in package_actions::image_store(
                &data_root().map_err(|error| error.to_string())?,
                &wire_trust(&TrustFlags::default()),
            )
            .and_then(|store| store.images().map_err(installer::store_error))
            .map_err(|error| error.to_string())?
            {
                let runtime = meta
                    .manifest
                    .image
                    .as_ref()
                    .and_then(|image| image.runtime)
                    .map(|runtime| format!(" on {runtime}"))
                    .unwrap_or_default();
                println!(
                    "{} {} {} {}{runtime}",
                    meta.id,
                    meta.manifest.package.kind,
                    meta.manifest.package.id,
                    meta.manifest.package.version
                );
            }
            Ok(())
        }
        ImageCmd::Path { id, registry } => {
            package_actions::image_id(&id)?;
            let registry = registry
                .or_else(|| {
                    std::env::var("TILO_REGISTRY")
                        .ok()
                        .filter(|url| !url.is_empty())
                })
                .unwrap_or_else(|| "https://apps.tilo.so".to_owned());
            make(Change::FetchImage {
                id,
                registry,
                trust: wire_trust(&TrustFlags::default()),
            })
        }
        ImageCmd::Remove { id } => make(Change::RemoveImage { id }),
    }
}

fn age(installed_at: u64) -> String {
    let n = tilo_clock::unix_now();
    let elapsed = n.saturating_sub(installed_at);
    match elapsed {
        0..=59 => format!("{elapsed}s ago"),
        60..=3599 => format!("{}m ago", elapsed / 60),
        3600..=86399 => format!("{}h ago", elapsed / 3600),
        _ => format!("{}d ago", elapsed / 86400),
    }
}

fn which_launcher() -> PathBuf {
    // A sibling binary first: development builds put both in `target/release`.
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let sib = parent.join("tilo-launcher");
        if sib.exists() {
            return sib;
        }
    }
    PathBuf::from("tilo-launcher")
}
