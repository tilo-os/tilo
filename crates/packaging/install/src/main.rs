//! qos-install — the user-facing CLI to install, run, and manage qOS
//! packages (.qpkg) locally, verifying the package signature and trust
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
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Args, Parser, Subcommand};
use install::shim;
use pkg::blobs::BlobStore;
use pkg::trust::{
    AttestorScope, RevocationList, RevokedKey, TrustedAttestor, TrustedAttestors,
    default_revoked_keys_path, default_trusted_attestors_path,
};
use pkg::{MemoryBlobStore, PackageId, Verification, unpack, verify_with_trust};
use serde::{Deserialize, Serialize};

/// Mirrors qos-pkg-cli's `QpkgHeader` (kept local to avoid a circular dep).
#[derive(Debug, Deserialize)]
struct QpkgHeader {
    /// `qpkg:<hex>` form of the `PackageId`.
    package_id: String,
    #[allow(dead_code)]
    format: u32,
}

#[derive(Debug, Parser)]
#[command(
    name = "qos-install",
    about = "Install, launch, list, or remove qOS packages."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// Trust-related flags shared by `install` and `run`.
#[derive(Debug, Args, Clone, Default)]
struct TrustFlags {
    /// Require the parsed qpkg package-id/root to equal this 64-character hex hash.
    #[arg(long)]
    require_package_root: Option<String>,
    /// Require the qpkg's embedded publisher to equal this 64-character hex key.
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
    /// Install a .qpkg into the local store. Verifies signature first.
    #[command(alias = "add")]
    Install {
        /// Path to the .qpkg file.
        qpkg: PathBuf,
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
        /// Package id (as printed by `qos-install list`); omit to update all.
        pkg_id: Option<String>,
        /// Update from a local .qpkg instead of a configured registry
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
        /// Package id (as printed by `qos-install list`).
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
        /// Package id (as printed by `qos-install list`).
        pkg_id: String,
    },
    /// Unpin an installed package (drops it back to the eviction pool).
    Unpin {
        /// Package id (as printed by `qos-install list`).
        pkg_id: String,
    },
    /// Launch an installed package via qos-launcher.
    Run {
        /// Package id.
        pkg_id: String,
        /// Forwarded to qos-launcher.
        #[arg(long)]
        no_services: bool,
        /// Forwarded to qos-launcher. Deprecated — apps with an installed
        /// `qos-runtime/<v>` bundle don't need this; silently overridden when
        /// a bundle resolves.
        #[arg(long, hide = true)]
        minimal_libs: bool,
        /// Forwarded to qos-launcher; falls back to --minimal-libs if the
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
    /// Manage installed qOS runtime bundles.
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
    /// `$XDG_DATA_HOME/qos-runtime/<version>/`, from the host root or an
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
        /// Path to a signing identity JSON (see `qos-runtime-bundle key
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
    #[error("qpkg verification failed: {0}")]
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
    #[error("qpkg parse: {0}")]
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
    #[error("runtime bundle qos-runtime/{0} is already installed (use --force to overwrite)")]
    RuntimeBundleExists(String),
    #[error("runtime bundle qos-runtime/{0} is not installed")]
    RuntimeBundleNotInstalled(String),
    #[error(
        "{package} declares runtime `{runtime}`, and no bundle for it is installed. \
         That package will not launch: the launcher refuses to start an app whose \
         declared runtime is absent.\n  \
         Install it and retry:  qos-install runtime install --version {version} --from-host --identity <key.json>\n  \
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
    #[error("the .qpkg is for {found}, but {wanted} was asked for")]
    WrongPackage { wanted: String, found: String },
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
    /// Content-addressed root hash (hex, no `qpkg:` prefix).
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
    /// The manifest's declared `runtime = "qos-runtime/<v>"`, if any — lets
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
    // Honour QOS_APPS_DATA_DIR (tests/dev installs); matches qos-launcher::registry.
    if let Some(dir) = std::env::var_os("QOS_APPS_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let base = dirs::data_dir().ok_or(Error::NoDataDir)?;
    Ok(base.join("qos-apps"))
}

fn registry_path() -> Result<PathBuf, Error> {
    Ok(data_root()?.join("registry.toml"))
}

fn payloads_root() -> Result<PathBuf, Error> {
    Ok(data_root()?.join("payloads"))
}

/// The user's data home (parent of `qos-rooms/`), distinct from
/// [`data_root`]: this follows `XDG_DATA_HOME` like the launcher does,
/// not `QOS_APPS_DATA_DIR` — using the wrong one silently loses "remove data".
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
) -> Result<(pkg::Manifest, String, String), Error> {
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
                    "qos-install: --require-attestor {required:?} unmatched; --allow-self-signed permits self-signed publisher ed25519:{}",
                    hex_of(&identity)
                );
            } else if !trust.allow_self_signed {
                // Default (current) behavior: warn and proceed.
                eprintln!(
                    "qos-install: WARN — package self-signed by ed25519:{} (no trusted-attestor signature); use --require-attestor <id> to demand an attestation",
                    hex_of(&identity)
                );
            }
            Ok((manifest, hex_of(&identity), String::new()))
        }
        Verification::Revoked { reason, pubkey, .. } => {
            eprintln!(
                "qos-install: REFUSED — publisher ed25519:{} is revoked: {reason}",
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

/// Everything a verified `.qpkg` yields, before any file is written.
/// Reading/verifying is kept separate from placing files so `--dry-run`
/// is truthful and `update` can decide on a swap before touching a
/// working install.
struct Ingested {
    pkg_id: PackageId,
    store: MemoryBlobStore,
    manifest: pkg::Manifest,
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
    /// `qos-runtime/<v>`, when the manifest pins one.
    fn runtime_ref(&self) -> Option<&str> {
        self.manifest.package.runtime.as_deref()
    }
}

/// Read, verify and gate a `.qpkg`'s bytes, before any file is written.
///
/// Signature verification is not optional and no flag turns it off;
/// `--allow-self-signed` decides *whose* signature is acceptable, never
/// whether one is required.
fn ingest(bytes: &[u8], trust: &TrustFlags) -> Result<Ingested, Error> {
    let (pkg_id, store) = read_qpkg_bytes(bytes)?;
    let (manifest, publisher_key_hex, attested_by) = verify_and_gate(pkg_id, &store, trust)?;

    // A signature says who wrote the manifest, not that its contents are
    // reasonable — a package naming its own filesystem paths is refused
    // here regardless of signature.
    pkg::validate_manifest(&manifest).map_err(|e| Error::Policy(e.to_string()))?;

    let blob = |hash: &pkg::BlobHash, what: &str| -> Result<Vec<u8>, Error> {
        store
            .get(hash)
            .map_err(|e| Error::Parse(e.to_string()))?
            .ok_or_else(|| Error::Parse(format!(".qpkg is missing its {what}")))
    };
    let signed_root = blob(&pkg::BlobHash(*pkg_id.as_bytes()), "signed root")?;
    let root = pkg::SignedRoot::decode(&signed_root)
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
/// reopening the .qpkg.
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
/// The refusal is the point: catching a missing `qos-runtime/<v>`
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
        println!("  runtime  : qos-runtime/{version} is missing; building it from the host root");
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
            "qos-install: WARN — {} declares runtime `qos-runtime/{version}`, which is \
             not installed. The package will not launch until it is.",
            ingested.id()
        );
        return Ok(());
    }
    Err(Error::RuntimeDependencyMissing {
        package: ingested.id().to_string(),
        runtime: ingested.runtime_ref().unwrap_or("qos-runtime").to_string(),
        version,
    })
}

/// Write the PATH shims a [cli] package publishes; returns the command
/// names recorded on the registry entry.
fn write_cli_shims(manifest: &pkg::Manifest, id_field: &str) -> Result<Vec<String>, Error> {
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
            "qos-install: WARNING — cannot find qos-cli-shim at {}. \
             Skipping shim install. Set QOS_CLI_SHIM_PATH or install the qOS runtime first.",
            shim_source.display()
        );
        return Ok(cli_commands);
    }
    let launcher_override_owned = std::env::var_os("QOS_LAUNCHER_PATH").map(PathBuf::from);
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
        "  PATH     : prepend {} to your shell's PATH to pick them up (see `qos-install path`).",
        bin_dir.display()
    );
    Ok(cli_commands)
}

fn install(
    qpkg: &Path,
    pinned: bool,
    force: bool,
    trust: &TrustFlags,
    runtime: &RuntimeFlags,
    dry_run: bool,
) -> Result<(), Error> {
    // 1. Read + verify, before the registry check, so a --dry-run plan
    //    describes a package we've actually seen and checked.
    let bytes = fs::read(qpkg)?;
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
        print_install_plan(&ingested, existing.as_ref(), &dest, pinned, qpkg)?;
        return Ok(());
    }

    // 3. Dependencies, before any file is written: a half-succeeded install
    //    that then refuses is worse than one that refuses upfront.
    satisfy_runtime(&ingested, runtime)?;

    reg.entry.retain(|e| e.id != id_field);

    // 4. Unpack into payloads/<id> — the reverse-DNS id (not content hash) is the install slot.
    place_payload(&ingested, &dest)?;
    let manifest_path = dest.join("manifest.toml");

    // 5. The evidence a later `qos-install verify` replays.
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
        source: qpkg.display().to_string(),
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
    println!("run with:  qos-install run {id_field}");
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
        Some((v, true)) => println!("  runtime   : qos-runtime/{v} (installed)"),
        Some((v, false)) => println!(
            "  runtime   : qos-runtime/{v} MISSING — install would refuse without \
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
                "--from names one .qpkg, so it needs one package id".into(),
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
            Some((v, true)) => println!("  runtime   : qos-runtime/{v} (installed)"),
            Some((v, false)) => println!("  runtime   : qos-runtime/{v} MISSING"),
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

    // 3. Swap: the new payload is built beside the old one and moved into
    //    place, so a mid-failure leaves the old version working, not half-replaced.
    swap_payload(&ingested, &entry.payload_root)?;

    // 4. Records: integrity first, since a registry entry claiming a
    //    version whose evidence isn't written yet would fail its own `verify`.
    let data_root = data_root()?;
    install::integrity::record(
        &data_root,
        &entry.id,
        &ingested.signed_root,
        &ingested.manifest_bytes,
        &ingested.collection_bytes,
    )?;

    // The new version may publish a different set of commands, so the
    // old shims go before the new ones are written.
    if !entry.cli_commands.is_empty() {
        let bin_dir = bin_root()?;
        let _ = shim::remove_shims(&bin_dir, &entry.cli_commands);
    }
    let cli_commands = write_cli_shims(&ingested.manifest, &entry.id)?;

    let mut reg = load_registry()?;
    if let Some(slot) = reg.entry.iter_mut().find(|e| e.id == entry.id) {
        slot.version = ingested.version().to_string();
        slot.package_id_hex = hex_of(ingested.pkg_id.as_bytes());
        slot.publisher_key_hex
            .clone_from(&ingested.publisher_key_hex);
        slot.attested_by.clone_from(&ingested.attested_by);
        slot.installed_at = now();
        slot.cli_commands = cli_commands;
        slot.runtime = ingested.runtime_ref().unwrap_or_default().to_string();
        slot.source.clone_from(&source);
    }
    save_registry(&reg)?;

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
fn swap_payload(ingested: &Ingested, live: &Path) -> Result<(), Error> {
    let parent = live
        .parent()
        .ok_or_else(|| Error::Swap {
            package: ingested.id().to_string(),
            detail: format!("{} has no parent directory", live.display()),
        })?
        .to_path_buf();
    fs::create_dir_all(&parent)?;
    let pid = std::process::id();
    let staging = parent.join(format!(".staging-{}-{pid}", ingested.id()));
    let retired = parent.join(format!(".retiring-{}-{pid}", ingested.id()));

    place_payload(ingested, &staging)?;

    if live.exists() {
        fs::rename(live, &retired).map_err(|e| Error::Swap {
            package: ingested.id().to_string(),
            detail: format!("moving the installed payload aside: {e}"),
        })?;
    }
    if let Err(e) = fs::rename(&staging, live) {
        // Restore the working install before reporting — leaving no payload at
        // all is the one outcome this exists to prevent.
        if retired.exists() {
            let _ = fs::rename(&retired, live);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(Error::Swap {
            package: ingested.id().to_string(),
            detail: format!("moving the new payload into place: {e}"),
        });
    }
    let _ = fs::remove_dir_all(&retired);
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
/// verified `.qpkg` bytes plus the registry that served them.
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
            eprintln!("qos-install: could not remove {}: {e}", path.display());
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
    reason = "each bool is an independent `qos-install run` flag forwarded to the launcher; \
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
            "qos-install: REFUSED — installed package's publisher is revoked: {}",
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
                "qos-install: --require-attestor {required:?} unmatched; --allow-self-signed permits install-time attested-by {:?}",
                entry.attested_by
            );
        }
    }

    // Find qos-launcher on PATH; fall back to a sibling of this binary.
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
            "qos-install: WARNING — --no-landlock disables kernel-level FS/net restrictions. Only for debugging."
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
    println!("# installed via `qos-install install <pkg.qpkg>` are picked up.");
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
    println!("package-id    : qpkg:{}", entry.package_id_hex);
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
            "recorded (run `qos-install verify` to replay it)"
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
    println!("installed runtime bundle: qos-runtime/{version}");
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
        .join("qos-runtime");
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
    println!("removed runtime bundle: qos-runtime/{version}");
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
    println!("verified: qos-runtime/{version}");
    println!("  path     : {}", path.display());
    println!("  files    : {}", manifest.files.len());
    println!("  root-hash: blake3:{}", manifest.root_hash);
    Ok(())
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Install {
            qpkg,
            pinned,
            force,
            trust,
            runtime,
            dry_run,
        } => install(&qpkg, pinned, force, &trust, &runtime, dry_run),
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
    };
    if let Err(e) = result {
        eprintln!("qos-install: {e}");
        std::process::exit(1);
    }
}

/// Read a .qpkg tar → [`MemoryBlobStore`] + `PackageId`. Mirrors
/// (deliberately duplicated) qos-pkg-cli::read_qpkg so this crate
/// stays independent.
#[allow(dead_code)]
fn read_qpkg(path: &Path) -> Result<(PackageId, MemoryBlobStore), Error> {
    read_qpkg_bytes(&fs::read(path)?)
}

/// Same, from bytes already in memory — what a registry fetch returns,
/// enabling verify-before-write.
fn read_qpkg_bytes(bytes: &[u8]) -> Result<(PackageId, MemoryBlobStore), Error> {
    let mut ar = tar::Archive::new(std::io::Cursor::new(bytes));
    let mut header: Option<QpkgHeader> = None;
    let mut store = MemoryBlobStore::new();
    for entry in ar.entries().map_err(|e| Error::Parse(e.to_string()))? {
        let mut entry = entry.map_err(|e| Error::Parse(e.to_string()))?;
        let entry_path = entry.path().map(|p| p.to_path_buf()).unwrap_or_default();
        let name = entry_path.to_string_lossy().to_string();
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|e| Error::Parse(e.to_string()))?;
        match name.as_str() {
            "qpkg.toml" => {
                let s = String::from_utf8(buf)
                    .map_err(|e| Error::Parse(format!("qpkg.toml not UTF-8: {e}")))?;
                header =
                    Some(toml::from_str(&s).map_err(|e| Error::Parse(format!("qpkg.toml: {e}")))?);
            }
            n if n.starts_with("blobs/") => {
                store
                    .put(&buf)
                    .map_err(|e| Error::Parse(format!("blob store: {e}")))?;
            }
            // signed-root/manifest.toml/collection.qcl are also duplicated under
            // blobs/<hex>; ingest defensively in case a packager omitted the duplicate.
            "signed-root" | "manifest.toml" | "collection.qcl" => {
                store
                    .put(&buf)
                    .map_err(|e| Error::Parse(format!("blob store: {e}")))?;
            }
            _ => {}
        }
    }
    let header = header.ok_or_else(|| Error::Parse(".qpkg is missing qpkg.toml".into()))?;
    let id: PackageId = header
        .package_id
        .parse()
        .map_err(|e| Error::Parse(format!("qpkg.toml package_id: {e:?}")))?;
    Ok((id, store))
}

/// Best-effort publisher key extraction: reads the signed-root blob by
/// `PackageId` and decodes the 32-byte `publisher_identity`.
fn extract_publisher_key(id: &PackageId, store: &MemoryBlobStore) -> Option<[u8; 32]> {
    let blob = store
        .get(&pkg::blobs::BlobHash(*id.as_bytes()))
        .ok()
        .flatten()?;
    let signed = pkg::signed::SignedRoot::decode(&blob).ok()?;
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
        let sib = parent.join("qos-launcher");
        if sib.exists() {
            return sib;
        }
    }
    PathBuf::from("qos-launcher")
}
