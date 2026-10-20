//! tilo-install — the user-facing CLI to install, run, and manage Tilo
//! packages (.tapp) locally, verifying the package signature and trust
//! chain before installing.
//!
//! Packages go into the device's package store — the same registry the
//! launcher, the shell and the app host read — with their payload bytes in
//! the content store and each tree materialised once from it.

use std::fs;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Args, Parser, Subcommand};

mod language_pack;
use install::shim;
use language_pack::install_language_pack;
use package_store::{Admission, InstalledPackage, PackageStore, StoreError};
use tilo_app_package::trust::{
    AttestorScope, RevocationList, RevokedKey, TrustFile, TrustedAttestor, TrustedAttestors,
    default_revoked_keys_path, default_trusted_attestors_path,
};
use tilo_app_package::{
    BlobFetcher, DiscardBlobs, PackageId, PackageMeta, Verification, verify_with_trust,
};

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
    /// keeping the user's per-room state.
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
    /// Print the PATH-shim directory and a shell-rc snippet to prepend it.
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

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("data-dir resolution failed (XDG_DATA_HOME / HOME unset?)")]
    NoDataDir,
    #[error("config-dir resolution failed (XDG_CONFIG_HOME / HOME unset?)")]
    NoConfigDir,
    #[error("tapp verification failed: {0}")]
    Verify(String),
    #[error("publisher key is revoked ({reason})")]
    Revoked { reason: String },
    #[error("policy refused: --require-attestor {required:?} but package verified as {found}")]
    AttestorMismatch { required: String, found: String },
    #[error(
        "policy refused: --require-attestor {required:?} and package is self-signed (use --allow-self-signed to override)"
    )]
    RequireAttestorWithoutAttestation { required: String },
    #[error("policy refused: --require-publisher is not a 64-character Ed25519 key: {0}")]
    InvalidRequiredPublisher(String),
    #[error(
        "policy refused: package publisher ed25519:{found} does not match --require-publisher {required}"
    )]
    PublisherMismatch { required: String, found: String },
    #[error("policy refused: --require-package-root is not a 64-character BLAKE3 hash: {0}")]
    InvalidRequiredPackageRoot(String),
    #[error(
        "policy refused: package root {found} does not match --require-package-root {required}"
    )]
    PackageRootMismatch { required: String, found: String },
    #[error("tapp parse: {0}")]
    Parse(String),
    #[error("refusing to install: {0}")]
    Policy(String),
    #[error("package {0} already installed (use --force to overwrite)")]
    AlreadyInstalled(String),
    #[error("no such package installed: {0}")]
    NotInstalled(String),
    #[error(
        "{0} ships with the system image and cannot be removed; \
         `app rollback {0}` goes back to an older version"
    )]
    ImageApp(String),
    #[error("registry read: {0}")]
    RegistryRead(String),
    #[error("trust registry: {0}")]
    Trust(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("shim install/remove: {0}")]
    Shim(String),
    #[error("collecting garbage: {0}")]
    Gc(String),
    #[error("runtime-bundle: {0}")]
    RuntimeBundle(String),
    #[error("runtime bundle tilo-runtime/{0} is already installed (use --force to overwrite)")]
    RuntimeBundleExists(String),
    #[error("runtime bundle tilo-runtime/{0} is not installed")]
    RuntimeBundleNotInstalled(String),
    #[error(
        "{package} needs runtime `{runtime}`, the exact runtime build it was linked against, \
         and this device does not have it. It would not start: its libraries would not load.\n  \
         Install the runtime first: `app install {package}` fetches it from the registry that \
         offers the app, or install its runtime package with `tilo-install install <runtime.tapp>`.\n  \
         Or install anyway, knowing it cannot launch yet:  add --ignore-missing-runtime"
    )]
    RuntimeDependencyMissing { package: String, runtime: String },
    #[error(
        "{package} pins runtime `{runtime}` and this device does not have it, so the \
         installed copy would stop working with it. Install them together: `app install \
         {package}` (or `app update`) fetches the runtime first, or install the runtime \
         package with `tilo-install install <runtime.tapp>`.\n  Or install anyway: add \
         --ignore-missing-runtime"
    )]
    OptionalRuntimeMissing { package: String, runtime: String },
    #[error(
        "{package} {version} needs system interfaces this Tilo does not provide: {unmet}.\n  \
         It was built for another version of Tilo. Update the system first, or keep the \
         version you have."
    )]
    InterfacesUnmet {
        package: String,
        version: String,
        unmet: String,
    },
    #[error("manifest declares runtime `{0}`, which is not in `<name>/<version>` form")]
    RuntimeReferenceMalformed(String),
    #[error(
        "{package} is installed at {installed}; {candidate} is not newer (use --allow-downgrade to install it anyway)"
    )]
    NotNewer {
        package: String,
        installed: String,
        candidate: String,
    },
    #[error("the .tapp is for {found}, but {wanted} was asked for")]
    WrongPackage { wanted: String, found: String },
    #[error(
        "local update for {package} changed publisher from ed25519:{installed} to ed25519:{candidate}; \
         require the new identity explicitly with --require-publisher {candidate}"
    )]
    LocalUpdatePublisherChanged {
        package: String,
        installed: String,
        candidate: String,
    },
    #[error("fetching {app_id} from the configured registries: {detail}")]
    Fetch { app_id: String, detail: String },
    #[error("starting the async runtime for a registry fetch: {0}")]
    Runtime(#[source] tilo_rt::Error),
    #[error("package store: {0}")]
    Store(String),
    #[error(
        "{failed} of {checked} installed package(s) no longer match what was installed. \
         Reinstall them, or investigate — the payload has changed since it was verified."
    )]
    VerificationFailed { failed: usize, checked: usize },
}

/// The package store `tilo-install` works on: `TILO_APPS_DATA_DIR` when set
/// (image builds, tests), else `$XDG_DATA_HOME/tilo-apps` — the store the
/// launcher, the shell and the app host share.
fn data_root() -> Result<PathBuf, Error> {
    package_store::layout::default_user_root().ok_or(Error::NoDataDir)
}

fn store() -> Result<PackageStore, Error> {
    Ok(PackageStore::at(data_root()?))
}

/// The user's data home (parent of `tilo-rooms/`), distinct from
/// [`data_root`]: this follows `XDG_DATA_HOME` like the launcher does,
/// not `TILO_APPS_DATA_DIR` — using the wrong one silently loses "remove data".
fn data_home_root() -> Result<PathBuf, Error> {
    tilo_paths::env_dir("XDG_DATA_HOME")
        .or_else(tilo_paths::data_dir)
        .ok_or(Error::NoDataDir)
}

fn trusted_attestors_path() -> Result<PathBuf, Error> {
    default_trusted_attestors_path().ok_or(Error::NoConfigDir)
}

fn revoked_keys_path() -> Result<PathBuf, Error> {
    default_revoked_keys_path().ok_or(Error::NoConfigDir)
}

fn load_trusted_attestors() -> Result<TrustedAttestors, Error> {
    let path = trusted_attestors_path()?;
    TrustedAttestors::load(&path).map_err(|e| Error::Trust(e.to_string()))
}

fn load_revocation_list() -> Result<RevocationList, Error> {
    let path = revoked_keys_path()?;
    RevocationList::load(&path).map_err(|e| Error::Trust(e.to_string()))
}

fn bin_root() -> Result<PathBuf, Error> {
    Ok(store()?.layout().bin())
}

/// The installed packages: the system image's apps and the user's store,
/// one row per app for the copy that runs.
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

/// The copy of `pkg_id` that runs, whichever store holds it.
fn find_installed(pkg_id: &str) -> Result<InstalledPackage, Error> {
    layers(pkg_id)?
        .running()
        .cloned()
        .ok_or_else(|| Error::NotInstalled(pkg_id.into()))
}

/// Whether a user copy of `version` must be held in front of the image's
/// copy of the same app: it is older, so only an explicit choice (a
/// rollback, a forced install) put it there.
fn held_below_image(layers: &package_store::Layers, version: &str) -> bool {
    layers.image.as_ref().is_some_and(|image| {
        tilo_app_package::version::compare(version, &image.version) == std::cmp::Ordering::Less
    })
}

fn store_error(error: StoreError) -> Error {
    match error {
        StoreError::NotInstalled(id) => Error::NotInstalled(id),
        other => Error::Store(other.to_string()),
    }
}

/// attestor policy. Returns the publisher key set for `verify_with_trust`.
fn validate_registry_constraints(
    pkg_id: PackageId,
    meta: &PackageMeta,
    trust: &TrustFlags,
) -> Result<Vec<[u8; 32]>, Error> {
    if let Some(required) = trust.require_package_root.as_deref() {
        let decoded =
            hex::decode(required).map_err(|e| Error::InvalidRequiredPackageRoot(e.to_string()))?;
        if decoded.len() != 32 {
            return Err(Error::InvalidRequiredPackageRoot(format!(
                "decoded to {} bytes",
                decoded.len()
            )));
        }
        let found = hex::encode(pkg_id.as_bytes());
        if !required.eq_ignore_ascii_case(&found) {
            return Err(Error::PackageRootMismatch {
                required: required.to_ascii_lowercase(),
                found,
            });
        }
    }
    let Some(hex_key) = trust.require_publisher.as_deref() else {
        return Ok(Vec::new());
    };
    let decoded =
        hex::decode(hex_key).map_err(|e| Error::InvalidRequiredPublisher(e.to_string()))?;
    let required: [u8; 32] = decoded.try_into().map_err(|bytes: Vec<u8>| {
        Error::InvalidRequiredPublisher(format!("decoded to {} bytes", bytes.len()))
    })?;
    let found = meta.publisher();
    if found != required {
        return Err(Error::PublisherMismatch {
            required: hex::encode(required),
            found: hex::encode(found),
        });
    }
    Ok(vec![required])
}

/// Verify `pkg_id` against trust config under the caller's `TrustFlags`
/// policy; returns the manifest and attestor id (empty if self-signed).
fn verify_and_gate(
    meta: &PackageMeta,
    trust: &TrustFlags,
) -> Result<(tilo_app_package::Manifest, String, String), Error> {
    let attestors = load_trusted_attestors()?;
    let revocation = load_revocation_list()?;
    let required_publishers = validate_registry_constraints(meta.id, meta, trust)?;
    let outcome = verify_with_trust(
        meta,
        &required_publishers,
        attestors.as_slice(),
        &revocation,
    );
    match outcome {
        Verification::AttestedBy { attestor, manifest } => {
            if let Some(required) = &trust.require_attestor
                && required != &attestor
            {
                return Err(Error::AttestorMismatch {
                    required: required.clone(),
                    found: attestor,
                });
            }
            let pub_key_hex = hex::encode(meta.publisher());
            Ok((manifest, pub_key_hex, attestor))
        }
        Verification::UntrustedPublisher { identity, manifest } => {
            // --require-attestor set: refuse unless --allow-self-signed rescues it.
            if let Some(required) = &trust.require_attestor {
                if !trust.allow_self_signed {
                    return Err(Error::RequireAttestorWithoutAttestation {
                        required: required.clone(),
                    });
                }
                // fallback: --allow-self-signed permits this
                eprintln!(
                    "tilo-install: --require-attestor {required:?} unmatched; --allow-self-signed permits self-signed publisher ed25519:{}",
                    hex::encode(identity)
                );
            } else if !trust.allow_self_signed {
                // Default (current) behavior: warn and proceed.
                eprintln!(
                    "tilo-install: WARN — package self-signed by ed25519:{} (no trusted-attestor signature); use --require-attestor <id> to demand an attestation",
                    hex::encode(identity)
                );
            }
            Ok((manifest, hex::encode(identity), String::new()))
        }
        Verification::Revoked { reason, pubkey, .. } => {
            eprintln!(
                "tilo-install: REFUSED — publisher ed25519:{} is revoked: {reason}",
                hex::encode(pubkey)
            );
            Err(Error::Revoked { reason })
        }
        Verification::Trusted { manifest } => {
            let pub_key_hex = hex::encode(meta.publisher());
            if let Some(required) = &trust.require_attestor {
                // Trusted-key path bypasses the attestor chain; --require-attestor is
                // strict, so refuse here regardless.
                return Err(Error::AttestorMismatch {
                    required: required.clone(),
                    found: "trusted-key".into(),
                });
            }
            Ok((manifest, pub_key_hex, String::new()))
        }
        other => Err(Error::Verify(format!("{other:?}"))),
    }
}

/// Where a candidate package's bytes are.
enum Candidate {
    File(PathBuf),
    Fetched(Vec<u8>),
}

impl Candidate {
    fn reader(&self) -> Result<Box<dyn Read + Send + '_>, Error> {
        Ok(match self {
            Self::File(path) => Box::new(std::io::BufReader::new(fs::File::open(path)?)),
            Self::Fetched(bytes) => Box::new(bytes.as_slice()),
        })
    }

    fn len(&self) -> Result<u64, Error> {
        Ok(match self {
            Self::File(path) => fs::metadata(path)?.len(),
            Self::Fetched(bytes) => u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        })
    }
}

/// Everything a verified `.tapp` yields, before anything is stored.
/// Checking is kept separate from installing so `--dry-run` is truthful
/// and `update` can decide before touching a working install.
struct Checked {
    meta: PackageMeta,
    manifest: tilo_app_package::Manifest,
    publisher_key_hex: String,
    attested_by: String,
}

impl Checked {
    fn id(&self) -> &str {
        &self.manifest.package.id
    }
    fn version(&self) -> &str {
        &self.manifest.package.version
    }
    /// `tilo-runtime/<v>`, when the manifest pins one.
    fn runtime_ref(&self) -> Option<&str> {
        tilo_app_package::desktop::runtime_dependency(&self.manifest)
    }
    fn cli_commands(&self) -> Vec<String> {
        self.manifest
            .cli
            .as_ref()
            .map(|c| c.commands.clone())
            .unwrap_or_default()
    }
}

/// Read, verify and gate a `.tapp`, storing nothing.
///
/// Signature verification is not optional and no flag turns it off;
/// `--allow-self-signed` decides *whose* signature is acceptable, never
/// whether one is required. Every payload blob the archive carries is
/// checked against the signed collection on the way.
fn check(candidate: &Candidate, trust: &TrustFlags) -> Result<Checked, Error> {
    let tapp = tilo_app_package::tapp::read(candidate.reader()?, &mut DiscardBlobs)
        .map_err(|error| Error::Parse(error.to_string()))?;
    if !tapp.carried.is_empty() && !tapp.is_complete() {
        return Err(Error::Parse(format!(
            ".tapp carries part of its payload and lacks {} blob(s)",
            tapp.missing().len()
        )));
    }
    let meta = tapp.meta;
    let (manifest, publisher_key_hex, attested_by) = verify_and_gate(&meta, trust)?;

    let len = candidate.len()?;
    tilo_app_package::desktop::validate_native_host(&manifest)
        .map_err(|e| Error::Policy(e.to_string()))?;
    if let Some(limit) = tilo_app_package::app_size_limit(&manifest)
        && len > limit
    {
        return Err(Error::Policy(format!(
            "application .tapp is {len} bytes; this target's signed package contract permits at most {limit} bytes"
        )));
    }

    Ok(Checked {
        meta,
        manifest,
        publisher_key_hex,
        attested_by,
    })
}

/// Install a checked candidate into the package store. The store streams
/// the archive again; its metadata must be the package that was checked.
fn install_checked(
    store: &PackageStore,
    candidate: &Candidate,
    checked: &Checked,
    pinned: bool,
    held: bool,
    source: String,
    payload: Option<&dyn BlobFetcher>,
) -> Result<package_store::Installed, Error> {
    store
        .install(
            candidate.reader()?,
            checked_admission(checked, pinned, held, source),
            payload,
        )
        .map_err(store_error)
}

fn runtime_status(checked: &Checked) -> Result<Option<(String, bool)>, Error> {
    checked.runtime_ref().map(bundle_status).transpose()
}

/// The version `raw` (`<name>/<version>`) names, and whether its bundle is
/// installed for this user or in the image.
fn bundle_status(raw: &str) -> Result<(String, bool), Error> {
    let (_, version) = raw
        .split_once('/')
        .ok_or_else(|| Error::RuntimeReferenceMalformed(raw.to_string()))?;
    let per_user = runtime_bundle::default_bundle_path(version).ok();
    let system = runtime_bundle::system_bundle_path(version);
    let present = per_user
        .iter()
        .cloned()
        .chain([system])
        .any(|p| p.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME).exists());
    Ok((version.to_string(), present))
}

/// The optional runtimes `checked` pins that are not installed.
fn missing_optional_runtimes(checked: &Checked) -> Result<Vec<String>, Error> {
    let mut missing = Vec::new();
    for raw in tilo_app_package::desktop::optional_runtime_dependencies(&checked.manifest) {
        if !bundle_status(raw)?.1 {
            missing.push(raw.clone());
        }
    }
    Ok(missing)
}

fn checked_admission(
    checked: &Checked,
    pinned: bool,
    held: bool,
    source: String,
) -> impl FnOnce(&tilo_app_package::PackageMeta) -> Result<Admission, String> + Send {
    let expected = checked.meta.id;
    let attested_by = checked.attested_by.clone();
    move |meta| {
        if meta.id != expected {
            return Err(format!(
                "the package changed while it was installed: {} instead of {expected}",
                meta.id
            ));
        }
        Ok(Admission {
            pinned,
            attested_by,
            source,
            held,
        })
    }
}

fn print_cohort_selection(row: &InstalledPackage, preserved: bool) {
    let receipt = serde_json::json!({
        "id": row.id, "version": row.version, "root": row.package_id_hex,
        "runtime": row.runtime, "pinned": row.pinned, "held": row.held,
        "preserved": preserved,
    });
    println!("cohort-selection: {receipt}");
}

/// Refuse a package that requires a system interface version this OS does
/// not serve: installed, it could not work, and it would sit in front of the
/// image's copy that does.
fn fits_os(checked: &Checked) -> Result<(), Error> {
    let provided = package_store::interfaces::system_interfaces();
    let unmet = package_store::interfaces::unmet(provided.as_ref(), &checked.manifest);
    if unmet.is_empty() {
        return Ok(());
    }
    Err(Error::InterfacesUnmet {
        package: checked.id().to_string(),
        version: checked.version().to_string(),
        unmet: tilo_app_package::interfaces::describe_unmet(&unmet),
    })
}

/// Make the declared runtime available, or refuse the install.
///
/// The refusal is the point: catching a missing `tilo-runtime/<v>`
/// bundle here (with a fix command) beats discovering
/// `RuntimeBundleMissing` at first launch.
fn satisfy_runtime(checked: &Checked, flags: &RuntimeFlags) -> Result<(), Error> {
    // The runtimes an app pins by exact version travel with it, whichever
    // path installs it: an app replaced while its engine is absent would
    // open to "not installed" until someone fetched the engine by hand.
    if !flags.ignore_missing_runtime
        && let Some(runtime) = missing_optional_runtimes(checked)?.into_iter().next()
    {
        return Err(Error::OptionalRuntimeMissing {
            package: checked.id().to_string(),
            runtime,
        });
    }
    let Some((version, present)) = runtime_status(checked)? else {
        return Ok(());
    };
    if present {
        return Ok(());
    }
    if flags.ignore_missing_runtime {
        eprintln!(
            "tilo-install: WARN — {} declares runtime `tilo-runtime/{version}`, which is \
             not installed. The package will not launch until it is.",
            checked.id()
        );
        return Ok(());
    }
    Err(Error::RuntimeDependencyMissing {
        package: checked.id().to_string(),
        runtime: checked.runtime_ref().unwrap_or("tilo-runtime").to_string(),
    })
}

/// Write the PATH shims a `cli` package publishes; returns the command
/// names now on PATH for it.
fn write_cli_shims(
    manifest: &tilo_app_package::Manifest,
    id_field: &str,
) -> Result<Vec<String>, Error> {
    let cli_commands: Vec<String> = manifest
        .cli
        .as_ref()
        .map(|c| c.commands.clone())
        .unwrap_or_default();
    if cli_commands.is_empty() {
        return Ok(cli_commands);
    }
    let bin_dir = bin_root()?;
    let shim_source = shim::locate_shim_source();
    if !shim_source.exists() {
        eprintln!(
            "tilo-install: WARNING — cannot find tilo-cli-shim at {}. \
             Skipping shim install. Set TILO_CLI_SHIM_PATH or install the Tilo runtime first.",
            shim_source.display()
        );
        return Ok(cli_commands);
    }
    let launcher_override_owned = std::env::var_os("TILO_LAUNCHER_PATH").map(PathBuf::from);
    let launcher_override = launcher_override_owned.as_deref();
    let spec = shim::ShimInstall {
        package_id: id_field,
        commands: &cli_commands,
        shim_source: &shim_source,
        bin_dir: &bin_dir,
        launcher_override,
    };
    shim::write_shims(&spec).map_err(|e| Error::Shim(e.to_string()))?;
    println!(
        "  shims    : wrote {} shim(s) into {}",
        cli_commands.len(),
        bin_dir.display()
    );
    println!(
        "  PATH     : prepend {} to your shell's PATH to pick them up (see `tilo-install path`).",
        bin_dir.display()
    );
    Ok(cli_commands)
}

/// Replace the PATH shims of an installed package with the new version's.
fn replace_cli_shims(
    previous: Option<&InstalledPackage>,
    manifest: &tilo_app_package::Manifest,
    id_field: &str,
) -> Result<Vec<String>, Error> {
    if let Some(old) = previous
        && !old.cli_commands.is_empty()
    {
        let _ = shim::remove_shims(&bin_root()?, &old.id, &old.cli_commands);
    }
    write_cli_shims(manifest, id_field)
}

/// How `install` treats a package beyond trust and runtime policy.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Replacement {
    Refuse,
    Replace,
    PreserveHeld,
}

struct InstallOptions {
    pinned: bool,
    replacement: Replacement,
    dry_run: bool,
    /// Where payload blobs the archive does not carry come from.
    payload_from: Option<app_registry::HttpBlobs>,
}

fn install(
    tapp: &Path,
    options: InstallOptions,
    trust: &TrustFlags,
    runtime: &RuntimeFlags,
) -> Result<(), Error> {
    let InstallOptions {
        pinned,
        replacement,
        dry_run,
        payload_from,
    } = options;
    let force = replacement != Replacement::Refuse;
    let preserve_held = replacement == Replacement::PreserveHeld;
    // 1. Read + verify, before the registry check, so a --dry-run plan
    //    describes a package we've actually seen and checked.
    let candidate = Candidate::File(tapp.to_path_buf());
    let checked = check(&candidate, trust)?;
    if checked.manifest.package.kind == tilo_app_package::Kind::Runtime {
        return install_runtime_package(
            &candidate,
            &checked,
            RuntimeInstallOptions {
                replacement,
                pinned,
                dry_run,
            },
            payload_from
                .as_ref()
                .map(|source| source as &dyn BlobFetcher),
        );
    }
    if checked.manifest.package.kind == tilo_app_package::Kind::LanguagePack {
        return install_language_pack(
            &store()?,
            &candidate,
            &checked,
            dry_run,
            payload_from
                .as_ref()
                .map(|source| source as &dyn BlobFetcher),
        );
    }

    // 2. Registry check: an app the system image ships is installed too.
    let store = store()?;
    let layers = layers(checked.id())?;
    let existing = layers.running().cloned();
    if existing.is_some() && !force {
        return Err(Error::AlreadyInstalled(checked.id().to_string()));
    }

    if dry_run {
        print_install_plan(&checked, existing.as_ref(), &store, pinned, tapp)?;
        return Ok(());
    }

    // 3. Dependencies, before anything is stored: a half-succeeded install
    //    that then refuses is worse than one that refuses upfront.
    fits_os(&checked)?;
    satisfy_runtime(&checked, runtime)?;

    // 4. Into the content store, materialised once; the registry row is the
    //    commit point.
    let payload = payload_from
        .as_ref()
        .map(|source| source as &dyn BlobFetcher);
    let held = held_below_image(&layers, checked.version());
    let installed = if preserve_held {
        let selected = store
            .install_preserving_hold(
                candidate.reader()?,
                checked_admission(&checked, pinned, held, tapp.display().to_string()),
                payload,
            )
            .map_err(store_error)?;
        match selected {
            package_store::InstallSelection::Installed(installed) => *installed,
            package_store::InstallSelection::Preserved(row) => {
                print_cohort_selection(&row, true);
                return Ok(());
            }
        }
    } else {
        install_checked(
            &store,
            &candidate,
            &checked,
            pinned,
            held,
            tapp.display().to_string(),
            payload,
        )?
    };
    replace_cli_shims(existing.as_ref(), &checked.manifest, checked.id())?;
    if preserve_held {
        print_cohort_selection(&installed.package, false);
    }

    println!("installed: {} {}", checked.id(), checked.version());
    println!("  payload  : {}", installed.package.payload_root.display());
    println!(
        "  stored   : {} blob(s), {} already in the content store",
        installed.blobs, installed.blobs_already_stored
    );
    println!("  pinned   : {}", installed.package.pinned);
    if !checked.attested_by.is_empty() {
        println!("  attested : {}", checked.attested_by);
    }
    if let Some(rt) = checked.runtime_ref() {
        println!("  runtime  : {rt}");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    register_search_providers();
    println!("run with:  tilo-install run {}", checked.id());
    Ok(())
}

/// Where the launcher reads the runtime publishers it accepts; the
/// launcher's developer override names another list, and the installer
/// follows it so both judge a bundle alike.
fn trusted_runtime_publishers(
    bundle: &Path,
) -> Result<runtime_bundle::component_pins::BundleTrust, Error> {
    let path = std::env::var_os("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || PathBuf::from(runtime_bundle::TRUSTED_PUBLISHERS_FILE),
            PathBuf::from,
        );
    runtime_bundle::component_pins::load_for_bundle(&path, bundle)
        .map_err(|error| Error::RuntimeBundle(error.to_string()))
}

/// Stores the blobs a runtime package carries, or a registry delivers, as
/// files named by hash, streaming: a runtime's libraries are hundreds of
/// megabytes.
struct BlobDir(PathBuf);

impl tilo_app_package::BlobSink for BlobDir {
    fn accept(
        &mut self,
        hash: &tilo_app_package::BlobHash,
        len: u64,
        bytes: &mut dyn Read,
    ) -> std::io::Result<()> {
        let path = self.0.join(hash.to_hex());
        let mut file = fs::File::create(&path)?;
        std::io::copy(&mut bytes.take(len), &mut file)?;
        Ok(())
    }
}

impl tilo_app_package::BlobSource for BlobDir {
    fn open_blob(
        &self,
        hash: &tilo_app_package::BlobHash,
    ) -> std::io::Result<Option<tilo_app_package::blobs::OpenBlob<'_>>> {
        let path = self.0.join(hash.to_hex());
        match fs::File::open(&path) {
            Ok(file) => Ok(Some(tilo_app_package::blobs::OpenBlob {
                len: file.metadata()?.len(),
                reader: Box::new(std::io::BufReader::new(file)),
            })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl BlobFetcher for BlobDir {
    fn fetch(
        &self,
        hashes: &[tilo_app_package::BlobHash],
        sink: &mut dyn tilo_app_package::BlobSink,
    ) -> std::io::Result<()> {
        for hash in hashes {
            let mut file = fs::File::open(self.0.join(hash.to_hex()))?;
            sink.accept(hash, file.metadata()?.len(), &mut file)?;
        }
        Ok(())
    }
}

/// Install a native runtime package: its payload is a signed runtime
/// bundle, which lands where the launcher looks for the runtime it
/// provides (`$XDG_DATA_HOME/tilo-runtime/<version>`). The bundle must
/// verify as the build the package names, for this machine, signed by a
/// publisher this device trusts — the same checks the launcher makes before
/// mounting it — so an install never leaves a runtime the launcher refuses.
///
/// Each install (or finding it installed) leases the bundle, so collection
/// keeps it through the app install that follows; `pinned` keeps it for
/// good. A replaced bundle leaves the version's directory in one rename
/// before it is deleted, and stays while a running app has it mounted.
#[derive(Clone, Copy)]
struct RuntimeInstallOptions {
    replacement: Replacement,
    pinned: bool,
    dry_run: bool,
}

fn install_runtime_package(
    candidate: &Candidate,
    checked: &Checked,
    options: RuntimeInstallOptions,
    payload: Option<&dyn BlobFetcher>,
) -> Result<(), Error> {
    let provides = checked
        .manifest
        .package
        .provides
        .as_deref()
        .ok_or_else(|| {
            Error::Policy(format!(
                "{} is a runtime package that names no `package.provides`",
                checked.id()
            ))
        })?;
    let (_, version) = provides
        .split_once('/')
        .ok_or_else(|| Error::RuntimeReferenceMalformed(provides.to_string()))?;
    let dest = runtime_bundle::default_bundle_path(version)
        .map_err(|e| Error::RuntimeBundle(e.to_string()))?;
    if options.dry_run {
        println!("plan: install runtime {provides}");
        println!("  package   : {} {}", checked.id(), checked.version());
        println!("  bundle    : {}", dest.display());
        println!("(nothing was written)");
        return Ok(());
    }
    let store = store()?;
    let layers = layers(checked.id())?;
    let existing = layers.running();
    let already_installed =
        existing.is_some_and(|row| row.package_id_hex == checked.meta.id.to_hex());
    fits_os(checked)?;
    let root = dest
        .parent()
        .ok_or_else(|| Error::RuntimeBundle(format!("{} has no parent", dest.display())))?;
    fs::create_dir_all(root)?;
    let bundles = runtime_bundle::gc::BundleRoot::new(root);
    let _collection_held_off = bundles.install_lock()?;
    let keep = |bundles: &runtime_bundle::gc::BundleRoot| -> Result<(), Error> {
        bundles.lease(version, tilo_clock::unix_now())?;
        if options.pinned {
            bundles.pin(version)?;
        }
        Ok(())
    };
    let staging = root.join(format!(
        "{}{}",
        runtime_bundle::gc::STAGING_PREFIX,
        std::process::id()
    ));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let result = stage_runtime_bundle(candidate, &staging, payload).and_then(|tree| {
        let manifest = check_runtime_bundle(&tree, version)?;
        seal_runtime_bundle(&tree, &manifest);
        if dest.exists()
            && existing.is_some()
            && !already_installed
            && options.replacement == Replacement::Refuse
        {
            return Err(Error::AlreadyInstalled(checked.id().to_string()));
        }
        // Publish the verified signed package through the same catalogue and
        // integrity records AppHost reads. Reuse the already fetched blobs;
        // a thin engine must not download its entire payload a second time.
        let blobs = BlobDir(staging.join("blobs"));
        let preserve_held = options.replacement == Replacement::PreserveHeld;
        if !record_package(
            &store,
            candidate,
            checked,
            options.pinned,
            preserve_held,
            &blobs,
        )? {
            return Ok(false);
        }
        let replaced = bundles.retire(version)?;
        fs::rename(&tree, &dest)?;
        if let Some(replaced) = replaced {
            bundles.purge(&replaced, &install::gc::in_use_now())?;
        }
        Ok(true)
    });
    let _ = fs::remove_dir_all(&staging);
    if !result? {
        return Ok(());
    }
    keep(&bundles)?;
    if already_installed {
        println!("runtime {provides} is already installed; its verified package record is current");
    } else {
        println!("installed: runtime {provides}");
    }
    println!("  package  : {} {}", checked.id(), checked.version());
    println!("  bundle   : {}", dest.display());
    Ok(())
}

/// The normal package-store commit retains pins and can preserve an explicit
/// version hold. No runtime directory is promoted if that hold wins.
fn record_package(
    store: &PackageStore,
    candidate: &Candidate,
    checked: &Checked,
    pinned: bool,
    preserve_held: bool,
    blobs: &BlobDir,
) -> Result<bool, Error> {
    let admission = checked_admission(
        checked,
        pinned,
        held_below_image(&layers(checked.id())?, checked.version()),
        candidate_source(candidate),
    );
    if preserve_held {
        match store
            .install_preserving_hold(candidate.reader()?, admission, Some(blobs))
            .map_err(store_error)?
        {
            package_store::InstallSelection::Installed(_) => {}
            package_store::InstallSelection::Preserved(row) => {
                print_cohort_selection(&row, true);
                return Ok(false);
            }
        }
    } else {
        store
            .install(candidate.reader()?, admission, Some(blobs))
            .map_err(store_error)?;
    }
    Ok(true)
}

fn candidate_source(candidate: &Candidate) -> String {
    match candidate {
        Candidate::File(path) => path.display().to_string(),
        Candidate::Fetched(_) => "registry".to_string(),
    }
}

/// Materialise the package's payload tree under `staging`; returns the tree.
fn stage_runtime_bundle(
    candidate: &Candidate,
    staging: &Path,
    payload: Option<&dyn BlobFetcher>,
) -> Result<PathBuf, Error> {
    let blobs = staging.join("blobs");
    fs::create_dir_all(&blobs)?;
    let mut sink = BlobDir(blobs);
    let tapp = tilo_app_package::tapp::read(candidate.reader()?, &mut sink)
        .map_err(|error| Error::Parse(error.to_string()))?;
    // Most of a runtime build's files are in the runtime already installed:
    // copy those, each verified against the hash the signed package names,
    // and fetch only the rest.
    let (missing, reused) = install::runtime_blobs::LocalRuntimeBlobs::installed()
        .copy_into(&tapp.missing(), &sink.0)?;
    if reused > 0 {
        println!("  reused   : {reused} bytes already on this device");
    }
    if !missing.is_empty() {
        let fetcher = payload.ok_or_else(|| {
            Error::Policy(format!(
                "the runtime package does not carry {} payload blob(s); name the registry that \
                 offers it with --payload-from",
                missing.len()
            ))
        })?;
        fetcher
            .fetch(&missing, &mut sink)
            .map_err(|error| Error::Store(format!("fetching the runtime's payload: {error}")))?;
    }
    let tree = staging.join("tree");
    tilo_app_package::unpack(&tapp.meta, &sink, &tree)
        .map_err(|error| Error::Store(format!("unpacking the runtime: {error}")))?;
    Ok(tree)
}

/// Enable fs-verity on a fully verified bundle's files, so each launch
/// trusts them by their kernel-measured, publisher-signed digests instead of
/// hashing the whole runtime again. Best effort: where the filesystem has no
/// fs-verity, or the bundle predates its digests, launches hash the payload
/// as before, which is as safe and only slower.
fn seal_runtime_bundle(tree: &Path, manifest: &runtime_bundle::BundleManifest) {
    match runtime_bundle::enable_bundle_verity(tree, manifest) {
        Ok(0) => {}
        Ok(sealed) => {
            println!("  sealed   : {sealed} file(s) with fs-verity");
            // What a launch will do: a file the kernel measures differently
            // from its signed digest is hashed at every launch, so say so.
            let options = runtime_bundle::VerifyOptions::default();
            if let Ok((_, check)) = runtime_bundle::verify_bundle_verity_with(tree, &options)
                && check.hashed > 0
            {
                println!(
                    "  warning  : {} file(s) do not measure their signed fs-verity digest; each \
                     launch hashes them",
                    check.hashed
                );
            }
        }
        Err(error) => {
            println!("  sealed   : no ({error}); each launch hashes the runtime instead");
        }
    }
}

/// The checks the launcher makes before it mounts a per-user bundle.
fn check_runtime_bundle(
    tree: &Path,
    version: &str,
) -> Result<runtime_bundle::BundleManifest, Error> {
    let manifest = runtime_bundle::verify_bundle_with(
        tree,
        &runtime_bundle::VerifyOptions {
            require_publisher: None,
            require_signature: true,
            require_arch: Some(runtime_bundle::host_arch().to_string()),
        },
    )
    .map_err(|e| Error::RuntimeBundle(e.to_string()))?;
    if manifest.version != version {
        return Err(Error::RuntimeBundle(format!(
            "the package says it provides {version}, but its bundle is {}",
            manifest.version
        )));
    }
    trusted_runtime_publishers(tree)?
        .check(&manifest)
        .map_err(|error| Error::RuntimeBundle(error.to_string()))?;
    Ok(manifest)
}

/// Let GNOME Shell and `KRunner` search the resources the runtime beside this
/// binary admits. Best effort: an app installs and runs without it.
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

/// What an install would change, printed instead of doing it.
fn print_install_plan(
    checked: &Checked,
    existing: Option<&InstalledPackage>,
    store: &PackageStore,
    pinned: bool,
    from: &Path,
) -> Result<(), Error> {
    println!("plan: install {} {}", checked.id(), checked.version());
    println!("  from      : {}", from.display());
    println!("  signature : verified ({})", describe_trust(checked));
    match existing {
        Some(e) => println!(
            "  replaces  : {} {} at {}",
            e.id,
            e.version,
            e.payload_root.display()
        ),
        None => println!("  replaces  : nothing (not currently installed)"),
    }
    println!(
        "  payload   : write {}",
        store.layout().payload(checked.meta.id).display()
    );
    println!("  pinned    : {pinned}");
    match runtime_status(checked)? {
        None => println!("  runtime   : none declared"),
        Some((v, true)) => println!("  runtime   : tilo-runtime/{v} (installed)"),
        Some((v, false)) => println!(
            "  runtime   : tilo-runtime/{v} MISSING — install would refuse without \
             --ignore-missing-runtime"
        ),
    }
    let commands = checked.cli_commands();
    if commands.is_empty() {
        println!("  shims     : none");
    } else {
        println!(
            "  shims     : {} in {}",
            commands.join(", "),
            bin_root()?.display()
        );
    }
    println!("  state     : untouched (a package's files are not its user's data)");
    println!("(nothing was written)");
    Ok(())
}

fn describe_trust(checked: &Checked) -> String {
    if checked.attested_by.is_empty() {
        format!("self-signed by ed25519:{}", checked.publisher_key_hex)
    } else {
        format!("attested by {}", checked.attested_by)
    }
}

/// Fetch a newer version and swap it in, keeping the user's state.
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
            UpdateOutcome::Swapped => updated = updated.saturating_add(1),
            UpdateOutcome::AlreadyCurrent => current = current.saturating_add(1),
            UpdateOutcome::Planned => {}
        }
    }
    if targets.len() > 1 {
        println!("{updated} updated, {current} already current");
    }
    Ok(())
}

/// Remove the user's copies of apps the system image now ships a newer
/// version of: nothing runs them any more.
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

enum UpdateOutcome {
    Swapped,
    AlreadyCurrent,
    Planned,
}

/// The flags an update runs under.
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

/// Whether the candidate may replace the installed package, or why not.
fn update_allowed(
    entry: &InstalledPackage,
    checked: &Checked,
    request: &UpdateRequest<'_>,
) -> Result<Option<UpdateOutcome>, Error> {
    if checked.id() != entry.id {
        return Err(Error::WrongPackage {
            wanted: entry.id.clone(),
            found: checked.id().to_string(),
        });
    }
    // A local file has no signed registry index authorising a publisher-key
    // rotation. Keep the identity recorded at install time unless the user
    // explicitly pins the candidate key on this command line. Registry-backed
    // updates have their own owner-signed publisher authorisation chain in
    // `resolve_and_fetch`, so legitimate rotations remain possible there.
    if request.from.is_some()
        && !entry.publisher_key_hex.is_empty()
        && !entry
            .publisher_key_hex
            .eq_ignore_ascii_case(&checked.publisher_key_hex)
        && request
            .trust
            .require_publisher
            .as_deref()
            .is_none_or(|required| !required.eq_ignore_ascii_case(&checked.publisher_key_hex))
    {
        return Err(Error::LocalUpdatePublisherChanged {
            package: entry.id.clone(),
            installed: entry.publisher_key_hex.clone(),
            candidate: checked.publisher_key_hex.clone(),
        });
    }

    // A package recorded at another package format was installed before the
    // format changed: any build the store offers now replaces it.
    if !package_store::integrity::record_current(entry) {
        return Ok(None);
    }
    // Is it actually newer? Non-semver versions are compared as strings
    // for equality only — not evidence of an upgrade.
    match compare_versions(&entry.version, checked.version()) {
        Some(std::cmp::Ordering::Equal) | None if entry.version == checked.version() => {
            if checked.manifest.package.kind == tilo_app_package::Kind::Runtime {
                verify_current_runtime(entry, checked)?;
            }
            println!("{} {} is already current", entry.id, entry.version);
            Ok(Some(UpdateOutcome::AlreadyCurrent))
        }
        Some(std::cmp::Ordering::Greater) if !request.allow_downgrade => Err(Error::NotNewer {
            package: entry.id.clone(),
            installed: entry.version.clone(),
            candidate: checked.version().to_string(),
        }),
        _ => Ok(None),
    }
}

/// A version string and a retained directory cannot establish that an exact
/// runtime package remains installed. Replay its signed package record and
/// compare the mounted bundle's signed manifest with that package's payload.
fn verify_current_runtime(entry: &InstalledPackage, checked: &Checked) -> Result<(), Error> {
    if entry.package_id_hex != checked.meta.id.to_hex() {
        return Err(Error::Policy(
            "the offered runtime changes the installed version's signed root".into(),
        ));
    }
    let report = package_store::integrity::verify(entry, &entry.payload_root)
        .map_err(|error| Error::Store(error.to_string()))?;
    if !report.is_intact() {
        return Err(Error::RuntimeBundle(
            "the installed runtime package payload is damaged".into(),
        ));
    }
    let version = checked
        .manifest
        .package
        .provides
        .as_deref()
        .and_then(|runtime| runtime.split_once('/').map(|(_, version)| version))
        .ok_or_else(|| {
            Error::RuntimeBundle("the runtime package names no valid provided runtime".into())
        })?;
    let bundle = runtime_bundle::default_bundle_path(version)
        .map_err(|error| Error::RuntimeBundle(error.to_string()))?;
    check_runtime_bundle(&bundle, version)?;
    let manifest = runtime_bundle::BUNDLE_MANIFEST_FILENAME;
    if fs::read(bundle.join(manifest))? != fs::read(entry.payload_root.join(manifest))? {
        return Err(Error::RuntimeBundle(
            "the installed bundle differs from its signed package".into(),
        ));
    }
    Ok(())
}

/// Update a package that is not an app: a language pack or a runtime, each
/// put where its readers look. `None` for an app.
fn update_shared(
    entry: &InstalledPackage,
    candidate: &Candidate,
    checked: &Checked,
    request: &UpdateRequest<'_>,
    payload: Option<&dyn BlobFetcher>,
) -> Result<Option<UpdateOutcome>, Error> {
    if checked.manifest.package.kind == tilo_app_package::Kind::LanguagePack {
        install_language_pack(&store()?, candidate, checked, request.dry_run, payload)?;
        return Ok(Some(if request.dry_run {
            UpdateOutcome::Planned
        } else {
            UpdateOutcome::Swapped
        }));
    }
    if checked.manifest.package.kind == tilo_app_package::Kind::Runtime {
        install_runtime_package(
            candidate,
            checked,
            RuntimeInstallOptions {
                replacement: Replacement::Replace,
                pinned: entry.pinned,
                dry_run: request.dry_run,
            },
            payload,
        )?;
        return Ok(Some(if request.dry_run {
            UpdateOutcome::Planned
        } else {
            UpdateOutcome::Swapped
        }));
    }
    Ok(None)
}

fn update_one(
    entry: &InstalledPackage,
    request: &UpdateRequest<'_>,
) -> Result<UpdateOutcome, Error> {
    // 1. Get candidate bytes (local file or configured registries); either
    //    way `check` verifies them before anything is stored.
    // A registry serves thin packages; their payload comes from the node that
    // served them, and only the blobs the store lacks are fetched.
    let (candidate, source, provider) = if let Some(path) = request.from {
        (
            Candidate::File(path.to_path_buf()),
            path.display().to_string(),
            None,
        )
    } else {
        let (bytes, source, provider) = fetch_from_registries(&entry.id, request.version_req)?;
        (Candidate::Fetched(bytes), source, Some(provider))
    };
    let payload: Option<&dyn BlobFetcher> = match (&provider, &request.payload_from) {
        (Some(provider), _) => Some(provider),
        (None, Some(server)) => Some(server),
        (None, None) => None,
    };
    let checked = check(&candidate, request.trust)?;
    if let Some(outcome) = update_allowed(entry, &checked, request)? {
        return Ok(outcome);
    }

    if let Some(outcome) = update_shared(entry, &candidate, &checked, request, payload)? {
        return Ok(outcome);
    }

    let data_home = data_home_root()?;
    let rooms = install::state::find(&data_home, &entry.id);
    if request.dry_run {
        println!(
            "plan: update {} {} -> {}",
            entry.id,
            entry.version,
            checked.version()
        );
        println!("  from      : {source}");
        println!("  signature : verified ({})", describe_trust(&checked));
        if entry.system {
            println!(
                "  payload   : install beside the system image's {}",
                entry.payload_root.display()
            );
        } else {
            println!("  payload   : replace {}", entry.payload_root.display());
        }
        match runtime_status(&checked)? {
            None => println!("  runtime   : none declared"),
            Some((v, true)) => println!("  runtime   : tilo-runtime/{v} (installed)"),
            Some((v, false)) => println!("  runtime   : tilo-runtime/{v} MISSING"),
        }
        println!(
            "  state     : KEPT — {} room(s), {}",
            rooms.len(),
            install::state::human_bytes(install::state::total_bytes(&rooms))
        );
        println!("(nothing was written)");
        return Ok(UpdateOutcome::Planned);
    }

    fits_os(&checked)?;
    satisfy_runtime(&checked, request.runtime)?;

    // 2. The new version is materialised beside the old one and the registry
    //    row replaced atomically: a failure before that point leaves the old
    //    version installed and working, and the old tree goes only after.
    let store = store()?;
    let held = held_below_image(&layers(&entry.id)?, checked.version());
    let installed = install_checked(
        &store,
        &candidate,
        &checked,
        entry.pinned,
        held,
        source.clone(),
        payload,
    )?;
    replace_cli_shims(Some(entry), &checked.manifest, &entry.id)?;

    println!(
        "updated: {} {} -> {}",
        entry.id,
        entry.version,
        checked.version()
    );
    println!("  from     : {source}");
    println!("  payload  : {}", installed.package.payload_root.display());
    println!(
        "  stored   : {} blob(s), {} shared with what was installed",
        installed.blobs, installed.blobs_already_stored
    );
    println!(
        "  state    : kept ({} room(s), {})",
        rooms.len(),
        install::state::human_bytes(install::state::total_bytes(&rooms))
    );
    Ok(UpdateOutcome::Swapped)
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
        // A package whose runtime bundle has gone missing still lists, but
        // flagged — installed-but-unlaunchable, which a bare list would hide.
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

/// Which store a listed package runs from: the system image's, or the
/// user's (`held` when rolled back below the image's version).
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
/// The payload is the publisher's (reproducible by reinstalling); the
/// per-room state is the user's and does not come back — so the two
/// are separate decisions, and deleting state is never done silently.
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
    let rooms = install::state::find(&data_home, pkg_id);

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
        print_state_summary(&rooms);
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
    let delete_state = decide_state(&rooms, purge, keep_state);

    let removed = store()?.remove(pkg_id, true).map_err(store_error)?;
    // Undo PATH shims, best-effort — a missing bin_dir just means none were installed.
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
        let (removed, failures) = install::state::remove(&rooms);
        println!("  state    : deleted in {removed} room(s)");
        for (path, e) in failures {
            eprintln!("tilo-install: could not remove {}: {e}", path.display());
        }
    } else if !rooms.is_empty() {
        println!(
            "  state    : kept — {} room(s), {} under {}",
            rooms.len(),
            install::state::human_bytes(install::state::total_bytes(&rooms)),
            data_home.join(install::state::ROOMS_DIR).display()
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

/// Flip the pin: pinned packages are kept, ranked first, and exempt from
/// size-pressure eviction.
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

    // Re-check trust policy at launch — revocation may postdate install;
    // --require-attestor is honoured against the recorded attested-by field.
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
            reason: rev.reason.clone(),
        });
    }
    if let Some(required) = &trust.require_attestor {
        // The recorded attested_by is fixed at install time and can go stale
        // if trusted-attestors.toml changed since; re-check it's still trusted.
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

    // Find tilo-launcher on PATH; fall back to a sibling of this binary.
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
    // exec()-replace so this process's exit code IS the launcher's.
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
    let rooms = install::state::find(&data_home_root()?, pkg_id);
    if rooms.is_empty() {
        println!("saved data    : none");
    } else {
        println!(
            "saved data    : {} room(s), {}",
            rooms.len(),
            install::state::human_bytes(install::state::total_bytes(&rooms))
        );
        for room in &rooms {
            println!("                {}  {}", room.room, room.path.display());
        }
    }
    Ok(())
}

/// Compare two version strings as semver; `None` when either isn't
/// valid semver (common for hand-written manifests).
fn compare_versions(installed: &str, candidate: &str) -> Option<std::cmp::Ordering> {
    let a = semver::Version::parse(installed).ok()?;
    let b = semver::Version::parse(candidate).ok()?;
    Some(a.cmp(&b))
}

/// Resolve `app_id` through the configured registries; returns the
/// verified `.tapp` bytes plus the registry that served them.
///
/// This verifies the registry protocol (signed index, signed entry,
/// blake3 of the blob) — separately from `ingest`'s later check of the
/// package's own publisher signature against local trust config. Two
/// independent checks answering different questions.
fn fetch_from_registries(
    app_id: &str,
    version_req: Option<&str>,
) -> Result<(Vec<u8>, String, app_registry::ProviderBlobs), Error> {
    let host = app_registry::HostProfile::detect();
    let outcome = tilo_rt::block_on(app_registry::resolve::resolve_and_fetch(
        None,
        &host,
        app_id,
        version_req,
    ))
    .map_err(Error::Runtime)?
    .map_err(|e| Error::Fetch {
        app_id: app_id.to_string(),
        // `{e:#}` renders anyhow's full context chain — where the useful part
        // of a fetch failure lives (e.g. "no registry offers X" vs "index
        // signature not trusted").
        detail: format!("{e:#}"),
    })?;
    let source = format!("{} (registry {})", outcome.version, outcome.registry);
    Ok((outcome.bytes, source, outcome.payload))
}

fn runtime_bundle_present(version: &str) -> bool {
    if version.is_empty() {
        return false;
    }
    runtime_bundle::default_bundle_path(version)
        .ok()
        .into_iter()
        .chain([runtime_bundle::system_bundle_path(version)])
        .any(|p| p.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME).exists())
}

fn print_state_summary(rooms: &[install::state::RoomState]) {
    if rooms.is_empty() {
        println!("  saved data: none");
        return;
    }
    println!(
        "  saved data: {} room(s), {} total",
        rooms.len(),
        install::state::human_bytes(install::state::total_bytes(rooms))
    );
    for room in rooms {
        println!(
            "              {:<24} {:>10}  {} file(s)",
            room.room,
            install::state::human_bytes(room.bytes),
            room.files
        );
    }
}

/// Whether to delete the app's saved state.
///
/// Flags win; otherwise the user is asked, and with no terminal to ask
/// on the answer defaults to *keep* — silent, unattended data deletion
/// is the one failure mode that can't be undone.
fn decide_state(rooms: &[install::state::RoomState], purge: bool, keep_state: bool) -> bool {
    if rooms.is_empty() {
        return false;
    }
    if purge {
        return true;
    }
    if keep_state {
        return false;
    }
    print_state_summary(rooms);
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

fn trust_add(id: &str, pubkey_hex: &str, scope: &str) -> Result<(), Error> {
    // Validate scope + pubkey hex up-front, before touching the file.
    let scope_parsed = AttestorScope::parse(scope).map_err(|e| Error::Trust(e.to_string()))?;
    let _ = decode_hex_32(pubkey_hex).map_err(Error::Trust)?;
    let mut attestors = load_trusted_attestors()?;
    attestors.attestor.retain(|a| a.id != id);
    attestors.attestor.push(TrustedAttestor {
        id: id.into(),
        pubkey_hex: pubkey_hex.into(),
        scope: scope_parsed,
    });
    let path = trusted_attestors_path()?;
    attestors
        .save(&path)
        .map_err(|e| Error::Trust(e.to_string()))?;
    println!("trust: added {id} (scope: {scope})");
    println!("  file: {}", path.display());
    Ok(())
}

fn trust_list() -> Result<(), Error> {
    let attestors = load_trusted_attestors()?;
    if attestors.attestor.is_empty() {
        println!("(no trusted attestors)");
        return Ok(());
    }
    println!("{:<24}  {:<12}  pubkey", "id", "scope");
    for a in &attestors.attestor {
        println!("{:<24}  {:<12}  {}", a.id, a.scope.render(), a.pubkey_hex);
    }
    Ok(())
}

fn trust_remove(id: &str) -> Result<(), Error> {
    let mut attestors = load_trusted_attestors()?;
    let before = attestors.attestor.len();
    attestors.attestor.retain(|a| a.id != id);
    if attestors.attestor.len() == before {
        return Err(Error::Trust(format!("no trusted attestor with id {id:?}")));
    }
    let path = trusted_attestors_path()?;
    attestors
        .save(&path)
        .map_err(|e| Error::Trust(e.to_string()))?;
    println!("trust: removed {id}");
    Ok(())
}

fn revoke(pubkey_hex: &str, reason: &str) -> Result<(), Error> {
    let _ = decode_hex_32(pubkey_hex).map_err(Error::Trust)?;
    let mut list = load_revocation_list()?;
    list.revoked.retain(|k| k.pubkey_hex != pubkey_hex);
    list.revoked.push(RevokedKey {
        pubkey_hex: pubkey_hex.into(),
        reason: reason.into(),
    });
    let path = revoked_keys_path()?;
    list.save(&path).map_err(|e| Error::Trust(e.to_string()))?;
    println!("revoked: ed25519:{pubkey_hex}");
    if !reason.is_empty() {
        println!("  reason: {reason}");
    }
    println!("  file: {}", path.display());
    Ok(())
}

fn revoked_list() -> Result<(), Error> {
    let list = load_revocation_list()?;
    if list.revoked.is_empty() {
        println!("(no revoked keys)");
        return Ok(());
    }
    println!("{:<66}  reason", "pubkey");
    for k in &list.revoked {
        println!("{:<66}  {}", k.pubkey_hex, k.reason);
    }
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
        bundles.purge(&replaced, &install::gc::in_use_now())?;
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
    seal_runtime_bundle(&dest, &manifest);
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

/// The per-user runtime bundles, where `runtime_bundle::default_bundle_path`
/// puts them.
fn runtime_bundles() -> Result<runtime_bundle::gc::BundleRoot, Error> {
    runtime_bundle::gc::BundleRoot::from_env().map_err(|e| Error::RuntimeBundle(e.to_string()))
}

fn runtime_remove(version: &str) -> Result<(), Error> {
    let bundles = runtime_bundles()?;
    let _collection_held_off = bundles.install_lock()?;
    bundles.unpin(version)?;
    // Out of the version's directory in one rename first: a removal stopped
    // halfway never leaves a bundle a launch would pick.
    let trashed = bundles
        .retire(version)?
        .ok_or_else(|| Error::RuntimeBundleNotInstalled(version.to_string()))?;
    if bundles.purge(&trashed, &install::gc::in_use_now())? {
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

/// What `gc` and every change run.
fn gc_request(dry_run: bool, grace_secs: u64, max_store_bytes: u64) -> install::gc::Request {
    install::gc::Request {
        max_store_bytes,
        grace_secs,
        now: tilo_clock::unix_now(),
        dry_run,
    }
}

fn collect_garbage(request: install::gc::Request) -> Result<install::gc::Report, Error> {
    let store = store()?;
    let catalog = package_store::Catalog::at(
        store.layout().root(),
        package_store::SystemStore::from_env(),
    );
    install::gc::collect(&store, &catalog, &runtime_bundles()?, request).map_err(Error::Gc)
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

/// After an install, update or uninstall: reclaim what it left unneeded.
/// Never fails the change it follows; says what it freed on stderr, so the
/// change's own output stays as scripts read it.
fn collect_after_change() {
    let request = install::gc::Request::after_change(package_store::DEFAULT_MAX_STORE_BYTES);
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
    install::state::human_bytes(bytes)
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
    // An install holds collection off while it runs, so the runtime it
    // checked for cannot be collected under it. Without a bundle root there
    // is nothing to collect, and an app with no runtime creates none; a
    // runtime install takes the lock itself.
    let collection_held_off = matches!(cli.cmd, Cmd::Install { .. } | Cmd::Update { .. })
        .then(|| {
            let bundles = runtime_bundles().ok()?;
            bundles
                .path()
                .is_dir()
                .then(|| bundles.install_lock().ok())?
        })
        .flatten();
    let result = dispatch(cli.cmd);
    drop(collection_held_off);
    if let Err(e) = result {
        eprintln!("tilo-install: {e}");
        std::process::exit(1);
    }
    if changes {
        collect_after_change();
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
            InstallOptions {
                pinned,
                replacement: match (force, preserve_held) {
                    (false, _) => Replacement::Refuse,
                    (true, false) => Replacement::Replace,
                    (true, true) => Replacement::PreserveHeld,
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
            TrustCmd::Add { id, pubkey, scope } => trust_add(&id, &pubkey, &scope),
            TrustCmd::List => trust_list(),
            TrustCmd::Remove { id } => trust_remove(&id),
        },
        Cmd::Revoke { pubkey_hex, reason } => revoke(&pubkey_hex, &reason),
        Cmd::Revoked { cmd } => match cmd {
            RevokedCmd::List => revoked_list(),
        },
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

fn decode_hex_32(s: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(s).map_err(|e| e.to_string())?;
    if bytes.len() != 32 {
        return Err(format!("expected 32 bytes, got {}", bytes.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
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
    // Prefer a sibling binary — matches how the demo installs both under target/release/.
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

#[cfg(test)]
mod installer_tests {
    #[test]
    fn local_install_uses_the_shared_thin_app_limit() {
        assert!(tilo_app_package::tapp_size_allowed(
            tilo_app_package::Kind::App,
            tilo_app_package::MAX_APP_TAPP_BYTES
        ));
        assert!(!tilo_app_package::tapp_size_allowed(
            tilo_app_package::Kind::App,
            tilo_app_package::MAX_APP_TAPP_BYTES + 1
        ));
    }
}
