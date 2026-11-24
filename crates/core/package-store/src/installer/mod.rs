//! Installing a `.tapp`: the one implementation every entry shares. The device daemon's package
//! cache calls it on a running device, and an image build calls it in-process for the image's
//! store, where no device exists yet.
//!
//! It reads and verifies the archive before anything is stored (signature, trust chain, the policy
//! the caller asks for, the size the target permits), refuses a package this system could not run
//! (interfaces, runtime builds), puts the payload into the content store, writes the PATH shims a
//! `cli` package publishes and places runtime bundles and language packs where their readers look.
//! What it did comes back as a [`Report`] for the caller to show.

mod language_pack;
mod runtime;

pub use runtime::seal as seal_runtime_bundle;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_app_package::trust::{RevocationList, RevokedKey, TrustedAttestor};
use tilo_app_package::{
    BlobFetcher, DiscardBlobs, PackageId, PackageMeta, Verification, verify_with_trust,
};

use crate::{Admission, Catalog, InstalledPackage, PackageStore, StoreError, shim, state};

/// Whose signature an install accepts, beyond a valid one.
///
/// No field turns signature verification off: [`Trust::allow_self_signed`] decides whose
/// signature is acceptable, never whether one is required.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trust {
    /// The package root (64 hex characters) the archive must be.
    pub require_package_root: Option<String>,
    /// The publisher key (64 hex characters) that must have signed it.
    pub require_publisher: Option<String>,
    /// The trusted attestor that must vouch for the publisher.
    pub require_attestor: Option<String>,
    /// Accept a publisher no trusted attestor vouches for. With
    /// [`Trust::require_attestor`], only when that attestor's match fails.
    pub allow_self_signed: bool,
    /// The attestors the workspace trusts, as its trust list reads them.
    #[serde(default)]
    pub attestors: Vec<TrustedAttestor>,
    /// The publisher keys the workspace refuses.
    #[serde(default)]
    pub revoked: Vec<RevokedKey>,
}

impl Trust {
    /// The revocation list, as the installer's verification takes it.
    #[must_use]
    pub fn revocation(&self) -> RevocationList {
        RevocationList {
            revoked: self.revoked.clone(),
        }
    }
}

/// What an install does when the package is already installed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Replacement {
    /// Refuse: the package is already installed.
    #[default]
    Refuse,
    /// Replace the installed version.
    Replace,
    /// Replace it, unless the person holds an explicitly chosen version.
    PreserveHeld,
}

/// One install of a `.tapp`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub trust: Trust,
    /// Exempt the package from eviction under size pressure.
    pub pinned: bool,
    pub replacement: Replacement,
    /// Install although its runtime build is missing; it will not launch until that is installed.
    pub ignore_missing_runtime: bool,
    /// Report what would change and change nothing.
    pub dry_run: bool,
}

/// One update of an installed package to a candidate `.tapp`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRequest {
    pub trust: Trust,
    pub origin: Origin,
    /// Install an older version than the installed one.
    pub allow_downgrade: bool,
    pub ignore_missing_runtime: bool,
    pub dry_run: bool,
}

/// Where an update's candidate comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    /// A local file, which has no signed registry entry authorising a change of publisher key.
    #[default]
    LocalFile,
    /// A registry's signed entry, whose owner may rotate the publisher key.
    Registry,
}

/// What an install or update ended in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    /// Nothing was written; the report says what would have been.
    #[default]
    Planned,
    /// An app is installed and selected.
    Installed,
    /// An installed package was replaced by the candidate.
    Updated,
    /// The candidate is the version installed.
    AlreadyCurrent,
    /// The person's held version stays selected; the candidate was stored beside it.
    Preserved,
    /// A runtime package, its bundle where the launcher looks for it.
    Runtime,
    /// A language pack, its catalogues switched in.
    LanguagePack,
}

/// The package a change left selected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selected {
    pub id: String,
    pub version: String,
    /// The package root, 64 hex characters.
    pub root: String,
}

/// What an install or update did, for the caller to show.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub outcome: Outcome,
    pub selected: Option<Selected>,
    /// What changed, line by line, as a person reads it.
    pub lines: Vec<String>,
    /// What the person should know that did not stop the change.
    pub warnings: Vec<String>,
}

impl Report {
    fn line(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }

    fn warn(&mut self, warning: impl Into<String>) {
        self.warnings.push(warning.into());
    }

    fn select(&mut self, package: &InstalledPackage) {
        self.selected = Some(Selected {
            id: package.id.clone(),
            version: package.version.clone(),
            root: package.package_id_hex.clone(),
        });
    }
}

/// Why a package operation failed, as the person is told.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("data-dir resolution failed (XDG_DATA_HOME / HOME unset?)")]
    NoDataDir,
    #[error("config-dir resolution failed (XDG_CONFIG_HOME / HOME unset?)")]
    NoConfigDir,
    #[error("tapp verification failed: {0}")]
    Verify(String),
    #[error("publisher key ed25519:{publisher} is revoked ({reason})")]
    Revoked { publisher: String, reason: String },
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
    #[error("Flatpak: {0}")]
    Flatpak(String),
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
    RequiredRuntimeMissing { package: String, runtime: String },
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
    #[error("package store: {0}")]
    Store(String),
    #[error(
        "{failed} of {checked} installed package(s) no longer match what was installed. \
         Reinstall them, or investigate — the payload has changed since it was verified."
    )]
    VerificationFailed { failed: usize, checked: usize },
}

/// A store error as the person is told it.
#[must_use]
pub fn store_error(error: StoreError) -> Error {
    match error {
        StoreError::NotInstalled(id) => Error::NotInstalled(id),
        other => Error::Store(other.to_string()),
    }
}

/// The person's data home (parent of `tilo-workspaces/`): it follows `XDG_DATA_HOME` like the
/// launcher, not `TILO_APPS_DATA_DIR`.
///
/// # Errors
/// [`Error::NoDataDir`] without one.
pub fn data_home_root() -> Result<PathBuf, Error> {
    tilo_paths::env_dir("XDG_DATA_HOME")
        .or_else(tilo_paths::data_dir)
        .ok_or(Error::NoDataDir)
}

/// A `.tapp` on disk and where it came from, as its install records it.
#[derive(Clone, Debug)]
pub struct Candidate {
    path: PathBuf,
    source: String,
}

impl Candidate {
    /// A local file, recorded as its path.
    #[must_use]
    pub fn file(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            source: path.display().to_string(),
        }
    }

    /// A file staged from elsewhere, recorded as `source`.
    #[must_use]
    pub fn staged(path: &Path, source: impl Into<String>) -> Self {
        Self {
            path: path.to_path_buf(),
            source: source.into(),
        }
    }

    fn reader(&self) -> Result<Box<dyn Read + Send + '_>, Error> {
        Ok(Box::new(std::io::BufReader::new(fs::File::open(
            &self.path,
        )?)))
    }

    fn len(&self) -> Result<u64, Error> {
        Ok(fs::metadata(&self.path)?.len())
    }
}

/// Everything a verified `.tapp` yields, before anything is stored.
pub struct Checked {
    pub meta: PackageMeta,
    pub manifest: tilo_app_package::Manifest,
    pub publisher_key_hex: String,
    /// The trusted attestor that signed for the publisher; empty if none.
    pub attested_by: String,
    /// A publisher this device trusts signed it, directly or through an attestor; false for a valid
    /// self-signed or unknown publisher.
    pub verified: bool,
}

impl Checked {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.manifest.package.id
    }

    #[must_use]
    pub fn version(&self) -> &str {
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

/// Decode a 64-character hex key or hash.
///
/// # Errors
/// What is wrong with it.
pub fn decode_hex_32(s: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(s).map_err(|e| e.to_string())?;
    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| format!("expected 32 bytes, got {}", bytes.len()))
}

/// Hold the package to `require_package_root` and `require_publisher`; returns the publisher keys
/// [`verify_with_trust`] must accept.
fn validate_registry_constraints(
    pkg_id: PackageId,
    meta: &PackageMeta,
    trust: &Trust,
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

fn verify_and_check(
    meta: &PackageMeta,
    trust: &Trust,
    report: &mut Report,
) -> Result<Checked, Error> {
    let revocation = trust.revocation();
    let required_publishers = validate_registry_constraints(meta.id, meta, trust)?;
    let outcome = verify_with_trust(meta, &required_publishers, &trust.attestors, &revocation);
    let (manifest, publisher, attested_by, verified) = match outcome {
        Verification::AttestedBy { attestor, manifest } => {
            if let Some(required) = &trust.require_attestor
                && required != &attestor
            {
                return Err(Error::AttestorMismatch {
                    required: required.clone(),
                    found: attestor,
                });
            }
            (manifest, meta.publisher(), attestor, true)
        }
        Verification::UntrustedPublisher { identity, manifest } => {
            // `require_attestor` set: refuse unless `allow_self_signed` rescues it.
            if let Some(required) = &trust.require_attestor {
                if !trust.allow_self_signed {
                    return Err(Error::RequireAttestorWithoutAttestation {
                        required: required.clone(),
                    });
                }
                report.warn(format!(
                    "--require-attestor {required:?} unmatched; --allow-self-signed permits self-signed publisher ed25519:{}",
                    hex::encode(identity)
                ));
            } else if !trust.allow_self_signed {
                report.warn(format!(
                    "WARN — package self-signed by ed25519:{} (no trusted-attestor signature); use --require-attestor <id> to demand an attestation",
                    hex::encode(identity)
                ));
            }
            (manifest, identity, String::new(), false)
        }
        Verification::Revoked { reason, pubkey, .. } => {
            return Err(Error::Revoked {
                publisher: hex::encode(pubkey),
                reason,
            });
        }
        Verification::Trusted { manifest } => {
            if let Some(required) = &trust.require_attestor {
                // A trusted key bypasses the attestor chain, and `require_attestor` is strict, so
                // refuse here regardless.
                return Err(Error::AttestorMismatch {
                    required: required.clone(),
                    found: "trusted-key".into(),
                });
            }
            (manifest, meta.publisher(), String::new(), true)
        }
        other => return Err(Error::Verify(format!("{other:?}"))),
    };
    Ok(Checked {
        meta: meta.clone(),
        manifest,
        publisher_key_hex: hex::encode(publisher),
        attested_by,
        verified,
    })
}

/// Read, verify and check a `.tapp`, storing nothing.
///
/// Every payload blob the archive carries is checked against the signed collection on the way.
///
/// # Errors
/// A malformed or partly carried archive, a signature or trust refusal, a package for another
/// host, or one larger than this target permits.
pub fn check(candidate: &Candidate, trust: &Trust, report: &mut Report) -> Result<Checked, Error> {
    let tapp = tilo_app_package::tapp::read(candidate.reader()?, &mut DiscardBlobs)
        .map_err(|error| Error::Parse(error.to_string()))?;
    if !tapp.carried.is_empty() && !tapp.is_complete() {
        return Err(Error::Parse(format!(
            ".tapp carries part of its payload and lacks {} blob(s)",
            tapp.missing().len()
        )));
    }
    let checked = verify_and_check(&tapp.meta, trust, report)?;
    let len = candidate.len()?;
    tilo_app_package::desktop::validate_native_host(&checked.manifest)
        .map_err(|e| Error::Policy(e.to_string()))?;
    // The store's limit for this package on this machine: a developer session's own builds install
    // under the local-build ceiling.
    if let Some(limit) = crate::size_limit::app_size_limit(&checked.meta)
        && len > limit
    {
        return Err(Error::Policy(format!(
            "application .tapp is {len} bytes; this target's signed package contract permits at most {limit} bytes"
        )));
    }
    Ok(checked)
}

/// Install a development build: `bytes` is a `.tapp` marked as `identity`'s dev build of an app
/// and signed by it, as the device's daemon packs one (`tilo-app-package::pack_dev`).
///
/// The device installs only what it signed itself, and only under the id its dev marking
/// derives, so a dev build never takes an installed app's identity or grants.
///
/// # Errors
/// The package is not marked as a dev build, names another device, is unsigned, is signed by
/// another key, runs as an id its marking does not derive, or its id belongs to an app this
/// device installed from a publisher.
pub fn install_dev(
    store: &PackageStore,
    catalog: &Catalog,
    bytes: &[u8],
    identity: &tilo_app_package::SigningIdentity,
    source: &str,
) -> Result<Report, Error> {
    let device = identity.public_key_bytes();
    let key = hex::encode(device);
    let refuse = |why: String| Error::Policy(format!("a development build: {why}"));
    let mut blobs = std::collections::BTreeMap::new();
    let tapp = tilo_app_package::tapp::read(bytes, &mut blobs)
        .map_err(|error| refuse(error.to_string()))?;
    let Some(dev) = tapp.meta.manifest.package.dev.clone() else {
        return Err(refuse(
            "it names no device it was built on (`[package.dev]`)".into(),
        ));
    };
    let named = dev
        .device_bytes()
        .map_err(|error| refuse(format!("its device key: {error}")))?;
    if named != device {
        return Err(refuse(format!(
            "it was built on another device, ed25519:{}; this device, ed25519:{key}, runs only \
             its own",
            hex::encode(named)
        )));
    }
    if tapp.meta.root.publisher_identity != device {
        return Err(refuse(format!(
            "it is signed by ed25519:{}, not by this device",
            hex::encode(tapp.meta.root.publisher_identity)
        )));
    }
    match tilo_app_package::verify(&tapp.meta, &[device]) {
        tilo_app_package::Verification::Trusted { .. } => {}
        other => {
            return Err(refuse(format!(
                "it carries no signature this device made ({other:?})"
            )));
        }
    }
    if tapp.meta.manifest.package.kind != tilo_app_package::Kind::App {
        return Err(refuse("it is no app".into()));
    }
    let expected = tilo_app_package::dev_app_id(&dev.of, &device);
    if tapp.meta.manifest.package.id != expected {
        return Err(refuse(format!(
            "a build of {} runs as {expected}, not {}",
            dev.of, tapp.meta.manifest.package.id
        )));
    }
    let id = tapp.meta.manifest.package.id.clone();
    if let Some(existing) = catalog
        .find(&id)
        .map_err(|error| Error::RegistryRead(error.to_string()))?
    {
        let installed = std::fs::read_to_string(&existing.manifest_path)
            .ok()
            .and_then(|text| tilo_app_package::Manifest::from_toml_str(&text).ok())
            .and_then(|manifest| manifest.package.dev);
        if installed.is_none() {
            return Err(refuse(format!(
                "{id} is an app this device installed; a development build never takes its id"
            )));
        }
    }
    // The store reads a candidate from a file: write the very package the device signed there.
    let tmp = store.layout().tmp();
    std::fs::create_dir_all(&tmp)?;
    let path = tmp.join(format!("dev-{id}-{}.tapp", std::process::id()));
    tilo_app_package::tapp::write_file(
        &path,
        &tapp.meta,
        Some(&blobs),
        tilo_app_package::tapp::WriteOptions {
            existing: tilo_app_package::tapp::ExistingOutput::Replace,
            size_limit: tilo_app_package::tapp::SizeLimit::LocalBuild,
        },
    )
    .map_err(|error| refuse(error.to_string()))?;
    let installed = install(
        store,
        catalog,
        &Candidate::staged(&path, source),
        &Request {
            trust: Trust {
                require_publisher: Some(key),
                allow_self_signed: true,
                ..Trust::default()
            },
            replacement: Replacement::Replace,
            ..Request::default()
        },
        None,
    );
    let _ = std::fs::remove_file(&path);
    installed
}

/// Whether a user copy of `version` must be held in front of the image's copy: it is older, so only
/// an explicit choice (a rollback, a forced install) put it there.
fn held_below_image(layers: &crate::Layers, version: &str) -> bool {
    layers.image.as_ref().is_some_and(|image| {
        tilo_app_package::version::compare(version, &image.version) == std::cmp::Ordering::Less
    })
}

fn layers(catalog: &Catalog, pkg_id: &str) -> Result<crate::Layers, Error> {
    catalog
        .layers(pkg_id)
        .map_err(|e| Error::RegistryRead(e.to_string()))
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

/// The store commit, keeping pins and, with `preserve_held`, a version the person holds. `None`
/// when the hold won and the candidate was only stored beside it.
fn commit(
    store: &PackageStore,
    candidate: &Candidate,
    admission: impl FnOnce(&tilo_app_package::PackageMeta) -> Result<Admission, String> + Send,
    preserve_held: bool,
    payload: Option<&dyn BlobFetcher>,
    report: &mut Report,
) -> Result<Option<crate::Installed>, Error> {
    if !preserve_held {
        return store
            .install(candidate.reader()?, admission, payload)
            .map(Some)
            .map_err(store_error);
    }
    match store
        .install_preserving_hold(candidate.reader()?, admission, payload)
        .map_err(store_error)?
    {
        crate::InstallSelection::Installed(installed) => Ok(Some(*installed)),
        crate::InstallSelection::Preserved(row) => {
            cohort_selection(&row, true, report);
            report.outcome = Outcome::Preserved;
            report.select(&row);
            Ok(None)
        }
    }
}

/// The receipt a development cohort update reads back: which version is selected and whether the
/// person's held version stayed.
fn cohort_selection(row: &InstalledPackage, preserved: bool, report: &mut Report) {
    let receipt = serde_json::json!({
        "id": row.id, "version": row.version, "root": row.package_id_hex,
        "runtime": row.runtime, "pinned": row.pinned, "held": row.held,
        "preserved": preserved,
    });
    report.line(format!("cohort-selection: {receipt}"));
}

/// The version `raw` (`<name>/<version>`) names, and whether its bundle is installed for this user
/// or in the image.
fn bundle_status(raw: &str) -> Result<(String, bool), Error> {
    let (_, version) = raw
        .split_once('/')
        .ok_or_else(|| Error::RuntimeReferenceMalformed(raw.to_string()))?;
    Ok((version.to_string(), runtime_bundle_present(version)))
}

/// Whether runtime build `version` is installed for this user or in the image.
#[must_use]
pub fn runtime_bundle_present(version: &str) -> bool {
    if version.is_empty() {
        return false;
    }
    runtime_bundle::default_bundle_path(version)
        .ok()
        .into_iter()
        .chain([runtime_bundle::system_bundle_path(version)])
        .any(|p| p.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME).exists())
}

fn runtime_status(checked: &Checked) -> Result<Option<(String, bool)>, Error> {
    checked.runtime_ref().map(bundle_status).transpose()
}

fn missing_required_runtimes(checked: &Checked) -> Result<Vec<String>, Error> {
    let mut missing = Vec::new();
    for raw in tilo_app_package::desktop::required_runtime_dependencies(&checked.manifest) {
        if !bundle_status(raw)?.1 {
            missing.push(raw.clone());
        }
    }
    Ok(missing)
}

/// Refuse a package that requires a system interface version this OS does not serve: installed, it
/// could not work, and it would sit in front of the image's copy that does.
fn fits_os(checked: &Checked) -> Result<(), Error> {
    let provided = crate::interfaces::system_interfaces();
    let unmet = crate::interfaces::unmet(provided.as_ref(), &checked.manifest);
    if unmet.is_empty() {
        return Ok(());
    }
    Err(Error::InterfacesUnmet {
        package: checked.id().to_string(),
        version: checked.version().to_string(),
        unmet: tilo_app_package::interfaces::describe_unmet(&unmet),
    })
}

/// Make sure the declared runtime is available, or refuse the install: catching a missing
/// `tilo-runtime/<v>` bundle here, with a fix command, beats discovering it at first launch.
fn satisfy_runtime(
    checked: &Checked,
    ignore_missing_runtime: bool,
    report: &mut Report,
) -> Result<(), Error> {
    // The further runtimes an app requires travel with it, whichever path installs it: an app
    // replaced while its engine is absent would not start.
    if !ignore_missing_runtime
        && let Some(runtime) = missing_required_runtimes(checked)?.into_iter().next()
    {
        return Err(Error::RequiredRuntimeMissing {
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
    if ignore_missing_runtime {
        report.warn(format!(
            "WARN — {} declares runtime `tilo-runtime/{version}`, which is \
             not installed. The package will not launch until it is.",
            checked.id()
        ));
        return Ok(());
    }
    Err(Error::RuntimeDependencyMissing {
        package: checked.id().to_string(),
        runtime: checked.runtime_ref().unwrap_or("tilo-runtime").to_string(),
    })
}

/// Write the PATH shims a `cli` package publishes, replacing those of the version it replaces.
fn replace_cli_shims(
    store: &PackageStore,
    previous: Option<&InstalledPackage>,
    checked: &Checked,
    report: &mut Report,
) -> Result<(), Error> {
    let bin_dir = store.layout().bin();
    if let Some(old) = previous
        && !old.cli_commands.is_empty()
    {
        let _ = shim::remove_shims(&bin_dir, &old.id, &old.cli_commands);
    }
    let cli_commands = checked.cli_commands();
    if cli_commands.is_empty() {
        return Ok(());
    }
    let shim_source = shim::locate_shim_source();
    if !shim_source.exists() {
        report.warn(format!(
            "WARNING — cannot find tilo-cli-shim at {}. \
             Skipping shim install. Set TILO_CLI_SHIM_PATH or install the Tilo runtime first.",
            shim_source.display()
        ));
        return Ok(());
    }
    let launcher_override = std::env::var_os("TILO_LAUNCHER_PATH").map(PathBuf::from);
    let spec = shim::ShimInstall {
        package_id: checked.id(),
        commands: &cli_commands,
        shim_source: &shim_source,
        bin_dir: &bin_dir,
        launcher_override: launcher_override.as_deref(),
    };
    shim::write_shims(&spec).map_err(|e| Error::Shim(e.to_string()))?;
    report.line(format!(
        "  shims    : wrote {} shim(s) into {}",
        cli_commands.len(),
        bin_dir.display()
    ));
    report.line(format!(
        "  PATH     : prepend {} to your shell's PATH to pick them up (see `tilo-install path`).",
        bin_dir.display()
    ));
    Ok(())
}

/// Hold off runtime collection while a change runs, so a runtime it checked for cannot be
/// collected under it. Without a bundle root there is nothing to collect.
fn hold_off_collection() -> Option<advisory_lock::FileLock> {
    let bundles = runtime_bundle::gc::BundleRoot::from_env().ok()?;
    bundles
        .path()
        .is_dir()
        .then(|| bundles.install_lock().ok())?
}

/// Install `candidate` into `store`; `catalog` lists the same store with the image's packages.
///
/// # Errors
/// Every refusal and failure, before or during the store commit; nothing is selected then.
pub fn install(
    store: &PackageStore,
    catalog: &Catalog,
    candidate: &Candidate,
    request: &Request,
    payload: Option<&dyn BlobFetcher>,
) -> Result<Report, Error> {
    let _collection_held_off = hold_off_collection();
    let mut report = Report::default();
    // Read and verify before the registry check, so a dry-run plan describes a package that was
    // actually seen and checked.
    let checked = check(candidate, &request.trust, &mut report)?;
    match checked.manifest.package.kind {
        tilo_app_package::Kind::Runtime => {
            runtime::install(
                store,
                catalog,
                candidate,
                &checked,
                runtime::Options {
                    replacement: request.replacement,
                    pinned: request.pinned,
                    dry_run: request.dry_run,
                },
                payload,
                &mut report,
            )?;
            return Ok(report);
        }
        tilo_app_package::Kind::LanguagePack => {
            language_pack::install(
                store,
                catalog,
                candidate,
                &checked,
                request.dry_run,
                payload,
                &mut report,
            )?;
            return Ok(report);
        }
        _ => {}
    }

    // An app the system image ships is installed too.
    let layers = layers(catalog, checked.id())?;
    let existing = layers.running().cloned();
    if existing.is_some() && request.replacement == Replacement::Refuse {
        return Err(Error::AlreadyInstalled(checked.id().to_string()));
    }
    if request.dry_run {
        install_plan(
            store,
            &checked,
            existing.as_ref(),
            request.pinned,
            candidate,
            &mut report,
        )?;
        return Ok(report);
    }

    // Dependencies, before anything is stored: a half-succeeded install that then refuses is worse
    // than one that refuses upfront.
    fits_os(&checked)?;
    satisfy_runtime(&checked, request.ignore_missing_runtime, &mut report)?;

    // Into the content store, materialised once; the registry row is the commit point.
    let held = held_below_image(&layers, checked.version());
    let admission = checked_admission(&checked, request.pinned, held, candidate.source.clone());
    let preserve_held = request.replacement == Replacement::PreserveHeld;
    let Some(installed) = commit(
        store,
        candidate,
        admission,
        preserve_held,
        payload,
        &mut report,
    )?
    else {
        return Ok(report);
    };
    replace_cli_shims(store, existing.as_ref(), &checked, &mut report)?;
    if preserve_held {
        cohort_selection(&installed.package, false, &mut report);
    }

    report.outcome = Outcome::Installed;
    report.select(&installed.package);
    report.line(format!("installed: {} {}", checked.id(), checked.version()));
    report.line(format!(
        "  payload  : {}",
        installed.package.payload_root.display()
    ));
    report.line(format!(
        "  stored   : {} blob(s), {} already in the content store",
        installed.blobs, installed.blobs_already_stored
    ));
    report.line(format!("  pinned   : {}", installed.package.pinned));
    if !checked.attested_by.is_empty() {
        report.line(format!("  attested : {}", checked.attested_by));
    }
    if let Some(rt) = checked.runtime_ref() {
        report.line(format!("  runtime  : {rt}"));
    }
    Ok(report)
}

fn install_plan(
    store: &PackageStore,
    checked: &Checked,
    existing: Option<&InstalledPackage>,
    pinned: bool,
    from: &Candidate,
    report: &mut Report,
) -> Result<(), Error> {
    report.line(format!(
        "plan: install {} {}",
        checked.id(),
        checked.version()
    ));
    report.line(format!("  from      : {}", from.source));
    report.line(format!(
        "  signature : verified ({})",
        describe_trust(checked)
    ));
    report.line(match existing {
        Some(e) => format!(
            "  replaces  : {} {} at {}",
            e.id,
            e.version,
            e.payload_root.display()
        ),
        None => "  replaces  : nothing (not currently installed)".to_string(),
    });
    report.line(format!(
        "  payload   : write {}",
        store.layout().payload(checked.meta.id).display()
    ));
    report.line(format!("  pinned    : {pinned}"));
    report.line(match runtime_status(checked)? {
        None => "  runtime   : none declared".to_string(),
        Some((v, true)) => format!("  runtime   : tilo-runtime/{v} (installed)"),
        Some((v, false)) => format!(
            "  runtime   : tilo-runtime/{v} MISSING — install would refuse without \
             --ignore-missing-runtime"
        ),
    });
    let commands = checked.cli_commands();
    report.line(if commands.is_empty() {
        "  shims     : none".to_string()
    } else {
        format!(
            "  shims     : {} in {}",
            commands.join(", "),
            store.layout().bin().display()
        )
    });
    report.line("  state     : untouched (a package's files are not its user's data)");
    report.line("(nothing was written)");
    Ok(())
}

fn describe_trust(checked: &Checked) -> String {
    if checked.attested_by.is_empty() {
        format!("self-signed by ed25519:{}", checked.publisher_key_hex)
    } else {
        format!("attested by {}", checked.attested_by)
    }
}

fn compare_versions(installed: &str, candidate: &str) -> Option<std::cmp::Ordering> {
    let a = semver::Version::parse(installed).ok()?;
    let b = semver::Version::parse(candidate).ok()?;
    Some(a.cmp(&b))
}

/// Whether the candidate may replace `entry`; false when the report already says why the update ends.
fn update_allowed(
    entry: &InstalledPackage,
    checked: &Checked,
    request: &UpdateRequest,
    report: &mut Report,
) -> Result<bool, Error> {
    if checked.id() != entry.id {
        return Err(Error::WrongPackage {
            wanted: entry.id.clone(),
            found: checked.id().to_string(),
        });
    }
    // A local file has no signed registry entry authorising a publisher-key rotation, so keep the
    // identity recorded at install time unless the caller pins the candidate key. A registry update
    // takes its publisher from the entry the registry owner signed, where rotations stay possible.
    if request.origin == Origin::LocalFile
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
    // A package recorded at a format this build does not read was installed by another Tilo build:
    // any build the store offers now replaces it.
    if !crate::integrity::record_current(entry) {
        return Ok(true);
    }
    // Whether it is newer at all: non-semver versions compare as strings, for equality only.
    match compare_versions(&entry.version, checked.version()) {
        Some(std::cmp::Ordering::Equal) | None if entry.version == checked.version() => {
            if checked.manifest.package.kind == tilo_app_package::Kind::Runtime {
                runtime::verify_current(entry, checked)?;
            }
            report.outcome = Outcome::AlreadyCurrent;
            report.select(entry);
            report.line(format!("{} {} is already current", entry.id, entry.version));
            Ok(false)
        }
        Some(std::cmp::Ordering::Greater) if !request.allow_downgrade => Err(Error::NotNewer {
            package: entry.id.clone(),
            installed: entry.version.clone(),
            candidate: checked.version().to_string(),
        }),
        _ => Ok(true),
    }
}

fn update_plan(
    entry: &InstalledPackage,
    checked: &Checked,
    candidate: &Candidate,
    workspaces: &[state::WorkspaceState],
    report: &mut Report,
) -> Result<(), Error> {
    report.line(format!(
        "plan: update {} {} -> {}",
        entry.id,
        entry.version,
        checked.version()
    ));
    report.line(format!("  from      : {}", candidate.source));
    report.line(format!(
        "  signature : verified ({})",
        describe_trust(checked)
    ));
    report.line(if entry.system {
        format!(
            "  payload   : install beside the system image's {}",
            entry.payload_root.display()
        )
    } else {
        format!("  payload   : replace {}", entry.payload_root.display())
    });
    report.line(match runtime_status(checked)? {
        None => "  runtime   : none declared".to_string(),
        Some((v, true)) => format!("  runtime   : tilo-runtime/{v} (installed)"),
        Some((v, false)) => format!("  runtime   : tilo-runtime/{v} MISSING"),
    });
    report.line(format!(
        "  state     : KEPT — {} workspace(s), {}",
        workspaces.len(),
        state::human_bytes(state::total_bytes(workspaces))
    ));
    report.line("(nothing was written)");
    Ok(())
}

/// Update the installed package `pkg_id` to `candidate`, keeping the person's per-workspace data.
/// The new version is materialised beside the old one and the registry row replaced atomically: a
/// failure before that point leaves the old version installed and working.
///
/// # Errors
/// Every refusal and failure; the installed version stays selected then.
pub fn update(
    store: &PackageStore,
    catalog: &Catalog,
    pkg_id: &str,
    candidate: &Candidate,
    request: &UpdateRequest,
    payload: Option<&dyn BlobFetcher>,
) -> Result<Report, Error> {
    let _collection_held_off = hold_off_collection();
    let entry = layers(catalog, pkg_id)?
        .running()
        .cloned()
        .ok_or_else(|| Error::NotInstalled(pkg_id.into()))?;
    let mut report = Report::default();
    let checked = check(candidate, &request.trust, &mut report)?;
    if !update_allowed(&entry, &checked, request, &mut report)? {
        return Ok(report);
    }

    // A language pack or a runtime, each put where its readers look.
    match checked.manifest.package.kind {
        tilo_app_package::Kind::LanguagePack => {
            language_pack::install(
                store,
                catalog,
                candidate,
                &checked,
                request.dry_run,
                payload,
                &mut report,
            )?;
            return Ok(report);
        }
        tilo_app_package::Kind::Runtime => {
            runtime::install(
                store,
                catalog,
                candidate,
                &checked,
                runtime::Options {
                    replacement: Replacement::Replace,
                    pinned: entry.pinned,
                    dry_run: request.dry_run,
                },
                payload,
                &mut report,
            )?;
            return Ok(report);
        }
        _ => {}
    }

    let workspaces = state::find(&data_home_root()?, &entry.id);
    if request.dry_run {
        update_plan(&entry, &checked, candidate, &workspaces, &mut report)?;
        return Ok(report);
    }

    fits_os(&checked)?;
    satisfy_runtime(&checked, request.ignore_missing_runtime, &mut report)?;

    let held = held_below_image(&layers(catalog, &entry.id)?, checked.version());
    let admission = checked_admission(&checked, entry.pinned, held, candidate.source.clone());
    let Some(installed) = commit(store, candidate, admission, false, payload, &mut report)? else {
        return Ok(report);
    };
    replace_cli_shims(store, Some(&entry), &checked, &mut report)?;
    report.outcome = Outcome::Updated;
    report.select(&installed.package);
    report.line(format!(
        "updated: {} {} -> {}",
        entry.id,
        entry.version,
        checked.version()
    ));
    report.line(format!("  from     : {}", candidate.source));
    report.line(format!(
        "  payload  : {}",
        installed.package.payload_root.display()
    ));
    report.line(format!(
        "  stored   : {} blob(s), {} shared with what was installed",
        installed.blobs, installed.blobs_already_stored
    ));
    report.line(format!(
        "  state    : kept ({} workspace(s), {})",
        workspaces.len(),
        state::human_bytes(state::total_bytes(&workspaces))
    ));
    Ok(report)
}

/// Install `candidate`, or update the installed package of the same id to it: what installing a
/// local file means to the person who hands it over.
///
/// # Errors
/// As [`install`] and [`update`].
pub fn install_or_update(
    store: &PackageStore,
    catalog: &Catalog,
    candidate: &Candidate,
    trust: &Trust,
    payload: Option<&dyn BlobFetcher>,
) -> Result<Report, Error> {
    let mut scratch = Report::default();
    let checked = check(candidate, trust, &mut scratch)?;
    let installed = layers(catalog, checked.id())?.running().is_some();
    if installed {
        update(
            store,
            catalog,
            checked.id(),
            candidate,
            &UpdateRequest {
                trust: trust.clone(),
                origin: Origin::LocalFile,
                ..UpdateRequest::default()
            },
            payload,
        )
    } else {
        install(
            store,
            catalog,
            candidate,
            &Request {
                trust: trust.clone(),
                ..Request::default()
            },
            payload,
        )
    }
}
