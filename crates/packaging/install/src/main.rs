//! tilo-install — the user-facing CLI to install, run, and manage Tilo
//! packages (.tapp) locally, verifying the package signature and trust
//! chain before installing.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Args, Parser, Subcommand};
use install::shim;
use serde::{Deserialize, Serialize};
use tilo_app_package::blobs::BlobStore;
use tilo_app_package::trust::{
    AttestorScope, RevocationList, RevokedKey, TrustedAttestor, TrustedAttestors,
    default_revoked_keys_path, default_trusted_attestors_path,
};
use tilo_app_package::{MemoryBlobStore, PackageId, Verification, unpack, verify_with_trust};

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
        #[command(flatten)]
        trust: TrustFlags,
        #[command(flatten)]
        runtime: RuntimeFlags,
        /// Print exactly what would change and exit, touching nothing.
        #[arg(long)]
        dry_run: bool,
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
    },
    /// List installed packages.
    List,
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
        /// Turn off the Landlock enforcement. Debug only.
        #[arg(long, hide = true)]
        no_landlock: bool,
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
    /// Manage installed Tilo runtime bundles.
    Runtime {
        #[command(subcommand)]
        cmd: RuntimeCmd,
    },
}

/// What to do when a package's declared runtime bundle isn't installed.
/// Checked at install time (not first launch) so `RuntimeBundleMissing`
/// is caught while the user is still looking, rather than as a mystery later.
#[derive(Debug, Args, Clone, Default)]
struct RuntimeFlags {
    /// Build and install the manifest's declared runtime bundle from the
    /// current host root, if missing.
    #[arg(long)]
    with_runtime: bool,
    /// Sign the bundle built by --with-runtime; unsigned bundles are
    /// refused by the launcher unless told otherwise.
    #[arg(long, requires = "with_runtime")]
    runtime_identity: Option<PathBuf>,
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
    /// Remove an installed runtime bundle by version tag.
    Remove { version: String },
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
    #[error("registry read: {0}")]
    RegistryRead(String),
    #[error("registry write: {0}")]
    RegistryWrite(String),
    #[error("trust registry: {0}")]
    Trust(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("unpack: {0}")]
    Unpack(String),
    #[error("shim install/remove: {0}")]
    Shim(String),
    #[error("runtime-bundle: {0}")]
    RuntimeBundle(String),
    #[error("runtime bundle tilo-runtime/{0} is already installed (use --force to overwrite)")]
    RuntimeBundleExists(String),
    #[error("runtime bundle tilo-runtime/{0} is not installed")]
    RuntimeBundleNotInstalled(String),
    #[error(
        "{package} declares runtime `{runtime}`, and no bundle for it is installed. \
         That package will not launch: the launcher refuses to start an app whose \
         declared runtime is absent.\n  \
         Install it and retry:  tilo-install runtime install --version {version} --from-host --identity <key.json>\n  \
         Or build it as part of this install:  add --with-runtime\n  \
         Or install anyway, knowing it cannot launch yet:  add --ignore-missing-runtime"
    )]
    RuntimeDependencyMissing {
        package: String,
        runtime: String,
        version: String,
    },
    #[error("manifest declares runtime `{0}`, which is not in `<name>/<version>` form")]
    RuntimeReferenceMalformed(String),
    #[error("integrity: {0}")]
    Integrity(#[from] install::integrity::IntegrityError),
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
    Runtime(#[source] std::io::Error),
    #[error("payload swap for {package}: {detail}")]
    Swap { package: String, detail: String },
    #[error(
        "{failed} of {checked} installed package(s) no longer match what was installed. \
         Reinstall them, or investigate — the payload has changed since it was verified."
    )]
    VerificationFailed { failed: usize, checked: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryEntry {
    /// Reverse-DNS id from manifest [package].id.
    id: String,
    /// Semver from manifest.
    version: String,
    /// Content-addressed root hash (hex, no `tapp:` prefix).
    package_id_hex: String,
    /// Absolute path to the payload directory.
    payload_root: PathBuf,
    /// Absolute path to the extracted manifest.
    manifest_path: PathBuf,
    /// Publisher public key hex.
    publisher_key_hex: String,
    /// Pinned = never GC'd (when GC lands).
    pinned: bool,
    /// Unix seconds of install.
    installed_at: u64,
    /// Attestor id if verified via an attestor chain; empty for self-signed.
    #[serde(default)]
    attested_by: String,
    /// PATH command names this package published (so `remove` can un-write
    /// them without re-reading the manifest); empty if no [cli] block.
    #[serde(default)]
    cli_commands: Vec<String>,
    /// The manifest's declared `runtime = "tilo-runtime/<v>"`, if any — lets
    /// `list`/`verify` report bundle status without re-reading manifests.
    #[serde(default)]
    runtime: String,
    /// Where the bytes came from (local path or registry name) —
    /// provenance, not a trust input (the signature is).
    #[serde(default)]
    source: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    entry: Vec<RegistryEntry>,
}

fn data_root() -> Result<PathBuf, Error> {
    // Honour TILO_APPS_DATA_DIR (tests/dev installs); matches tilo-launcher::registry.
    if let Some(dir) = std::env::var_os("TILO_APPS_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let base = dirs::data_dir().ok_or(Error::NoDataDir)?;
    Ok(base.join("tilo-apps"))
}

fn registry_path() -> Result<PathBuf, Error> {
    Ok(data_root()?.join("registry.toml"))
}

fn payloads_root() -> Result<PathBuf, Error> {
    Ok(data_root()?.join("payloads"))
}

/// The user's data home (parent of `tilo-rooms/`), distinct from
/// [`data_root`]: this follows `XDG_DATA_HOME` like the launcher does,
/// not `TILO_APPS_DATA_DIR` — using the wrong one silently loses "remove data".
fn data_home_root() -> Result<PathBuf, Error> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME")
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    dirs::data_dir().ok_or(Error::NoDataDir)
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
    Ok(data_root()?.join("bin"))
}

fn load_registry() -> Result<Registry, Error> {
    let p = registry_path()?;
    if !p.exists() {
        return Ok(Registry::default());
    }
    let bytes = fs::read(&p).map_err(|e| Error::RegistryRead(e.to_string()))?;
    let text = String::from_utf8(bytes).map_err(|e| Error::RegistryRead(e.to_string()))?;
    toml::from_str(&text).map_err(|e| Error::RegistryRead(e.to_string()))
}

fn save_registry(reg: &Registry) -> Result<(), Error> {
    let p = registry_path()?;
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = toml::to_string(reg).map_err(|e| Error::RegistryWrite(e.to_string()))?;
    atomic_file::write(&p, text.as_bytes())?;
    Ok(())
}

/// Validate registry-pinned identity before applying the user's broader
/// attestor policy. Returns the publisher key set for `verify_with_trust`.
fn validate_registry_constraints(
    pkg_id: PackageId,
    store: &MemoryBlobStore,
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
        let found = hex_of(pkg_id.as_bytes());
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
    let found = extract_publisher_key(&pkg_id, store)
        .ok_or_else(|| Error::Verify("signed root has no publisher key".to_string()))?;
    if found != required {
        return Err(Error::PublisherMismatch {
            required: hex_of(&required),
            found: hex_of(&found),
        });
    }
    Ok(vec![required])
}

/// Verify `pkg_id` against trust config under the caller's `TrustFlags`
/// policy; returns the manifest and attestor id (empty if self-signed).
fn verify_and_gate(
    pkg_id: PackageId,
    store: &MemoryBlobStore,
    trust: &TrustFlags,
) -> Result<(tilo_app_package::Manifest, String, String), Error> {
    let attestors = load_trusted_attestors()?;
    let revocation = load_revocation_list()?;
    let required_publishers = validate_registry_constraints(pkg_id, store, trust)?;
    let outcome = verify_with_trust(
        pkg_id,
        store,
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
            let pub_key_hex = extract_publisher_key(&pkg_id, store)
                .map(|k| hex_of(&k))
                .unwrap_or_default();
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
                    hex_of(&identity)
                );
            } else if !trust.allow_self_signed {
                // Default (current) behavior: warn and proceed.
                eprintln!(
                    "tilo-install: WARN — package self-signed by ed25519:{} (no trusted-attestor signature); use --require-attestor <id> to demand an attestation",
                    hex_of(&identity)
                );
            }
            Ok((manifest, hex_of(&identity), String::new()))
        }
        Verification::Revoked { reason, pubkey, .. } => {
            eprintln!(
                "tilo-install: REFUSED — publisher ed25519:{} is revoked: {reason}",
                hex_of(&pubkey)
            );
            Err(Error::Revoked { reason })
        }
        Verification::Trusted { manifest } => {
            let pub_key_hex = extract_publisher_key(&pkg_id, store)
                .map(|k| hex_of(&k))
                .unwrap_or_default();
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

/// Everything a verified `.tapp` yields, before any file is written.
/// Reading/verifying is kept separate from placing files so `--dry-run`
/// is truthful and `update` can decide on a swap before touching a
/// working install.
struct Ingested {
    pkg_id: PackageId,
    store: MemoryBlobStore,
    manifest: tilo_app_package::Manifest,
    publisher_key_hex: String,
    attested_by: String,
    /// The three blobs the publisher's signature covers, taken from the
    /// just-verified store, so the integrity record matches exactly what was checked.
    signed_root: Vec<u8>,
    manifest_bytes: Vec<u8>,
    collection_bytes: Vec<u8>,
}

impl Ingested {
    fn id(&self) -> &str {
        &self.manifest.package.id
    }
    fn version(&self) -> &str {
        &self.manifest.package.version
    }
    /// `tilo-runtime/<v>`, when the manifest pins one.
    fn runtime_ref(&self) -> Option<&str> {
        self.manifest.package.runtime.as_deref()
    }
}

/// Read, verify and gate a `.tapp`'s bytes, before any file is written.
///
/// Signature verification is not optional and no flag turns it off;
/// `--allow-self-signed` decides *whose* signature is acceptable, never
/// whether one is required.
fn ingest(bytes: &[u8], trust: &TrustFlags) -> Result<Ingested, Error> {
    let app_registry::tapp::TappMeta {
        package_id: pkg_id,
        store,
        ..
    } = app_registry::tapp::load(bytes).map_err(|error| Error::Parse(error.to_string()))?;
    let (manifest, publisher_key_hex, attested_by) = verify_and_gate(pkg_id, &store, trust)?;

    if !tilo_app_package::tapp_size_allowed(
        manifest.package.kind,
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    ) {
        return Err(Error::Policy(format!(
            "application .tapp is {} bytes; the {}-byte limit keeps apps instant and moves shared dependencies into runtime bundles",
            bytes.len(),
            tilo_app_package::MAX_APP_TAPP_BYTES
        )));
    }

    // A signature says who wrote the manifest, not that its contents are
    // reasonable — a package naming its own filesystem paths is refused
    // here regardless of signature.
    tilo_app_package::validate_manifest(&manifest).map_err(|e| Error::Policy(e.to_string()))?;

    let blob = |hash: &tilo_app_package::BlobHash, what: &str| -> Result<Vec<u8>, Error> {
        store
            .get(hash)
            .map_err(|e| Error::Parse(e.to_string()))?
            .ok_or_else(|| Error::Parse(format!(".tapp is missing its {what}")))
    };
    let signed_root = blob(
        &tilo_app_package::BlobHash(*pkg_id.as_bytes()),
        "signed root",
    )?;
    let root = tilo_app_package::SignedRoot::decode(&signed_root)
        .map_err(|e| Error::Parse(format!("signed root: {e}")))?;
    let manifest_bytes = blob(&root.manifest_hash, "manifest")?;
    let collection_bytes = blob(&root.payload_root_hash, "payload collection")?;

    Ok(Ingested {
        pkg_id,
        store,
        manifest,
        publisher_key_hex,
        attested_by,
        signed_root,
        manifest_bytes,
        collection_bytes,
    })
}

/// Unpack a verified package into `dest` (replacing what's there) and
/// write its manifest alongside, so the launcher can read it without
/// reopening the .tapp.
fn place_payload(ingested: &Ingested, dest: &Path) -> Result<PathBuf, Error> {
    if dest.exists() {
        fs::remove_dir_all(dest)?;
    }
    fs::create_dir_all(dest)?;
    let manifest =
        unpack(ingested.pkg_id, &ingested.store, dest).map_err(|e| Error::Unpack(e.to_string()))?;
    let manifest_path = dest.join("manifest.toml");
    fs::write(
        &manifest_path,
        manifest
            .to_toml_string()
            .map_err(|e| Error::Unpack(e.to_string()))?,
    )?;
    Ok(manifest_path)
}

/// Where the manifest's declared runtime bundle would live, and
/// whether it's there. `Ok(None)` means no runtime declared (the
/// common case for static binaries).
fn runtime_status(ingested: &Ingested) -> Result<Option<(String, bool)>, Error> {
    let Some(raw) = ingested.runtime_ref() else {
        return Ok(None);
    };
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
    Ok(Some((version.to_string(), present)))
}

/// Make the declared runtime available, or refuse the install.
///
/// The refusal is the point: catching a missing `tilo-runtime/<v>`
/// bundle here (with a fix command) beats discovering
/// `RuntimeBundleMissing` at first launch.
fn satisfy_runtime(ingested: &Ingested, flags: &RuntimeFlags) -> Result<(), Error> {
    let Some((version, present)) = runtime_status(ingested)? else {
        return Ok(());
    };
    if present {
        return Ok(());
    }
    if flags.with_runtime {
        println!("  runtime  : tilo-runtime/{version} is missing; building it from the host root");
        return runtime_install(
            &version,
            true,
            None,
            None,
            flags.runtime_identity.as_deref(),
            false,
        );
    }
    if flags.ignore_missing_runtime {
        eprintln!(
            "tilo-install: WARN — {} declares runtime `tilo-runtime/{version}`, which is \
             not installed. The package will not launch until it is.",
            ingested.id()
        );
        return Ok(());
    }
    Err(Error::RuntimeDependencyMissing {
        package: ingested.id().to_string(),
        runtime: ingested.runtime_ref().unwrap_or("tilo-runtime").to_string(),
        version,
    })
}

/// Write the PATH shims a [cli] package publishes; returns the command
/// names recorded on the registry entry.
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

fn install(
    tapp: &Path,
    pinned: bool,
    force: bool,
    trust: &TrustFlags,
    runtime: &RuntimeFlags,
    dry_run: bool,
) -> Result<(), Error> {
    // 1. Read + verify, before the registry check, so a --dry-run plan
    //    describes a package we've actually seen and checked.
    let bytes = fs::read(tapp)?;
    let ingested = ingest(&bytes, trust)?;
    let id_field = ingested.id().to_string();
    let version_field = ingested.version().to_string();

    // 2. Registry check.
    let mut reg = load_registry()?;
    let existing = reg.entry.iter().find(|e| e.id == id_field).cloned();
    if existing.is_some() && !force {
        return Err(Error::AlreadyInstalled(id_field));
    }

    let dest = payloads_root()?.join(&id_field);
    if dry_run {
        print_install_plan(&ingested, existing.as_ref(), &dest, pinned, tapp)?;
        return Ok(());
    }

    // 3. Dependencies, before any file is written: a half-succeeded install
    //    that then refuses is worse than one that refuses upfront.
    satisfy_runtime(&ingested, runtime)?;

    reg.entry.retain(|e| e.id != id_field);

    // 4. Unpack into payloads/<id> — the reverse-DNS id (not content hash) is the install slot.
    place_payload(&ingested, &dest)?;
    let manifest_path = dest.join("manifest.toml");

    // 5. The evidence a later `tilo-install verify` replays.
    let data_root = data_root()?;
    install::integrity::record(
        &data_root,
        &id_field,
        &ingested.signed_root,
        &ingested.manifest_bytes,
        &ingested.collection_bytes,
    )?;

    let cli_commands = write_cli_shims(&ingested.manifest, &id_field)?;

    reg.entry.push(RegistryEntry {
        id: id_field.clone(),
        version: version_field.clone(),
        package_id_hex: hex_of(ingested.pkg_id.as_bytes()),
        payload_root: dest.clone(),
        manifest_path,
        publisher_key_hex: ingested.publisher_key_hex.clone(),
        pinned,
        installed_at: now(),
        attested_by: ingested.attested_by.clone(),
        cli_commands,
        runtime: ingested.runtime_ref().unwrap_or_default().to_string(),
        source: tapp.display().to_string(),
    });
    save_registry(&reg)?;

    println!("installed: {id_field} {version_field}");
    println!("  payload  : {}", dest.display());
    println!("  pinned   : {pinned}");
    if !ingested.attested_by.is_empty() {
        println!("  attested : {}", ingested.attested_by);
    }
    if let Some(rt) = ingested.runtime_ref() {
        println!("  runtime  : {rt}");
    }
    println!("run with:  tilo-install run {id_field}");
    Ok(())
}

/// What an install would change, printed instead of doing it.
fn print_install_plan(
    ingested: &Ingested,
    existing: Option<&RegistryEntry>,
    dest: &Path,
    pinned: bool,
    from: &Path,
) -> Result<(), Error> {
    println!("plan: install {} {}", ingested.id(), ingested.version());
    println!("  from      : {}", from.display());
    println!("  signature : verified ({})", describe_trust(ingested));
    match existing {
        Some(e) => println!(
            "  replaces  : {} {} at {}",
            e.id,
            e.version,
            e.payload_root.display()
        ),
        None => println!("  replaces  : nothing (not currently installed)"),
    }
    println!("  payload   : write {}", dest.display());
    println!("  pinned    : {pinned}");
    match runtime_status(ingested)? {
        None => println!("  runtime   : none declared"),
        Some((v, true)) => println!("  runtime   : tilo-runtime/{v} (installed)"),
        Some((v, false)) => println!(
            "  runtime   : tilo-runtime/{v} MISSING — install would refuse without \
             --with-runtime or --ignore-missing-runtime"
        ),
    }
    let commands: Vec<String> = ingested
        .manifest
        .cli
        .as_ref()
        .map(|c| c.commands.clone())
        .unwrap_or_default();
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

fn describe_trust(ingested: &Ingested) -> String {
    if ingested.attested_by.is_empty() {
        format!("self-signed by ed25519:{}", ingested.publisher_key_hex)
    } else {
        format!("attested by {}", ingested.attested_by)
    }
}

/// Fetch a newer version and swap it in, keeping the user's state.
fn update(
    pkg_id: Option<&str>,
    from: Option<&Path>,
    version_req: Option<&str>,
    allow_downgrade: bool,
    dry_run: bool,
    trust: &TrustFlags,
    runtime: &RuntimeFlags,
) -> Result<(), Error> {
    let reg = load_registry()?;
    let targets: Vec<RegistryEntry> = if let Some(id) = pkg_id {
        vec![
            reg.entry
                .iter()
                .find(|e| e.id == id)
                .cloned()
                .ok_or_else(|| Error::NotInstalled(id.into()))?,
        ]
    } else {
        if from.is_some() {
            return Err(Error::Policy(
                "--from names one .tapp, so it needs one package id".into(),
            ));
        }
        reg.entry
    };
    if targets.is_empty() {
        println!("(no packages installed)");
        return Ok(());
    }

    let mut updated: u32 = 0;
    let mut current: u32 = 0;
    for entry in &targets {
        match update_one(
            entry,
            from,
            version_req,
            allow_downgrade,
            dry_run,
            trust,
            runtime,
        )? {
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

enum UpdateOutcome {
    Swapped,
    AlreadyCurrent,
    Planned,
}

fn update_one(
    entry: &RegistryEntry,
    from: Option<&Path>,
    version_req: Option<&str>,
    allow_downgrade: bool,
    dry_run: bool,
    trust: &TrustFlags,
    runtime: &RuntimeFlags,
) -> Result<UpdateOutcome, Error> {
    // 1. Get candidate bytes (local file or configured registries); either
    //    way `ingest` verifies them before anything on disk moves.
    let (bytes, source) = match from {
        Some(path) => (fs::read(path)?, path.display().to_string()),
        None => fetch_from_registries(&entry.id, version_req)?,
    };
    let ingested = ingest(&bytes, trust)?;
    if ingested.id() != entry.id {
        return Err(Error::WrongPackage {
            wanted: entry.id.clone(),
            found: ingested.id().to_string(),
        });
    }
    // A local file has no signed registry index authorising a publisher-key
    // rotation. Keep the identity recorded at install time unless the user
    // explicitly pins the candidate key on this command line. Registry-backed
    // updates have their own owner-signed publisher authorisation chain in
    // `resolve_and_fetch`, so legitimate rotations remain possible there.
    if from.is_some()
        && !entry.publisher_key_hex.is_empty()
        && !entry
            .publisher_key_hex
            .eq_ignore_ascii_case(&ingested.publisher_key_hex)
        && trust
            .require_publisher
            .as_deref()
            .is_none_or(|required| !required.eq_ignore_ascii_case(&ingested.publisher_key_hex))
    {
        return Err(Error::LocalUpdatePublisherChanged {
            package: entry.id.clone(),
            installed: entry.publisher_key_hex.clone(),
            candidate: ingested.publisher_key_hex.clone(),
        });
    }

    // 2. Is it actually newer? Non-semver versions are compared as strings
    //    for equality only — not evidence of an upgrade.
    let ordering = compare_versions(&entry.version, ingested.version());
    match ordering {
        Some(std::cmp::Ordering::Equal) | None if entry.version == ingested.version() => {
            println!("{} {} is already current", entry.id, entry.version);
            return Ok(UpdateOutcome::AlreadyCurrent);
        }
        Some(std::cmp::Ordering::Greater) if !allow_downgrade => {
            return Err(Error::NotNewer {
                package: entry.id.clone(),
                installed: entry.version.clone(),
                candidate: ingested.version().to_string(),
            });
        }
        _ => {}
    }

    let data_home = data_home_root()?;
    let rooms = install::state::find(&data_home, &entry.id);
    if dry_run {
        println!(
            "plan: update {} {} -> {}",
            entry.id,
            entry.version,
            ingested.version()
        );
        println!("  from      : {source}");
        println!("  signature : verified ({})", describe_trust(&ingested));
        println!("  payload   : swap {}", entry.payload_root.display());
        match runtime_status(&ingested)? {
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

    satisfy_runtime(&ingested, runtime)?;

    let data_root = data_root()?;
    let layout = UpdateLayout::new(entry, &ingested, &data_root)?;
    let journal = UpdateJournal::begin(
        &data_root,
        UpdateIntent {
            package_id: entry.id.clone(),
            owner_pid: std::process::id(),
            new_package_id_hex: hex_of(ingested.pkg_id.as_bytes()),
            payload_live: layout.payload_live.clone(),
            payload_staging: layout.payload_staging.clone(),
            payload_retired: layout.payload_retired.clone(),
            integrity_live: layout.integrity_live.clone(),
            integrity_retired: layout.integrity_retired.clone(),
            old_commands: entry.cli_commands.clone(),
            new_commands: ingested
                .manifest
                .cli
                .as_ref()
                .map(|cli| cli.commands.clone())
                .unwrap_or_default(),
        },
    )?;

    // 3. Swap: the new payload is built beside the old one and moved into
    //    place, so a mid-failure leaves the old version working, not half-replaced.
    let payload_swap = swap_payload(&ingested, &layout)?;

    // 4. Records: integrity first, since a registry entry claiming a
    //    version whose evidence isn't written yet would fail its own `verify`.
    let integrity_swap = IntegritySwap::record(
        &data_root,
        &entry.id,
        &ingested.signed_root,
        &ingested.manifest_bytes,
        &ingested.collection_bytes,
        &layout.integrity_retired,
    )?;

    // The new version may publish a different set of commands, so the
    // old shims go before the new ones are written.
    if !entry.cli_commands.is_empty() {
        let bin_dir = bin_root()?;
        let _ = shim::remove_shims(&bin_dir, &entry.cli_commands);
    }
    let candidate_commands = ingested
        .manifest
        .cli
        .as_ref()
        .map(|cli| cli.commands.clone())
        .unwrap_or_default();
    let cli_commands = match write_cli_shims(&ingested.manifest, &entry.id) {
        Ok(commands) => commands,
        Err(error) => {
            restore_cli_shims(&entry.id, &candidate_commands, &entry.cli_commands);
            return Err(error);
        }
    };

    let mut reg = match load_registry() {
        Ok(registry) => registry,
        Err(error) => {
            restore_cli_shims(&entry.id, &cli_commands, &entry.cli_commands);
            return Err(error);
        }
    };
    let Some(slot) = reg.entry.iter_mut().find(|e| e.id == entry.id) else {
        restore_cli_shims(&entry.id, &cli_commands, &entry.cli_commands);
        return Err(Error::NotInstalled(entry.id.clone()));
    };
    slot.version = ingested.version().to_string();
    slot.package_id_hex = hex_of(ingested.pkg_id.as_bytes());
    slot.publisher_key_hex
        .clone_from(&ingested.publisher_key_hex);
    slot.attested_by.clone_from(&ingested.attested_by);
    slot.installed_at = now();
    slot.cli_commands.clone_from(&cli_commands);
    slot.runtime = ingested.runtime_ref().unwrap_or_default().to_string();
    slot.source.clone_from(&source);
    if let Err(error) = save_registry(&reg) {
        restore_cli_shims(&entry.id, &cli_commands, &entry.cli_commands);
        return Err(error);
    }

    // The atomic registry replacement is the commit point: launchers now
    // agree that the new payload and integrity evidence are current. Until
    // this line, both guards restore the previous working installation on
    // every ordinary error path.
    integrity_swap.commit();
    payload_swap.commit();
    journal.finish()?;

    println!(
        "updated: {} {} -> {}",
        entry.id,
        entry.version,
        ingested.version()
    );
    println!("  from     : {source}");
    println!("  payload  : {}", entry.payload_root.display());
    println!(
        "  state    : kept ({} room(s), {})",
        rooms.len(),
        install::state::human_bytes(install::state::total_bytes(&rooms))
    );
    Ok(UpdateOutcome::Swapped)
}

/// Build the new payload beside the live one and move it into place.
///
/// Two renames, not one: `rename(2)` won't replace a non-empty
/// directory, so the old tree moves aside first and is removed only
/// once the new one is live — failure modes are "old still installed"
/// or "new installed", never "neither".
#[derive(Debug)]
struct UpdateLayout {
    payload_live: PathBuf,
    payload_staging: PathBuf,
    payload_retired: PathBuf,
    integrity_live: PathBuf,
    integrity_retired: PathBuf,
}

impl UpdateLayout {
    fn new(entry: &RegistryEntry, ingested: &Ingested, data_root: &Path) -> Result<Self, Error> {
        let parent = entry.payload_root.parent().ok_or_else(|| Error::Swap {
            package: entry.id.clone(),
            detail: format!("{} has no parent directory", entry.payload_root.display()),
        })?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let token = format!("{}-{nonce}", std::process::id());
        let integrity_live = install::integrity::record_dir(data_root, &entry.id);
        let integrity_parent = integrity_live
            .parent()
            .ok_or_else(|| Error::Swap {
                package: entry.id.clone(),
                detail: format!("{} has no parent directory", integrity_live.display()),
            })?
            .to_path_buf();
        let integrity_leaf = integrity_live
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("package")
            .to_string();
        Ok(Self {
            payload_live: entry.payload_root.clone(),
            payload_staging: parent.join(format!(".staging-{}-{token}", ingested.id())),
            payload_retired: parent.join(format!(".retiring-{}-{token}", ingested.id())),
            integrity_live,
            integrity_retired: integrity_parent
                .join(format!(".retiring-{integrity_leaf}-integrity-{token}")),
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct UpdateIntent {
    package_id: String,
    owner_pid: u32,
    new_package_id_hex: String,
    payload_live: PathBuf,
    payload_staging: PathBuf,
    payload_retired: PathBuf,
    integrity_live: PathBuf,
    integrity_retired: PathBuf,
    old_commands: Vec<String>,
    new_commands: Vec<String>,
}

struct UpdateJournal {
    path: PathBuf,
    finished: bool,
}

impl UpdateJournal {
    fn begin(data_root: &Path, intent: UpdateIntent) -> Result<Self, Error> {
        let dir = data_root.join("update-journal");
        fs::create_dir_all(&dir)?;
        let key = hex::encode(blake3::hash(intent.package_id.as_bytes()).as_bytes());
        let path = dir.join(format!("{key}.toml"));
        let bytes = toml::to_string(&intent)
            .map(String::into_bytes)
            .map_err(|error| Error::Swap {
                package: intent.package_id.clone(),
                detail: format!("encoding update recovery journal: {error}"),
            })?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| Error::Swap {
                package: intent.package_id.clone(),
                detail: format!("creating update recovery journal: {error}"),
            })?;
        use std::io::Write as _;
        if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
            let _ = fs::remove_file(&path);
            return Err(Error::Swap {
                package: intent.package_id,
                detail: format!("persisting update recovery journal: {error}"),
            });
        }
        if let Err(error) = fs::File::open(&dir).and_then(|directory| directory.sync_all()) {
            let _ = fs::remove_file(&path);
            return Err(Error::Swap {
                package: intent.package_id,
                detail: format!("persisting update recovery journal directory: {error}"),
            });
        }
        Ok(Self {
            path,
            finished: false,
        })
    }

    fn finish(mut self) -> Result<(), Error> {
        fs::remove_file(&self.path)?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for UpdateJournal {
    fn drop(&mut self) {
        if !self.finished {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn update_path_is_scoped(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
        && !path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
}

fn recover_update_directories(
    intent: &UpdateIntent,
    committed: bool,
    payload_root: &Path,
    integrity_root: &Path,
) -> Result<(), Error> {
    let payload_paths = [
        &intent.payload_live,
        &intent.payload_staging,
        &intent.payload_retired,
    ];
    let integrity_paths = [&intent.integrity_live, &intent.integrity_retired];
    if payload_paths
        .iter()
        .any(|path| !update_path_is_scoped(path, payload_root))
        || integrity_paths
            .iter()
            .any(|path| !update_path_is_scoped(path, integrity_root))
    {
        return Err(Error::Swap {
            package: intent.package_id.clone(),
            detail: "recovery journal contains a path outside the package stores".to_string(),
        });
    }
    if committed {
        if !intent.payload_live.exists() {
            return Err(Error::Swap {
                package: intent.package_id.clone(),
                detail: "registry committed the update but its payload is missing".to_string(),
            });
        }
        let _ = fs::remove_dir_all(&intent.payload_staging);
        let _ = fs::remove_dir_all(&intent.payload_retired);
        let _ = fs::remove_dir_all(&intent.integrity_retired);
        return Ok(());
    }

    let _ = fs::remove_dir_all(&intent.payload_staging);
    if intent.payload_retired.exists() {
        let _ = fs::remove_dir_all(&intent.payload_live);
        fs::rename(&intent.payload_retired, &intent.payload_live).map_err(|error| Error::Swap {
            package: intent.package_id.clone(),
            detail: format!("restoring payload after interrupted update: {error}"),
        })?;
    }
    if intent.integrity_retired.exists() {
        let _ = fs::remove_dir_all(&intent.integrity_live);
        fs::rename(&intent.integrity_retired, &intent.integrity_live).map_err(|error| {
            Error::Swap {
                package: intent.package_id.clone(),
                detail: format!("restoring integrity evidence after interrupted update: {error}"),
            }
        })?;
    }
    Ok(())
}

/// Resolve journals left by process termination or power loss. The atomic
/// registry is the commit record: if it names the candidate content hash,
/// cleanup finishes; otherwise the retired payload and evidence are restored.
fn recover_interrupted_updates() -> Result<(), Error> {
    let data_root = data_root()?;
    let journal_dir = data_root.join("update-journal");
    if !journal_dir.exists() {
        return Ok(());
    }
    let payload_root = payloads_root()?;
    let integrity_root = data_root.join("integrity");
    let registry = load_registry()?;
    for entry in fs::read_dir(&journal_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let text = fs::read_to_string(entry.path()).map_err(|error| Error::Swap {
            package: "unknown".to_string(),
            detail: format!("reading update recovery journal: {error}"),
        })?;
        let intent: UpdateIntent = toml::from_str(&text).map_err(|error| Error::Swap {
            package: "unknown".to_string(),
            detail: format!("decoding update recovery journal: {error}"),
        })?;
        if process_is_alive(intent.owner_pid) {
            return Err(Error::Swap {
                package: intent.package_id,
                detail: format!(
                    "another installer process ({}) is updating this package",
                    intent.owner_pid
                ),
            });
        }
        let installed = registry
            .entry
            .iter()
            .find(|installed| installed.id == intent.package_id);
        let committed = installed.is_some_and(|installed| {
            installed
                .package_id_hex
                .eq_ignore_ascii_case(&intent.new_package_id_hex)
        });
        let desired_commands = installed
            .map(|installed| installed.cli_commands.as_slice())
            .unwrap_or_default();
        recover_update_directories(&intent, committed, &payload_root, &integrity_root)?;
        if committed {
            let mut all_commands = intent.old_commands.clone();
            all_commands.extend(intent.new_commands.clone());
            all_commands.sort();
            all_commands.dedup();
            restore_cli_shims(&intent.package_id, &all_commands, desired_commands);
        } else {
            let mut all_commands = intent.old_commands.clone();
            all_commands.extend(intent.new_commands.clone());
            all_commands.sort();
            all_commands.dedup();
            restore_cli_shims(&intent.package_id, &all_commands, desired_commands);
        }
        fs::remove_file(entry.path())?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn process_is_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

#[cfg(not(target_os = "linux"))]
fn process_is_alive(_pid: u32) -> bool {
    false
}

fn swap_payload(ingested: &Ingested, layout: &UpdateLayout) -> Result<DirectorySwap, Error> {
    let live = &layout.payload_live;
    let parent = live.parent().ok_or_else(|| Error::Swap {
        package: ingested.id().to_string(),
        detail: format!("{} has no parent directory", live.display()),
    })?;
    fs::create_dir_all(&parent)?;
    let staging = &layout.payload_staging;
    let retired = &layout.payload_retired;

    place_payload(ingested, &staging)?;

    let had_live = live.exists();
    if had_live {
        fs::rename(live, retired).map_err(|e| Error::Swap {
            package: ingested.id().to_string(),
            detail: format!("moving the installed payload aside: {e}"),
        })?;
    }
    if let Err(e) = fs::rename(staging, live) {
        // Restore the working install before reporting — leaving no payload at
        // all is the one outcome this exists to prevent.
        if retired.exists() {
            let _ = fs::rename(retired, live);
        }
        let _ = fs::remove_dir_all(staging);
        return Err(Error::Swap {
            package: ingested.id().to_string(),
            detail: format!("moving the new payload into place: {e}"),
        });
    }
    Ok(DirectorySwap {
        live: live.clone(),
        retired: had_live.then(|| retired.clone()),
        committed: false,
    })
}

/// A directory replacement that remains reversible until the registry—the
/// launcher's authority—has been atomically published.
struct DirectorySwap {
    live: PathBuf,
    retired: Option<PathBuf>,
    committed: bool,
}

impl DirectorySwap {
    fn commit(mut self) {
        if let Some(retired) = &self.retired {
            let _ = fs::remove_dir_all(retired);
        }
        self.committed = true;
    }

    fn rollback(&mut self) {
        if self.committed {
            return;
        }
        let _ = fs::remove_dir_all(&self.live);
        if let Some(retired) = &self.retired {
            let _ = fs::rename(retired, &self.live);
        }
        self.committed = true;
    }
}

impl Drop for DirectorySwap {
    fn drop(&mut self) {
        self.rollback();
    }
}

/// Integrity evidence participates in the same update transaction as the
/// payload. `integrity::record` replaces three files; moving the old record
/// aside first means a later shim/registry error can restore the complete old
/// chain instead of leaving old registry metadata pointed at new evidence.
struct IntegritySwap {
    directory: DirectorySwap,
}

impl IntegritySwap {
    fn record(
        data_root: &Path,
        package_id: &str,
        signed_root: &[u8],
        manifest_bytes: &[u8],
        collection_bytes: &[u8],
        retired: &Path,
    ) -> Result<Self, Error> {
        let live = install::integrity::record_dir(data_root, package_id);
        let parent = live.parent().ok_or_else(|| Error::Swap {
            package: package_id.to_string(),
            detail: format!("{} has no parent directory", live.display()),
        })?;
        fs::create_dir_all(parent)?;
        let had_live = live.exists();
        if had_live {
            fs::rename(&live, retired).map_err(|error| Error::Swap {
                package: package_id.to_string(),
                detail: format!("moving the installed integrity record aside: {error}"),
            })?;
        }
        if let Err(error) = install::integrity::record(
            data_root,
            package_id,
            signed_root,
            manifest_bytes,
            collection_bytes,
        ) {
            let _ = fs::remove_dir_all(&live);
            if had_live {
                let _ = fs::rename(retired, &live);
            }
            return Err(error.into());
        }
        Ok(Self {
            directory: DirectorySwap {
                live,
                retired: had_live.then(|| retired.to_path_buf()),
                committed: false,
            },
        })
    }

    fn commit(self) {
        self.directory.commit();
    }
}

/// Best-effort compensation for a registry commit failure. Shims contain only
/// the package id, so recreating the old command set restores their complete
/// semantics; the payload and integrity guards handle the authoritative data.
fn restore_cli_shims(package_id: &str, new: &[String], old: &[String]) {
    let Ok(bin_dir) = bin_root() else {
        return;
    };
    let _ = shim::remove_shims(&bin_dir, new);
    if old.is_empty() {
        return;
    }
    let shim_source = shim::locate_shim_source();
    if !shim_source.exists() {
        return;
    }
    let launcher_override_owned = std::env::var_os("TILO_LAUNCHER_PATH").map(PathBuf::from);
    let spec = shim::ShimInstall {
        package_id,
        commands: old,
        shim_source: &shim_source,
        bin_dir: &bin_dir,
        launcher_override: launcher_override_owned.as_deref(),
    };
    let _ = shim::write_shims(&spec);
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
) -> Result<(Vec<u8>, String), Error> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(Error::Runtime)?;
    let host = app_registry::HostProfile::detect();
    let outcome = runtime
        .block_on(app_registry::resolve::resolve_and_fetch(
            None,
            &host,
            app_id,
            version_req,
        ))
        .map_err(|e| Error::Fetch {
            app_id: app_id.to_string(),
            // `{e:#}` renders anyhow's full context chain — where the useful part
            // of a fetch failure lives (e.g. "no registry offers X" vs "index
            // signature not trusted").
            detail: format!("{e:#}"),
        })?;
    let source = format!("{} (registry {})", outcome.version, outcome.registry);
    Ok((outcome.bytes, source))
}

fn list() -> Result<(), Error> {
    let reg = load_registry()?;
    if reg.entry.is_empty() {
        println!("(no packages installed)");
        return Ok(());
    }
    println!(
        "{:<32}  {:<8}  {:<6}  {:<18}  {:<18}  installed",
        "id", "version", "pinned", "attested-by", "runtime"
    );
    for e in &reg.entry {
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
            "{:<32}  {:<8}  {:<6}  {:<18}  {:<18}  {}",
            e.id,
            e.version,
            if e.pinned { "yes" } else { "no" },
            attested,
            runtime,
            ago
        );
    }
    Ok(())
}

/// Whether a runtime bundle of this version is installed (per-user or
/// system-wide), using the launcher's lookup order.
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

/// Uninstall a package, deciding about its user data separately.
///
/// The payload is the publisher's (reproducible by reinstalling); the
/// per-room state is the user's and does not come back — so the two
/// are separate decisions, and deleting state is never done silently.
fn uninstall(pkg_id: &str, purge: bool, keep_state: bool, dry_run: bool) -> Result<(), Error> {
    let mut reg = load_registry()?;
    let idx = reg
        .entry
        .iter()
        .position(|e| e.id == pkg_id)
        .ok_or_else(|| Error::NotInstalled(pkg_id.into()))?;

    let data_home = data_home_root()?;
    let rooms = install::state::find(&data_home, pkg_id);
    let data_root = data_root()?;

    if dry_run {
        // `idx` was just found via `.position()` above and nothing has
        // touched `reg.entry` since, so this is always `Some` — `.get()`
        // avoids indexing without changing behaviour.
        let Some(entry) = reg.entry.get(idx) else {
            return Err(Error::NotInstalled(pkg_id.into()));
        };
        println!("plan: uninstall {} {}", entry.id, entry.version);
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
        if install::integrity::has_record(&data_root, pkg_id) {
            println!(
                "  integrity : remove {}",
                install::integrity::record_dir(&data_root, pkg_id).display()
            );
        }
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

    let entry = reg.entry.remove(idx);
    if entry.payload_root.exists() {
        fs::remove_dir_all(&entry.payload_root)?;
    }
    // Undo PATH shims, best-effort — a missing bin_dir just means none were installed.
    if !entry.cli_commands.is_empty() {
        let bin_dir = bin_root()?;
        shim::remove_shims(&bin_dir, &entry.cli_commands)
            .map_err(|e| Error::Shim(e.to_string()))?;
        println!(
            "  removed {} PATH shim(s) from {}",
            entry.cli_commands.len(),
            bin_dir.display()
        );
    }
    install::integrity::forget(&data_root, pkg_id);
    save_registry(&reg)?;
    println!("uninstalled: {pkg_id} {}", entry.version);

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

/// Replay the signature chain for one installed package, or all of them.
fn verify_installed(pkg_id: Option<&str>) -> Result<(), Error> {
    let reg = load_registry()?;
    let targets: Vec<&RegistryEntry> = match pkg_id {
        Some(id) => vec![
            reg.entry
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| Error::NotInstalled(id.into()))?,
        ],
        None => reg.entry.iter().collect(),
    };
    if targets.is_empty() {
        println!("(no packages installed)");
        return Ok(());
    }
    let data_root = data_root()?;
    let checked = targets.len();
    let mut bad: usize = 0;
    for entry in targets {
        match install::integrity::verify(
            &data_root,
            &entry.id,
            &entry.package_id_hex,
            &entry.payload_root,
        ) {
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
            let present = runtime_bundle::default_bundle_path(version)
                .ok()
                .into_iter()
                .chain([runtime_bundle::system_bundle_path(version)])
                .any(|p| p.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME).exists());
            if !present {
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

/// Flip the `pinned` flag: pinned packages are kept, ranked first, and
/// exempt from size-pressure GC.
fn set_pinned(pkg_id: &str, pinned: bool) -> Result<(), Error> {
    let mut reg = load_registry()?;
    let entry = reg
        .entry
        .iter_mut()
        .find(|e| e.id == pkg_id)
        .ok_or_else(|| Error::NotInstalled(pkg_id.into()))?;
    entry.pinned = pinned;
    save_registry(&reg)?;
    println!("{}: {pkg_id}", if pinned { "pinned" } else { "unpinned" });
    Ok(())
}

// One CLI flag per bool, mirroring `Cmd::Run`'s own fields one-for-one;
// grouping them into a struct would only move the list, not shorten it.
#[allow(
    clippy::fn_params_excessive_bools,
    reason = "each bool is an independent `tilo-install run` flag forwarded to the launcher; \
              see the comment above"
)]
fn run(
    pkg_id: &str,
    no_services: bool,
    minimal_libs: bool,
    allow_missing_runtime: bool,
    no_landlock: bool,
    trust: &TrustFlags,
) -> Result<(), Error> {
    let reg = load_registry()?;
    let entry = reg
        .entry
        .iter()
        .find(|e| e.id == pkg_id)
        .ok_or_else(|| Error::NotInstalled(pkg_id.into()))?;

    if let Some(required) = trust.require_package_root.as_deref() {
        decode_hex_32(required).map_err(Error::InvalidRequiredPackageRoot)?;
        if !required.eq_ignore_ascii_case(&entry.package_id_hex) {
            return Err(Error::PackageRootMismatch {
                required: required.to_ascii_lowercase(),
                found: entry.package_id_hex.clone(),
            });
        }
    }
    if let Some(required) = trust.require_publisher.as_deref() {
        decode_hex_32(required).map_err(Error::InvalidRequiredPublisher)?;
        if !required.eq_ignore_ascii_case(&entry.publisher_key_hex) {
            return Err(Error::PublisherMismatch {
                required: required.to_ascii_lowercase(),
                found: entry.publisher_key_hex.clone(),
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
                        entry.attested_by.clone()
                    },
                });
            }
            eprintln!(
                "tilo-install: --require-attestor {required:?} unmatched; --allow-self-signed permits install-time attested-by {:?}",
                entry.attested_by
            );
        }
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
    if no_landlock {
        eprintln!(
            "tilo-install: WARNING — --no-landlock disables kernel-level FS/net restrictions. Only for debugging."
        );
        // Not implemented on the CLI yet — surface as a runtime flag if / when needed.
    }
    // exec()-replace so this process's exit code IS the launcher's.
    let err = cmd.exec();
    Err(Error::Io(err))
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

fn inspect(pkg_id: &str) -> Result<(), Error> {
    let reg = load_registry()?;
    let entry = reg
        .entry
        .iter()
        .find(|e| e.id == pkg_id)
        .ok_or_else(|| Error::NotInstalled(pkg_id.into()))?;
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
        now().saturating_sub(entry.installed_at)
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
    let data_root = data_root()?;
    println!(
        "integrity     : {}",
        if install::integrity::has_record(&data_root, pkg_id) {
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
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
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
    Ok(())
}

fn runtime_list() -> Result<(), Error> {
    let base = dirs::data_dir()
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

fn runtime_remove(version: &str) -> Result<(), Error> {
    let path = runtime_bundle::default_bundle_path(version)
        .map_err(|e| Error::RuntimeBundle(e.to_string()))?;
    if !path.exists() {
        return Err(Error::RuntimeBundleNotInstalled(version.to_string()));
    }
    fs::remove_dir_all(&path)?;
    println!("removed runtime bundle: tilo-runtime/{version}");
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

fn main() {
    let cli = Cli::parse();
    let result = recover_interrupted_updates().and_then(|()| match cli.cmd {
        Cmd::Install {
            tapp,
            pinned,
            force,
            trust,
            runtime,
            dry_run,
        } => install(&tapp, pinned, force, &trust, &runtime, dry_run),
        Cmd::Update {
            pkg_id,
            from,
            version,
            allow_downgrade,
            dry_run,
            trust,
            runtime,
        } => update(
            pkg_id.as_deref(),
            from.as_deref(),
            version.as_deref(),
            allow_downgrade,
            dry_run,
            &trust,
            &runtime,
        ),
        Cmd::List => list(),
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
            no_landlock,
            trust,
        } => run(
            &pkg_id,
            no_services,
            minimal_libs,
            allow_missing_runtime,
            no_landlock,
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
        Cmd::Runtime { cmd } => match cmd {
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
            RuntimeCmd::Path { version } => runtime_path(&version),
            RuntimeCmd::Verify { version } => runtime_verify(&version),
        },
    });
    if let Err(e) = result {
        eprintln!("tilo-install: {e}");
        std::process::exit(1);
    }
}

/// Best-effort publisher key extraction: reads the signed-root blob by
/// `PackageId` and decodes the 32-byte `publisher_identity`.
fn extract_publisher_key(id: &PackageId, store: &MemoryBlobStore) -> Option<[u8; 32]> {
    let blob = store
        .get(&tilo_app_package::blobs::BlobHash(*id.as_bytes()))
        .ok()
        .flatten()?;
    let signed = tilo_app_package::signed::SignedRoot::decode(&blob).ok()?;
    Some(signed.publisher_identity)
}

fn hex_of(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for b in bytes {
        // String's `Write` impl never actually fails.
        let _ = write!(out, "{b:02x}");
    }
    out
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

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn age(installed_at: u64) -> String {
    let n = now();
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
    use super::{DirectorySwap, UpdateIntent, UpdateJournal, recover_update_directories};
    use std::fs;
    use std::path::PathBuf;

    fn recovery_intent(root: &std::path::Path) -> (UpdateIntent, PathBuf, PathBuf) {
        let payload_root = root.join("payloads");
        let integrity_root = root.join("data/integrity");
        let intent = UpdateIntent {
            package_id: "so.tilo.Test".to_string(),
            owner_pid: 0,
            new_package_id_hex: "ab".repeat(32),
            payload_live: payload_root.join("so.tilo.Test"),
            payload_staging: payload_root.join(".staging-so.tilo.Test-1"),
            payload_retired: payload_root.join(".retiring-so.tilo.Test-1"),
            integrity_live: integrity_root.join("so.tilo.Test"),
            integrity_retired: integrity_root.join(".retiring-so.tilo.Test-1"),
            old_commands: Vec::new(),
            new_commands: Vec::new(),
        };
        (intent, payload_root, integrity_root)
    }

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

    #[test]
    fn uncommitted_directory_swap_restores_the_working_payload() {
        let temp = tempfile::tempdir().expect("tempdir");
        let live = temp.path().join("live");
        let retired = temp.path().join("retired");
        fs::create_dir(&live).expect("old payload");
        fs::write(live.join("version"), "old").expect("old marker");
        fs::rename(&live, &retired).expect("retire old payload");
        fs::create_dir(&live).expect("new payload");
        fs::write(live.join("version"), "new").expect("new marker");

        drop(DirectorySwap {
            live: live.clone(),
            retired: Some(retired.clone()),
            committed: false,
        });

        assert_eq!(fs::read_to_string(live.join("version")).unwrap(), "old");
        assert!(!retired.exists());
    }

    #[test]
    fn committed_directory_swap_keeps_new_payload_and_reaps_old() {
        let temp = tempfile::tempdir().expect("tempdir");
        let live = temp.path().join("live");
        let retired = temp.path().join("retired");
        fs::create_dir(&live).expect("new payload");
        fs::write(live.join("version"), "new").expect("new marker");
        fs::create_dir(&retired).expect("old payload");
        fs::write(retired.join("version"), "old").expect("old marker");

        DirectorySwap {
            live: live.clone(),
            retired: Some(retired.clone()),
            committed: false,
        }
        .commit();

        assert_eq!(fs::read_to_string(live.join("version")).unwrap(), "new");
        assert!(!retired.exists());
    }

    #[test]
    fn restart_before_registry_commit_restores_payload_and_evidence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (intent, payload_root, integrity_root) = recovery_intent(temp.path());
        for (path, marker) in [
            (&intent.payload_live, "new"),
            (&intent.payload_retired, "old"),
            (&intent.payload_staging, "partial"),
            (&intent.integrity_live, "new-evidence"),
            (&intent.integrity_retired, "old-evidence"),
        ] {
            fs::create_dir_all(path).unwrap();
            fs::write(path.join("version"), marker).unwrap();
        }

        recover_update_directories(&intent, false, &payload_root, &integrity_root).unwrap();

        assert_eq!(
            fs::read_to_string(intent.payload_live.join("version")).unwrap(),
            "old"
        );
        assert_eq!(
            fs::read_to_string(intent.integrity_live.join("version")).unwrap(),
            "old-evidence"
        );
        assert!(!intent.payload_retired.exists());
        assert!(!intent.payload_staging.exists());
        assert!(!intent.integrity_retired.exists());
    }

    #[test]
    fn restart_after_registry_commit_finishes_cleanup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (intent, payload_root, integrity_root) = recovery_intent(temp.path());
        for path in [
            &intent.payload_live,
            &intent.payload_retired,
            &intent.payload_staging,
            &intent.integrity_live,
            &intent.integrity_retired,
        ] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(intent.payload_live.join("version"), "new").unwrap();

        recover_update_directories(&intent, true, &payload_root, &integrity_root).unwrap();

        assert_eq!(
            fs::read_to_string(intent.payload_live.join("version")).unwrap(),
            "new"
        );
        assert!(!intent.payload_retired.exists());
        assert!(!intent.payload_staging.exists());
        assert!(!intent.integrity_retired.exists());
    }

    #[test]
    fn durable_journal_excludes_a_second_update_of_the_same_package() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (first, _, _) = recovery_intent(temp.path());
        let (second, _, _) = recovery_intent(temp.path());
        let journal = UpdateJournal::begin(temp.path(), first).expect("first journal");

        assert!(UpdateJournal::begin(temp.path(), second).is_err());

        drop(journal);
        let (third, _, _) = recovery_intent(temp.path());
        assert!(UpdateJournal::begin(temp.path(), third).is_ok());
    }
}
