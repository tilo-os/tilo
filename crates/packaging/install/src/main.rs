//! tilo-install — the user-facing CLI to install, run, and manage Tilo
//! packages (.tapp) locally, verifying the package signature and trust
//! chain before installing.
//!
//! Packages go into the device's package store — the same registry the
//! launcher, the shell and the app host read — with their payload bytes in
//! the content store and each tree materialised once from it. Installing and
//! updating are [`package_store::installer`]'s; this prints what it did.

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Args, Parser, Subcommand};

mod offered;
mod trust_cmd;
use package_store::installer::{
    self, Error, data_home_root, decode_hex_32, load_revocation_list, load_trusted_attestors,
    revoked_keys_path, runtime_bundle_present, store_error, trusted_attestors_path,
};
use package_store::{Admission, InstalledPackage, PackageStore, shim};
use tilo_app_package::{BlobFetcher, PackageId};

#[derive(Debug, Parser)]
#[command(
    name = "tilo-install",
    about = "Install, launch, list, or remove Tilo packages."
)]
struct Cli {
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

fn data_root() -> Result<PathBuf, Error> {
    package_store::layout::default_user_root().ok_or(Error::NoDataDir)
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

/// Trust-related flags as the installer takes them.
impl From<&TrustFlags> for installer::Trust {
    fn from(flags: &TrustFlags) -> Self {
        Self {
            require_package_root: flags.require_package_root.clone(),
            require_publisher: flags.require_publisher.clone(),
            require_attestor: flags.require_attestor.clone(),
            allow_self_signed: flags.allow_self_signed,
        }
    }
}

/// Show what an install or update did: its warnings on stderr, its lines on stdout.
fn print_report(report: &installer::Report) {
    for warning in &report.warnings {
        eprintln!("tilo-install: {warning}");
    }
    for line in &report.lines {
        println!("{line}");
    }
}

struct InstallOptions {
    pinned: bool,
    replacement: installer::Replacement,
    dry_run: bool,
    /// Where payload blobs the archive does not carry come from.
    payload_from: Option<app_registry::HttpBlobs>,
}

fn install(
    tapp: &Path,
    options: &InstallOptions,
    trust: &TrustFlags,
    runtime: &RuntimeFlags,
) -> Result<(), Error> {
    let store = store()?;
    let report = installer::install(
        &store,
        &catalog()?,
        &installer::Candidate::file(tapp),
        &installer::Request {
            trust: trust.into(),
            pinned: options.pinned,
            replacement: options.replacement,
            ignore_missing_runtime: runtime.ignore_missing_runtime,
            dry_run: options.dry_run,
        },
        options
            .payload_from
            .as_ref()
            .map(|source| source as &dyn BlobFetcher),
    )?;
    print_report(&report);
    if report.outcome == installer::Outcome::Installed
        && let Some(selected) = &report.selected
    {
        #[cfg(all(unix, not(target_os = "macos")))]
        register_search_providers();
        println!("run with:  tilo-install run {}", selected.id);
    }
    Ok(())
}

/// Let GNOME Shell and `KRunner` search the resources the runtime beside this binary admits. Best
/// effort: an app installs and runs without it.
#[cfg(all(unix, not(target_os = "macos")))]
fn register_search_providers() {
    let Some(runtime) = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("tilo-runtime")))
        .filter(|runtime| runtime.is_file())
    else {
        return;
    };
    let written = data_home_root()
        .map_err(|error| error.to_string())
        .and_then(|data| {
            let config = tilo_paths::config_dir().ok_or("no configuration directory")?;
            tilo_os_search::linux_files::write(&data, &config, &runtime)
                .map_err(|error| error.to_string())
        });
    match written {
        Ok(()) => println!(
            "  search   : KRunner finds its resources now, GNOME Shell after the next login"
        ),
        Err(error) => eprintln!("tilo-install: host search not registered: {error}"),
    }
}

/// Fetch a newer version and swap it in, keeping the user's per-workspace data.
fn update(pkg_id: Option<&str>, request: &UpdateRequest<'_>) -> Result<(), Error> {
    let targets: Vec<InstalledPackage> = if let Some(id) = pkg_id {
        vec![find_installed(id)?]
    } else {
        if request.from.is_some() {
            return Err(Error::Policy(
                "--from names one .tapp, so it needs one package id".into(),
            ));
        }
        if !request.dry_run {
            drop_superseded()?;
        }
        list_catalog()?
    };
    if targets.is_empty() {
        println!("(no packages installed)");
        return Ok(());
    }

    let mut updated: u32 = 0;
    let mut current: u32 = 0;
    for entry in &targets {
        match update_one(entry, request)? {
            installer::Outcome::Planned => {}
            installer::Outcome::AlreadyCurrent => current = current.saturating_add(1),
            _ => updated = updated.saturating_add(1),
        }
    }
    if targets.len() > 1 {
        println!("{updated} updated, {current} already current");
    }
    Ok(())
}

/// Remove the user's copies of apps the image now ships a newer version of: nothing runs them any
/// more.
fn drop_superseded() -> Result<(), Error> {
    let superseded = catalog()?
        .superseded()
        .map_err(|e| Error::RegistryRead(e.to_string()))?;
    if superseded.is_empty() {
        return Ok(());
    }
    let store = store()?;
    for entry in superseded {
        store.remove(&entry.id, true).map_err(store_error)?;
        if !entry.cli_commands.is_empty() {
            let _ = shim::remove_shims(&bin_root()?, &entry.id, &entry.cli_commands);
        }
        println!(
            "{} {}: the system image ships a newer version; removed this copy",
            entry.id, entry.version
        );
    }
    Ok(())
}

struct UpdateRequest<'a> {
    from: Option<&'a Path>,
    version_req: Option<&'a str>,
    allow_downgrade: bool,
    dry_run: bool,
    trust: &'a TrustFlags,
    runtime: &'a RuntimeFlags,
    /// Where payload blobs a `--from` archive does not carry come from.
    payload_from: Option<app_registry::HttpBlobs>,
}

fn update_one(
    entry: &InstalledPackage,
    request: &UpdateRequest<'_>,
) -> Result<installer::Outcome, Error> {
    // The candidate is a local file or the configured registries' offer; either way the installer
    // verifies it before anything is stored. A registry serves thin packages: their payload comes
    // from the node that served them, and only the blobs the store lacks are fetched.
    let (staged, candidate, trust, provider) = if let Some(path) = request.from {
        (
            None,
            installer::Candidate::file(path),
            installer::Trust::from(request.trust),
            None,
        )
    } else {
        let offered = offered::fetch_from_registries(entry, request.version_req, request.trust)?;
        let candidate = installer::Candidate::staged(offered.staged.path(), offered.source);
        (
            Some(offered.staged),
            candidate,
            installer::Trust::from(&offered.trust),
            offered.payload,
        )
    };
    let payload: Option<&dyn BlobFetcher> = match (&provider, &request.payload_from) {
        (Some(provider), _) => Some(provider),
        (None, Some(server)) => Some(server),
        (None, None) => None,
    };
    let store = store()?;
    let report = installer::update(
        &store,
        &catalog()?,
        &entry.id,
        &candidate,
        &installer::UpdateRequest {
            trust,
            origin: if request.from.is_some() {
                installer::Origin::LocalFile
            } else {
                installer::Origin::Registry
            },
            allow_downgrade: request.allow_downgrade,
            ignore_missing_runtime: request.runtime.ignore_missing_runtime,
            dry_run: request.dry_run,
        },
        payload,
    );
    drop(staged);
    let report = report?;
    print_report(&report);
    Ok(report.outcome)
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

/// Uninstall a package, deciding about its user data separately.
///
/// The payload is the publisher's, reproducible by reinstalling; the per-workspace data is the
/// user's and does not come back, so the two are separate decisions and data is never deleted
/// silently.
fn uninstall(pkg_id: &str, purge: bool, keep_state: bool, dry_run: bool) -> Result<(), Error> {
    // Only the user's copy can go; removing it puts the image's back.
    let layers = layers(pkg_id)?;
    let Some(entry) = layers.user.clone() else {
        return Err(match layers.image {
            Some(_) => Error::ImageApp(pkg_id.into()),
            None => Error::NotInstalled(pkg_id.into()),
        });
    };
    let data_home = data_home_root()?;
    let workspaces = package_store::state::find(&data_home, pkg_id);

    if dry_run {
        println!("plan: uninstall {} {}", entry.id, entry.version);
        if let Some(image) = &layers.image {
            println!("  then runs : {} from the system image", image.version);
        }
        println!("  payload   : remove {}", entry.payload_root.display());
        println!("  registry  : drop the entry");
        if entry.cli_commands.is_empty() {
            println!("  shims     : none");
        } else {
            println!(
                "  shims     : remove {} from {}",
                entry.cli_commands.join(", "),
                bin_root()?.display()
            );
        }
        println!(
            "  content   : evict the blobs no other package uses from {}",
            store()?.layout().content().display()
        );
        print_state_summary(&workspaces);
        println!(
            "  state     : {}",
            if purge {
                "DELETE (--purge)"
            } else if keep_state {
                "keep (--keep-state)"
            } else {
                "you would be asked"
            }
        );
        println!("(nothing was removed)");
        return Ok(());
    }

    // Ask about user data before removing the package, so "no" leaves things as they were.
    let delete_state = decide_state(&workspaces, purge, keep_state);

    let removed = store()?.remove(pkg_id, true).map_err(store_error)?;
    // Undo PATH shims, best effort: a missing `bin/` just means none were installed.
    if !entry.cli_commands.is_empty() {
        let bin_dir = bin_root()?;
        shim::remove_shims(&bin_dir, &entry.id, &entry.cli_commands)
            .map_err(|e| Error::Shim(e.to_string()))?;
        println!(
            "  removed {} PATH shim(s) from {}",
            entry.cli_commands.len(),
            bin_dir.display()
        );
    }
    println!("uninstalled: {pkg_id} {}", entry.version);
    if let Some(image) = &layers.image {
        println!(
            "  now runs : {} {} from the system image",
            image.id, image.version
        );
    }
    println!(
        "  content  : evicted {} blob(s) no other package uses",
        removed.evicted_blobs
    );

    if delete_state {
        let (removed, failures) = package_store::state::remove(&workspaces);
        println!("  state    : deleted in {removed} workspace(s)");
        for (path, e) in failures {
            eprintln!("tilo-install: could not remove {}: {e}", path.display());
        }
    } else if !workspaces.is_empty() {
        println!(
            "  state    : kept — {} workspace(s), {} under {}",
            workspaces.len(),
            package_store::state::human_bytes(package_store::state::total_bytes(&workspaces)),
            data_home
                .join(package_store::state::WORKSPACES_DIR)
                .display()
        );
        println!("             reinstalling picks it back up; `--purge` deletes it.");
    }
    Ok(())
}

/// Replay the signature chain for one installed package, or all of them.
fn verify_installed(pkg_id: Option<&str>) -> Result<(), Error> {
    let targets: Vec<InstalledPackage> = match pkg_id {
        Some(id) => vec![find_installed(id)?],
        None => list_catalog()?,
    };
    if targets.is_empty() {
        println!("(no packages installed)");
        return Ok(());
    }
    let checked = targets.len();
    let mut bad: usize = 0;
    for entry in &targets {
        match package_store::integrity::verify(entry, &entry.payload_root) {
            Ok(report) if report.is_intact() => {
                println!(
                    "ok       {} {}  ({} file(s) checked)",
                    entry.id, entry.version, report.files_checked
                );
                for extra in &report.unexpected {
                    println!("         note: not part of the package: {extra}");
                }
            }
            Ok(report) => {
                bad = bad.saturating_add(1);
                println!(
                    "FAILED   {} {}  ({} of {} file(s) do not match)",
                    entry.id,
                    entry.version,
                    report.faults.len(),
                    report.files_checked
                );
                for fault in &report.faults {
                    println!("         {fault}");
                }
            }
            Err(e) => {
                bad = bad.saturating_add(1);
                println!("FAILED   {} {}  {e}", entry.id, entry.version);
            }
        }
        if !entry.runtime.is_empty() {
            let version = entry.runtime.split_once('/').map_or("", |(_, v)| v);
            if !runtime_bundle_present(version) {
                println!(
                    "         note: declared runtime {} is not installed, so this \
                     package cannot launch",
                    entry.runtime
                );
            }
        }
    }
    if bad > 0 {
        return Err(Error::VerificationFailed {
            failed: bad,
            checked,
        });
    }
    Ok(())
}

/// Flip the pin: pinned packages are kept, ranked first, and exempt from eviction under size
/// pressure.
fn set_pinned(pkg_id: &str, pinned: bool) -> Result<(), Error> {
    store()?.set_pinned(pkg_id, pinned).map_err(store_error)?;
    println!("{}: {pkg_id}", if pinned { "pinned" } else { "unpinned" });
    Ok(())
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
    // `--require-attestor` is held against the recorded `attested_by`.
    let attestors = load_trusted_attestors()?;
    let revocation = load_revocation_list()?;
    if let Ok(pk_bytes) = decode_hex_32(&entry.publisher_key_hex)
        && let Some(rev) = revocation.find(&pk_bytes)
    {
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
        // `trusted-attestors.toml` changes, so check it is still trusted.
        let still_trusted = attestors.get(&entry.attested_by).is_some();
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
    if let Err(error) = store()?.mark_used(pkg_id) {
        eprintln!("tilo-install: could not record the launch: {error}");
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
    let err = cmd.exec();
    Err(Error::Io(err))
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

fn print_state_summary(workspaces: &[package_store::state::WorkspaceState]) {
    if workspaces.is_empty() {
        println!("  saved data: none");
        return;
    }
    println!(
        "  saved data: {} workspace(s), {} total",
        workspaces.len(),
        package_store::state::human_bytes(package_store::state::total_bytes(workspaces))
    );
    for saved in workspaces {
        println!(
            "              {:<24} {:>10}  {} file(s)",
            saved.workspace,
            package_store::state::human_bytes(saved.bytes),
            saved.files
        );
    }
}

/// Whether to delete the app's saved data.
///
/// Flags win; otherwise the user is asked, and with no terminal to ask on the answer is keep:
/// silent, unattended data deletion is the one failure that cannot be undone.
fn decide_state(
    workspaces: &[package_store::state::WorkspaceState],
    purge: bool,
    keep_state: bool,
) -> bool {
    if workspaces.is_empty() {
        return false;
    }
    if purge {
        return true;
    }
    if keep_state {
        return false;
    }
    print_state_summary(workspaces);
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

fn runtime_install(
    version: &str,
    from_host: bool,
    from: Option<&Path>,
    arch: Option<&str>,
    identity: Option<&Path>,
    force: bool,
) -> Result<(), Error> {
    let dest = runtime_bundle::default_bundle_path(version)
        .map_err(|e| Error::RuntimeBundle(e.to_string()))?;
    if dest.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME).exists() && !force {
        return Err(Error::RuntimeBundleExists(version.to_string()));
    }
    let bundles = runtime_bundles()?;
    let _collection_held_off = bundles.install_lock()?;
    if let Some(replaced) = bundles.retire(version)? {
        bundles.purge(&replaced, &package_store::gc::in_use_now())?;
    }
    // Built on this device on purpose, from no package: kept until removed.
    bundles.pin(version)?;
    fs::create_dir_all(&dest)?;
    let host_root: Option<&Path> = if let Some(p) = from {
        Some(p)
    } else if from_host {
        Some(Path::new("/"))
    } else {
        None
    };
    let manifest = runtime_bundle::build_bundle_with(
        &dest,
        version,
        &runtime_bundle::BuildOptions {
            from: host_root,
            arch,
            identity,
            extra_libs: &[],
            extra_bins: &[],
        },
    )
    .map_err(|e| Error::RuntimeBundle(e.to_string()))?;
    println!("installed runtime bundle: tilo-runtime/{version}");
    println!("  path     : {}", dest.display());
    println!("  arch     : {}", manifest.arch);
    println!("  files    : {}", manifest.files.len());
    println!("  root-hash: blake3:{}", manifest.root_hash);
    if let Some(pk) = &manifest.publisher_hex {
        println!("  publisher: ed25519:{pk}");
    }
    let mut report = installer::Report::default();
    installer::seal_runtime_bundle(&dest, &manifest, &mut report);
    print_report(&report);
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

fn runtime_bundles() -> Result<runtime_bundle::gc::BundleRoot, Error> {
    runtime_bundle::gc::BundleRoot::from_env().map_err(|e| Error::RuntimeBundle(e.to_string()))
}

fn runtime_remove(version: &str) -> Result<(), Error> {
    let bundles = runtime_bundles()?;
    let _collection_held_off = bundles.install_lock()?;
    bundles.unpin(version)?;
    // Out of the version's directory in one rename first: a removal stopped halfway would otherwise
    // leave a bundle a launch picks.
    let trashed = bundles
        .retire(version)?
        .ok_or_else(|| Error::RuntimeBundleNotInstalled(version.to_string()))?;
    if bundles.purge(&trashed, &package_store::gc::in_use_now())? {
        println!("removed runtime bundle: tilo-runtime/{version}");
    } else {
        println!(
            "removed runtime bundle: tilo-runtime/{version}; a running app still has it \
             mounted, so its files go once that app closes (`tilo-install gc`)"
        );
    }
    Ok(())
}

fn runtime_pin(version: &str, pinned: bool) -> Result<(), Error> {
    let bundles = runtime_bundles()?;
    if !bundles
        .bundle(version)
        .join(runtime_bundle::BUNDLE_MANIFEST_FILENAME)
        .exists()
    {
        return Err(Error::RuntimeBundleNotInstalled(version.to_string()));
    }
    if pinned {
        bundles.pin(version)?;
        println!("pinned runtime bundle: tilo-runtime/{version}");
    } else {
        bundles.unpin(version)?;
        println!("unpinned runtime bundle: tilo-runtime/{version}");
    }
    Ok(())
}

fn gc_request(dry_run: bool, grace_secs: u64, max_store_bytes: u64) -> package_store::gc::Request {
    package_store::gc::Request {
        max_store_bytes,
        grace_secs,
        now: tilo_clock::unix_now(),
        dry_run,
        keep: Vec::new(),
    }
}

fn collect_garbage(
    request: package_store::gc::Request,
) -> Result<package_store::gc::Report, Error> {
    let store = store()?;
    let catalog = package_store::Catalog::at(
        store.layout().root(),
        package_store::SystemStore::from_env(),
    );
    package_store::gc::collect(&store, &catalog, &runtime_bundles()?, request).map_err(Error::Gc)
}

fn gc(dry_run: bool, grace_secs: u64, max_store_bytes: u64) -> Result<(), Error> {
    let report = collect_garbage(gc_request(dry_run, grace_secs, max_store_bytes))?;
    let runtimes = &report.runtimes;
    if runtimes.busy {
        println!("runtimes: an install is running; collected by the next change");
    }
    for id in &report.evicted {
        println!("evicted  : {id} (store over {max_store_bytes} bytes)");
    }
    for path in &report.orphans.removed {
        println!("removed  : {} (named by no package)", path.display());
    }
    if report.orphans.staging_bytes > 0 {
        println!(
            "removed  : {} of staging crashed installs left",
            human_bytes(report.orphans.staging_bytes)
        );
    }
    for (version, bytes) in &runtimes.removed {
        let verb = if dry_run { "would go" } else { "removed " };
        println!("{verb} : runtime {version} ({})", human_bytes(*bytes));
    }
    for (version, why) in &runtimes.kept {
        let why = match why {
            runtime_bundle::gc::Kept::Referenced => "an installed package runs on it",
            runtime_bundle::gc::Kept::Pinned => "pinned",
            runtime_bundle::gc::Kept::Leased => "installed within the grace period",
            runtime_bundle::gc::Kept::InUse => "a running app has it mounted",
            runtime_bundle::gc::Kept::Launching => "an app launched on it holds it",
        };
        println!("kept     : runtime {version} ({why})");
    }
    let verb = if dry_run { "would free" } else { "freed" };
    println!("{verb}: {}", human_bytes(report.freed_bytes()));
    Ok(())
}

/// After an install, update or uninstall that began at `started` (Unix seconds): reclaim what it
/// left unneeded, keeping every app it wrote, which is about to be opened. Never fails the change it
/// follows, and reports on stderr so the change's own output stays as scripts read it.
fn collect_after_change(started: u64) {
    let written = store()
        .map_err(|error| error.to_string())
        .and_then(|store| package_store::gc::written_since(&store, started));
    let written = match written {
        Ok(written) => written,
        Err(error) => {
            eprintln!("tilo-install: warning: nothing reclaimed: {error}");
            return;
        }
    };
    let request = package_store::gc::Request::after_change(package_store::DEFAULT_MAX_STORE_BYTES)
        .keeping(written);
    match collect_garbage(request) {
        Ok(report) if report.freed_bytes() > 0 || !report.evicted.is_empty() => eprintln!(
            "tilo-install: reclaimed {} ({} runtime build(s), {} app(s) evicted)",
            human_bytes(report.freed_bytes()),
            report.runtimes.removed.len(),
            report.evicted.len()
        ),
        Ok(_) => {}
        Err(error) => eprintln!("tilo-install: warning: nothing reclaimed: {error}"),
    }
}

fn human_bytes(bytes: u64) -> String {
    package_store::state::human_bytes(bytes)
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

fn runtime(cmd: RuntimeCmd) -> Result<(), Error> {
    match cmd {
        RuntimeCmd::Install {
            version,
            from_host,
            from,
            arch,
            identity,
            force,
        } => runtime_install(
            &version,
            from_host,
            from.as_deref(),
            arch.as_deref(),
            identity.as_deref(),
            force,
        ),
        RuntimeCmd::List => runtime_list(),
        RuntimeCmd::Remove { version } => runtime_remove(&version),
        RuntimeCmd::Pin { version } => runtime_pin(&version, true),
        RuntimeCmd::Unpin { version } => runtime_pin(&version, false),
        RuntimeCmd::Path { version } => runtime_path(&version),
        RuntimeCmd::Verify { version } => runtime_verify(&version),
    }
}

fn main() {
    let cli = Cli::parse();
    // What changes the installed set: collection runs after it succeeds.
    let changes = matches!(
        cli.cmd,
        Cmd::Install { dry_run: false, .. }
            | Cmd::Update { dry_run: false, .. }
            | Cmd::Uninstall { dry_run: false, .. }
    );
    let started = tilo_clock::unix_now();
    let result = dispatch(cli.cmd);
    if let Err(e) = result {
        eprintln!("tilo-install: {e}");
        std::process::exit(1);
    }
    if changes {
        collect_after_change(started);
    }
}

fn dispatch(cmd: Cmd) -> Result<(), Error> {
    match cmd {
        Cmd::Install {
            tapp,
            pinned,
            force,
            preserve_held,
            trust,
            runtime,
            dry_run,
            payload_from,
        } => install(
            &tapp,
            &InstallOptions {
                pinned,
                replacement: match (force, preserve_held) {
                    (false, _) => installer::Replacement::Refuse,
                    (true, false) => installer::Replacement::Replace,
                    (true, true) => installer::Replacement::PreserveHeld,
                },
                dry_run,
                payload_from: payload_from.map(app_registry::HttpBlobs::new),
            },
            &trust,
            &runtime,
        ),
        Cmd::Update {
            pkg_id,
            from,
            version,
            allow_downgrade,
            dry_run,
            trust,
            runtime,
            payload_from,
        } => update(
            pkg_id.as_deref(),
            &UpdateRequest {
                from: from.as_deref(),
                version_req: version.as_deref(),
                allow_downgrade,
                dry_run,
                trust: &trust,
                runtime: &runtime,
                payload_from: payload_from.map(app_registry::HttpBlobs::new),
            },
        ),
        Cmd::List {
            json: true,
            runtime_paths,
        } => list_json(runtime_paths),
        Cmd::List { json: false, .. } => list(),
        Cmd::Uninstall {
            pkg_id,
            purge,
            keep_state,
            dry_run,
        } => uninstall(&pkg_id, purge, keep_state, dry_run),
        Cmd::Verify { pkg_id } => verify_installed(pkg_id.as_deref()),
        Cmd::Pin { pkg_id } => set_pinned(&pkg_id, true),
        Cmd::Unpin { pkg_id } => set_pinned(&pkg_id, false),
        Cmd::Run {
            pkg_id,
            no_services,
            minimal_libs,
            allow_missing_runtime,
            trust,
        } => run(
            &pkg_id,
            no_services,
            minimal_libs,
            allow_missing_runtime,
            &trust,
        ),
        Cmd::Inspect { pkg_id } => inspect(&pkg_id),
        Cmd::Trust { cmd } => match cmd {
            TrustCmd::Add { id, pubkey, scope } => trust_cmd::trust_add(&id, &pubkey, &scope),
            TrustCmd::List => trust_cmd::trust_list(),
            TrustCmd::Remove { id } => trust_cmd::trust_remove(&id),
        },
        Cmd::Revoke { pubkey_hex, reason } => trust_cmd::revoke(&pubkey_hex, &reason),
        Cmd::Revoked { cmd } => match cmd {
            RevokedCmd::List => trust_cmd::revoked_list(),
        },
        Cmd::Check { tapp, trust } => trust_cmd::check_verdict(&tapp, &trust),
        Cmd::Path => print_path(),
        Cmd::Gc {
            dry_run,
            grace_secs,
            max_store_bytes,
        } => gc(dry_run, grace_secs, max_store_bytes),
        Cmd::Runtime { cmd } => runtime(cmd),
        Cmd::Image { cmd } => image(cmd),
    }
}

fn parse_image_id(raw: &str) -> Result<PackageId, Error> {
    let text = if raw.starts_with("tapp:") {
        raw.to_owned()
    } else {
        format!("tapp:{raw}")
    };
    text.parse()
        .map_err(|error| Error::Parse(format!("image id {raw:?}: {error}")))
}

fn image_store() -> Result<package_store::Store, Error> {
    let mut store = package_store::Store::new(store()?);
    let system = package_store::SystemStore::from_env();
    if let Some(system) = &system {
        store = store
            .with_system(package_store::Layout::new(&system.data_dir))
            .with_system(package_store::Layout::new(&system.payload_dir));
    }
    let trust = package_store::ImageTrust::from_env(system.as_ref()).map_err(store_error)?;
    Ok(store.with_trust(trust))
}

fn image(cmd: ImageCmd) -> Result<(), Error> {
    match cmd {
        ImageCmd::Add {
            tapp,
            publisher,
            payload_from,
        } => {
            let store = image_store()?;
            let mut trust = store.trust().clone();
            if let Some(publisher) = publisher {
                trust = trust.with_publisher(
                    decode_hex_32(&publisher).map_err(Error::InvalidRequiredPublisher)?,
                );
            }
            let payload = payload_from.map(app_registry::HttpBlobs::new);
            let file = std::io::BufReader::new(fs::File::open(&tapp)?);
            let meta = store
                .user()
                .install_image(
                    file,
                    |meta| {
                        let attested_by = trust.check(meta).map_err(|error| error.to_string())?;
                        Ok(Admission {
                            pinned: false,
                            attested_by,
                            source: tapp.display().to_string(),
                            held: false,
                        })
                    },
                    payload.as_ref().map(|fetcher| fetcher as &dyn BlobFetcher),
                )
                .map_err(store_error)?;
            println!(
                "{} {} {} {}",
                meta.id,
                meta.manifest.package.kind,
                meta.manifest.package.id,
                meta.manifest.package.version
            );
            Ok(())
        }
        ImageCmd::List => {
            for meta in image_store()?.images().map_err(store_error)? {
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
            let id = parse_image_id(&id)?;
            let registry = registry
                .or_else(|| {
                    std::env::var("TILO_REGISTRY")
                        .ok()
                        .filter(|url| !url.is_empty())
                })
                .unwrap_or_else(|| "https://apps.tilo.so".to_owned());
            let store = image_store()?.with_fetcher(app_registry::HttpBlobs::new(registry));
            let tree = package_store::materialized_tree(&store, &id)
                .map_err(|error| Error::Store(error.to_string()))?;
            println!("{}", tree.display());
            Ok(())
        }
        ImageCmd::Remove { id } => {
            let id = parse_image_id(&id)?;
            let removed = store()?.remove_image(id).map_err(store_error)?;
            eprintln!(
                "tilo-install: removed {id}; evicted {} blob(s)",
                removed.evicted_blobs
            );
            Ok(())
        }
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
