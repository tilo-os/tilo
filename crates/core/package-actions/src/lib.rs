//! The changes a device makes to its package cache ([`Change`]): what its daemon's actions do
//! (`devices/<device>/packages`, [`published`]), what it does itself for the apps its workspaces
//! name, and what an OS image build does in-process (`tilo-install --offline-store`), where there
//! is no device to ask. All call [`apply`]: one implementation, its entries ([`Entry`]) differing
//! only in where the files a change names are. Writing a package is
//! [`package_store::installer`]'s; this decides which writes a change makes and says what they did.

use std::fs;
use std::path::{Path, PathBuf};

use package_store::installer::{self, Error, data_home_root, decode_hex_32, store_error};
use package_store::{Admission, Catalog, InstalledPackage, PackageStore, shim};
use serde::{Deserialize, Serialize};
use tilo_app_package::trust::{RevocationList, RevokedKey, TrustedAttestor};
use tilo_app_package::{BlobFetcher, PackageId, SigningIdentity};

pub mod catalogue;
#[cfg(unix)]
pub mod device;
pub mod flatpak;
mod offered;
mod published;
pub mod rows;

pub use published::{Published, published, reading};
pub use rows::{PackageAbout, PackageRow, RowState};

/// A file a change names: a path in an image build, `tilo.content:<hash>` for bytes a caller
/// put into the device's content before asking. `source` is also what the store records as
/// where a package came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Uploaded {
    pub source: String,
}

/// One file of a host root a runtime bundle is built from: its path on the host, and the file
/// that holds its bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostFile {
    pub path: String,
    pub file: Uploaded,
}

/// The trust a change gives a package, as `tilo-install`'s flags state it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trust {
    #[serde(default)]
    pub require_package_root: Option<String>,
    #[serde(default)]
    pub require_publisher: Option<String>,
    #[serde(default)]
    pub require_attestor: Option<String>,
    #[serde(default)]
    pub allow_self_signed: bool,
    /// The attestors the workspace trusts, as its trust list reads them.
    #[serde(default)]
    pub attestors: Vec<TrustedAttestor>,
    /// The publisher keys the workspace refuses.
    #[serde(default)]
    pub revoked: Vec<RevokedKey>,
}

/// What an install does when a package of the same id is installed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Replacement {
    #[default]
    Refuse,
    Replace,
    /// Replace, but keep a version the person rolled back to.
    PreserveHeld,
}

/// What an uninstall does with the app's saved data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SavedData {
    Delete,
    Keep,
    /// Not decided: a dry run says the person would be asked; a real uninstall keeps it.
    Ask,
}

/// The changes made to a device's package cache. The ones a caller asks for are the daemon's
/// published actions ([`published`]); the rest the device makes for the apps its workspaces name,
/// and an image build makes in-process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    /// Install the package in `file`.
    Install {
        file: Uploaded,
        #[serde(default)]
        trust: Trust,
        #[serde(default)]
        pinned: bool,
        #[serde(default)]
        replacement: Replacement,
        #[serde(default)]
        ignore_missing_runtime: bool,
        #[serde(default)]
        dry_run: bool,
        /// Registry to fetch payload blobs the package does not carry from.
        #[serde(default)]
        payload_from: Option<String>,
    },
    /// Install the development build of `manifest`'s app: the app's authored manifest and its
    /// payload, as a build leaves them. The device marks it, packs it, signs it with its own key,
    /// and installs it under the id its dev marking derives, apart from any installed app and
    /// its grants.
    InstallDev {
        /// The app's authored manifest (`tilo-app.toml`).
        manifest: Uploaded,
        /// The package's files, each by its path in the payload.
        payload: Vec<HostFile>,
    },
    /// Swap in a newer version of `app` (of every installed package without one), from `file`
    /// or the configured registries.
    Update {
        #[serde(default)]
        app: Option<String>,
        #[serde(default)]
        file: Option<Uploaded>,
        /// A semver requirement for the registries' offer.
        #[serde(default)]
        version: Option<String>,
        #[serde(default)]
        allow_downgrade: bool,
        #[serde(default)]
        dry_run: bool,
        #[serde(default)]
        trust: Trust,
        #[serde(default)]
        ignore_missing_runtime: bool,
        #[serde(default)]
        payload_from: Option<String>,
    },
    /// Remove the device's copy of `app`: payload, registry row and PATH shims.
    Uninstall {
        app: String,
        saved_data: SavedData,
        #[serde(default)]
        dry_run: bool,
    },
    SetPinned {
        app: String,
        pinned: bool,
    },
    /// `app` was opened: what was used last is evicted last.
    Opened {
        app: String,
    },
    /// Reclaim what nothing needs: apps past the size cap, orphaned trees, runtime builds no
    /// package runs on.
    Collect {
        #[serde(default)]
        dry_run: bool,
        #[serde(default = "default_grace")]
        grace_secs: u64,
        #[serde(default = "default_cap")]
        max_store_bytes: u64,
    },
    /// Add the environment image in `file`.
    AddImage {
        file: Uploaded,
        #[serde(default)]
        publisher: Option<String>,
        #[serde(default)]
        payload_from: Option<String>,
        #[serde(default)]
        trust: Trust,
    },
    RemoveImage {
        id: String,
    },
    /// Make the tree of image `id` present, fetching it from `registry` when no store holds it;
    /// answers its path.
    FetchImage {
        id: String,
        registry: String,
        #[serde(default)]
        trust: Trust,
    },
    /// Build a runtime bundle from the host root's files `from` (the device's own host root when
    /// `None`) and install it, signed with the identity in `identity` when there is one.
    InstallRuntime {
        version: String,
        #[serde(default)]
        from: Option<Vec<HostFile>>,
        #[serde(default)]
        arch: Option<String>,
        #[serde(default)]
        identity: Option<Uploaded>,
        #[serde(default)]
        force: bool,
    },
    /// Fetch the package of the runtime build `runtime` that the workspace's repository
    /// `registry` offers, bound to the package root `package` and publisher `publisher` (hex) a
    /// review of that offer named, and install it.
    FetchRuntime {
        registry: String,
        runtime: String,
        package: String,
        publisher: String,
    },
    RemoveRuntime {
        version: String,
    },
    PinRuntime {
        version: String,
        pinned: bool,
    },
    /// A trust list change older builds made to this device. The list is the
    /// workspace's now, changed by its trust actions; this decodes what an
    /// older caller wrote and is refused by [`apply`].
    AddAttestor {
        id: String,
        pubkey: String,
        scope: String,
    },
    RemoveAttestor {
        id: String,
    },
    Revoke {
        pubkey: String,
        #[serde(default)]
        reason: String,
    },
    /// Check `app`'s installed tree (every installed app's without one) against the package it
    /// was installed from: changes nothing, and fails when one differs.
    Verify {
        #[serde(default)]
        app: Option<String>,
    },
    /// Add the Flatpak remote `name` at `url` to the device's system installation.
    FlatpakRemoteAdd {
        name: String,
        url: String,
    },
    /// Add Flathub to the device's system installation.
    FlatpakFlathub,
    /// Enable or disable the Flatpak remote `name` of the device's system installation.
    FlatpakRemoteSetEnabled {
        name: String,
        enabled: bool,
    },
    /// Remove the Flatpak remote `name` from the device's system installation.
    FlatpakRemoteRemove {
        name: String,
    },
}

fn default_grace() -> u64 {
    runtime_bundle::gc::DEFAULT_GRACE_SECS
}

fn default_cap() -> u64 {
    package_store::DEFAULT_MAX_STORE_BYTES
}

/// What a change did, in the words the terminal shows: `lines` on standard output, `warnings` on
/// standard error.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub lines: Vec<String>,
    pub warnings: Vec<String>,
}

/// An image's package id, written `tapp:<hex>` or as the bare hex.
///
/// # Errors
///
/// A sentence naming what is not a package id.
pub fn image_id(raw: &str) -> Result<PackageId, String> {
    let text = if raw.starts_with("tapp:") {
        raw.to_owned()
    } else {
        format!("tapp:{raw}")
    };
    text.parse()
        .map_err(|error| format!("parse: image id {raw:?}: {error}"))
}

/// The store a change writes, with the image's packages beside it, and the size cap collection
/// after the change applies.
pub struct Target<'a> {
    pub store: &'a PackageStore,
    pub catalog: &'a Catalog,
    pub max_store_bytes: u64,
    /// The signing identity of the device whose cache this is, when a device's daemon makes the
    /// change: it marks, packs and signs the development builds it installs, and installs no
    /// other device's. The key never leaves the daemon.
    pub device: Option<&'a SigningIdentity>,
}

impl Target<'_> {
    fn root(&self) -> &Path {
        self.store.layout().root()
    }
}

/// Where a change runs, and so where the files it names are.
pub enum Entry<'a> {
    /// The device's daemon: a file is bytes a caller put first, `tilo.content:<hash>`, which
    /// `files` finds the device's copy of.
    Device(&'a (dyn Fn(&str) -> Option<PathBuf> + Sync)),
    /// The device on its own behalf: the files are on its own disk, where they are named.
    Here,
    /// An OS image build, in this process: the files are where they are named, and the machine
    /// building the image is no host whose search to register with.
    ImageBuild,
}

impl Entry<'_> {
    fn file(&self, named: &Uploaded) -> Result<PathBuf, Error> {
        match self {
            Self::Device(files) => files(&named.source).ok_or_else(|| {
                Error::Policy(format!(
                    "{} is no content this device holds: put its bytes first",
                    named.source
                ))
            }),
            Self::Here | Self::ImageBuild => Ok(PathBuf::from(&named.source)),
        }
    }

    /// Whether an install registers the host's search providers: on a running device only.
    fn registers_host_search(&self) -> bool {
        !matches!(self, Self::ImageBuild)
    }
}

/// What a change says as it goes.
#[derive(Default)]
struct Said(Report);

impl Said {
    fn line(&mut self, line: impl Into<String>) {
        self.0.lines.push(line.into());
    }

    fn warn(&mut self, warning: impl Into<String>) {
        self.0.warnings.push(warning.into());
    }

    fn take(&mut self, report: installer::Report) {
        self.0.warnings.extend(report.warnings);
        self.0.lines.extend(report.lines);
    }
}

/// Make `change` to `target`, from `entry`, which says where the files it names are.
///
/// # Errors
///
/// Why the change was refused or failed, as the person is told, after what it said before then
/// (an update of every app removes copies the image superseded before one fails to fetch).
pub fn apply(target: &Target<'_>, change: Change, entry: &Entry<'_>) -> Result<Report, String> {
    // A payload fetched from a registry runs on a runtime of its own, which no thread of the
    // service's runtime (its blocking pool included) may start: the change gets a thread of its
    // own, whoever calls.
    std::thread::scope(|scope| {
        scope
            .spawn(|| apply_here(target, change, entry))
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

fn apply_here(target: &Target<'_>, change: Change, entry: &Entry<'_>) -> Result<Report, String> {
    let mut said = Said::default();
    let started = tilo_clock::unix_now();
    // What changes the installed set: collection runs after it succeeds.
    let collects = matches!(
        change,
        Change::Install { dry_run: false, .. }
            | Change::InstallDev { .. }
            | Change::Update { dry_run: false, .. }
            | Change::Uninstall { dry_run: false, .. }
    );
    if let Err(error) = run(target, change, entry, &mut said) {
        let Said(Report {
            mut lines,
            warnings,
        }) = said;
        lines.extend(warnings);
        lines.push(error.to_string());
        return Err(lines.join("\n"));
    }
    if collects {
        collect_after(target, started, &mut said);
    }
    Ok(said.0)
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per kind of change, each handing it to the function that makes it"
)]
fn run(
    target: &Target<'_>,
    change: Change,
    entry: &Entry<'_>,
    said: &mut Said,
) -> Result<(), Error> {
    match change {
        Change::Install {
            file: named,
            trust,
            pinned,
            replacement,
            ignore_missing_runtime,
            dry_run,
            payload_from,
        } => install(
            target,
            &entry.file(&named)?,
            named.source,
            &installer::Request {
                trust: installer_trust(&trust),
                pinned,
                replacement: match replacement {
                    Replacement::Refuse => installer::Replacement::Refuse,
                    Replacement::Replace => installer::Replacement::Replace,
                    Replacement::PreserveHeld => installer::Replacement::PreserveHeld,
                },
                ignore_missing_runtime,
                dry_run,
            },
            payload_from,
            entry.registers_host_search(),
            said,
        ),
        Change::InstallDev { manifest, payload } => {
            let Some(device) = target.device else {
                return Err(Error::Policy(
                    "a development build is installed by the device that built it".into(),
                ));
            };
            dev_build(target, &manifest, &manifest.source, &payload, entry, device, said)
        }
        Change::Update {
            app,
            file: named,
            version,
            allow_downgrade,
            dry_run,
            trust,
            ignore_missing_runtime,
            payload_from,
        } => {
            let local = match &named {
                Some(named) => Some(entry.file(named)?),
                None => None,
            };
            let from = local
                .as_deref()
                .zip(named.as_ref().map(|named| named.source.as_str()));
            update(
                target,
                app.as_deref(),
                &UpdateRequest {
                    from,
                    version_req: version.as_deref(),
                    allow_downgrade,
                    dry_run,
                    trust: &trust,
                    ignore_missing_runtime,
                    payload_from: payload_from.map(app_registry::HttpBlobs::new),
                },
                said,
            )
        }
        Change::Uninstall {
            app,
            saved_data,
            dry_run,
        } => uninstall(target, &app, saved_data, dry_run, true, said),
        Change::SetPinned { app, pinned } => {
            target.store.set_pinned(&app, pinned).map_err(store_error)?;
            said.line(format!(
                "{}: {app}",
                if pinned { "pinned" } else { "unpinned" }
            ));
            Ok(())
        }
        Change::Opened { app } => target.store.mark_used(&app).map_err(store_error),
        Change::Verify { app } => verify(target, app.as_deref(), said),
        Change::Collect {
            dry_run,
            grace_secs,
            max_store_bytes,
        } => collect(
            target,
            package_store::gc::Request {
                max_store_bytes,
                grace_secs,
                now: tilo_clock::unix_now(),
                dry_run,
                keep: Vec::new(),
            },
            said,
        ),
        Change::AddImage {
            file: named,
            publisher,
            payload_from,
            trust,
        } => add_image(
            target,
            &entry.file(&named)?,
            &named.source,
            publisher.as_deref(),
            payload_from,
            &trust,
            said,
        ),
        Change::RemoveImage { id } => {
            let id = image_id(&id).map_err(Error::Policy)?;
            let removed = target.store.remove_image(id).map_err(store_error)?;
            said.warn(format!(
                "removed {id}; evicted {} blob(s)",
                removed.evicted_blobs
            ));
            Ok(())
        }
        Change::FetchImage {
            id,
            registry,
            trust,
        } => {
            let store = image_store(target.root(), &trust)?
                .with_fetcher(app_registry::HttpBlobs::new(registry));
            let id = image_id(&id).map_err(Error::Policy)?;
            let tree = package_store::materialized_tree(&store, &id)
                .map_err(|error| Error::Store(error.to_string()))?;
            said.line(tree.display().to_string());
            Ok(())
        }
        Change::InstallRuntime {
            version,
            from,
            arch,
            identity,
            force,
        } => {
            let identity = match &identity {
                Some(named) => Some(entry.file(named)?),
                None => None,
            };
            // The host root's files, as a root of their own the bundle is built from.
            let root = match from {
                Some(files) => Some(host_root(&files, entry)?),
                None => None,
            };
            install_runtime(
                &version,
                root.as_ref().map(tempfile::TempDir::path),
                arch.as_deref(),
                identity.as_deref(),
                force,
                said,
            )
        }
        Change::FetchRuntime {
            registry,
            runtime,
            package,
            publisher,
        } => fetch_runtime(
            target,
            &Offer {
                registry: &registry,
                runtime: &runtime,
                package: &package,
                publisher: &publisher,
            },
            entry.registers_host_search(),
            said,
        ),
        Change::RemoveRuntime { version } => remove_runtime(&version, said),
        Change::PinRuntime { version, pinned } => pin_runtime(&version, pinned, said),
        Change::AddAttestor { .. } | Change::RemoveAttestor { .. } | Change::Revoke { .. } => {
            Err(Error::Policy(
                "the trusted attestors and refused publishers are the workspace's: change them with its trust actions"
                    .into(),
            ))
        }
        Change::FlatpakRemoteAdd { name, url } => {
            flatpak::Flatpak::from_env()
                .remote_add(&name, &url)
                .map_err(Error::Flatpak)?;
            said.line(format!("flatpak: added the remote {name}"));
            Ok(())
        }
        Change::FlatpakFlathub => {
            flatpak::Flatpak::from_env()
                .flathub_add()
                .map_err(Error::Flatpak)?;
            said.line("flatpak: Flathub is a remote now");
            Ok(())
        }
        Change::FlatpakRemoteSetEnabled { name, enabled } => {
            flatpak::Flatpak::from_env()
                .remote_set_enabled(&name, enabled)
                .map_err(Error::Flatpak)?;
            said.line(format!(
                "flatpak: remote {name} {}",
                if enabled { "enabled" } else { "disabled" }
            ));
            Ok(())
        }
        Change::FlatpakRemoteRemove { name } => {
            flatpak::Flatpak::from_env()
                .remote_remove(&name)
                .map_err(Error::Flatpak)?;
            said.line(format!("flatpak: removed the remote {name}"));
            Ok(())
        }
    }
}

/// A runtime build a repository offers, as a review of the offer named it.
struct Offer<'a> {
    registry: &'a str,
    runtime: &'a str,
    package: &'a str,
    publisher: &'a str,
}

/// Fetch the package `offer` names from its repository, through the same verified download an
/// app's own runtime takes, and install it bound to the package root and publisher the review
/// named: a repository that now offers another package of the build is refused.
fn fetch_runtime(
    target: &Target<'_>,
    offer: &Offer<'_>,
    registers_host_search: bool,
    said: &mut Said,
) -> Result<(), Error> {
    let fetch_error = |detail: String| Error::Fetch {
        app_id: offer.runtime.to_owned(),
        detail,
    };
    let offered = app_cli::runtime_offer(offer.runtime, offer.registry)
        .map_err(fetch_error)?
        .ok_or_else(|| {
            fetch_error(format!(
                "{} offers no build of {} for this device",
                offer.registry, offer.runtime
            ))
        })?;
    if !offered
        .tapp_root_hash_hex
        .eq_ignore_ascii_case(offer.package)
        || !offered
            .publisher_pubkey_hex
            .eq_ignore_ascii_case(offer.publisher)
    {
        return Err(Error::Policy(format!(
            "{} now offers another package of {}: review it again",
            offer.registry, offer.runtime
        )));
    }
    let staging =
        tempfile::tempdir_in(target.root()).map_err(|error| Error::Store(error.to_string()))?;
    let staged = staging.path().join("runtime.tapp");
    app_cli::install::fetch_verified(&offered, &staged, &mut |_| {})
        .map_err(|error| fetch_error(error.to_string()))?;
    install(
        target,
        &staged,
        format!("registry {}", offer.registry),
        &installer::Request {
            trust: installer_trust(&Trust {
                require_package_root: Some(offer.package.to_owned()),
                require_publisher: Some(offer.publisher.to_owned()),
                ..Trust::default()
            }),
            pinned: false,
            replacement: installer::Replacement::Replace,
            ignore_missing_runtime: false,
            dry_run: false,
        },
        app_cli::install::payload_source(&offered),
        registers_host_search,
        said,
    )
}

/// Whether `caller` may ask a device to install a development build: the person on this
/// device's own daemon connection, as its seats admitted them.
///
/// A key another person's account acts as on this device, a visitor, an agent and the operating
/// system act for somebody else (or for nobody), and a confined device's person is not their own
/// connection either: all are refused. `personal` is the cache's: this computer's own daemon for
/// its account (not the system's), where that account reaches the daemon alone and the device
/// answers for it before its person's services run.
///
/// # Errors
/// The caller is not the person on this device's own connection: why.
pub fn dev_build_admitted(
    caller: &tilo_actions::remote::Caller,
    personal: bool,
) -> Result<(), String> {
    if caller.is_own_connection() && !caller.is_system() {
        return Ok(());
    }
    // The account running this computer's own daemon is the device, before its person's
    // services run and after: the daemon's socket is that account's alone.
    if personal && caller.is_system() {
        return Ok(());
    }
    Err("a development build is installed by the person on this device's own daemon \
         connection, never by a key, a visitor, an agent or the system"
        .into())
}

/// Mark `manifest`'s app as this device's development build, pack and sign it with `device`, and
/// install it: the device never writes its key anywhere a caller can read it.
fn dev_build(
    target: &Target<'_>,
    manifest: &Uploaded,
    source: &str,
    payload: &[HostFile],
    entry: &Entry<'_>,
    device: &SigningIdentity,
    said: &mut Said,
) -> Result<(), Error> {
    let text = fs::read_to_string(entry.file(manifest)?)?;
    let authored = tilo_app_package::Manifest::from_toml_str(&text)
        .map_err(|error| Error::Parse(format!("the app's manifest: {error}")))?;
    if authored.package.id.trim().is_empty() {
        return Err(Error::Policy("the app's manifest names no id".into()));
    }
    // The build's files, as `pack` walks them: a payload tree under the device's temporary
    // directory, written from the bytes the caller put in the device's content.
    let dir = target.store.layout().tmp().join(format!(
        "dev-build-{}-{}",
        std::process::id(),
        tilo_clock::unix_now()
    ));
    let cleanup = || {
        let _ = fs::remove_dir_all(&dir);
    };
    let written = (|| -> Result<(), Error> {
        fs::create_dir_all(&dir)?;
        for file in payload {
            let relative = Path::new(&file.path);
            if !tilo_app_package::is_safe_path(&file.path) || relative == Path::new("tilo-app.toml") {
                return Err(Error::Policy(format!(
                    "{} is not a payload path",
                    file.path
                )));
            }
            let at = dir.join(relative);
            if let Some(parent) = at.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.file(&file.file)?, &at)?;
        }
        Ok(())
    })();
    if let Err(error) = written {
        cleanup();
        return Err(error);
    }
    let packed = tilo_app_package::pack_dev(&dir, &authored, &authored.package.id, device)
        .map_err(|error| Error::Parse(format!("packing the development build: {error}")));
    let packed = match packed {
        Ok(packed) => packed,
        Err(error) => {
            cleanup();
            return Err(error);
        }
    };
    let bytes = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed))
        .map_err(|error| Error::Parse(format!("writing the development build: {error}")));
    let installed = bytes.and_then(|bytes| {
        installer::install_dev(target.store, target.catalog, &bytes, device, source)
    });
    cleanup();
    let report = installed?;
    let id = report
        .selected
        .as_ref()
        .map_or_else(String::new, |selected| selected.id.clone());
    said.line(format!("installed development build: {id}"));
    Ok(())
}

/// Install the package at `path` (`source` names where it came from), saying how to run it.
fn install(
    target: &Target<'_>,
    path: &Path,
    source: String,
    request: &installer::Request,
    payload_from: Option<String>,
    registers_host_search: bool,
    said: &mut Said,
) -> Result<(), Error> {
    let payload = payload_from.map(app_registry::HttpBlobs::new);
    let report = installer::install(
        target.store,
        target.catalog,
        &installer::Candidate::staged(path, source),
        request,
        payload.as_ref().map(|source| source as &dyn BlobFetcher),
    )?;
    let installed = report.outcome == installer::Outcome::Installed;
    let selected = report.selected.clone();
    said.take(report);
    if installed && let Some(selected) = selected {
        if registers_host_search {
            register_search_providers(said);
        }
        said.line(format!("run with:  tilo-install run {}", selected.id));
    }
    Ok(())
}

/// A host root holding `files` at their paths, to build a runtime bundle from.
fn host_root(files: &[HostFile], entry: &Entry<'_>) -> Result<tempfile::TempDir, Error> {
    let root = tempfile::tempdir()?;
    for host in files {
        let relative = Path::new(&host.path)
            .strip_prefix("/")
            .map_err(|_| Error::Policy(format!("{}: not an absolute host path", host.path)))?;
        if relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(Error::Policy(format!(
                "{}: a host path names only directories and a file",
                host.path
            )));
        }
        let at = root.path().join(relative);
        if let Some(parent) = at.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(entry.file(&host.file)?, &at)?;
    }
    Ok(root)
}

/// The installer's trust from the one a change names.
#[must_use]
pub fn installer_trust(trust: &Trust) -> installer::Trust {
    installer::Trust {
        require_package_root: trust.require_package_root.clone(),
        require_publisher: trust.require_publisher.clone(),
        require_attestor: trust.require_attestor.clone(),
        allow_self_signed: trust.allow_self_signed,
        attestors: trust.attestors.clone(),
        revoked: trust.revoked.clone(),
    }
}

/// Let GNOME Shell and `KRunner` search the resources this computer's runtime host admits, found
/// the way an app finds the host it attaches to. Best effort: an app installs and runs without it.
#[cfg(all(unix, not(target_os = "macos")))]
fn register_search_providers(said: &mut Said) {
    let written = std::env::current_exe()
        .and_then(|program| runtime_portal::native::locate_host(&program))
        .map_err(|error| error.to_string())
        .and_then(|runtime| {
            Ok((
                runtime,
                data_home_root().map_err(|error| error.to_string())?,
            ))
        })
        .and_then(|(runtime, data)| {
            let config = tilo_paths::config_dir().ok_or("no configuration directory")?;
            tilo_os_search::linux_files::write(&data, &config, &runtime)
                .map_err(|error| error.to_string())
        });
    match written {
        Ok(()) => said
            .line("  search   : KRunner finds its resources now, GNOME Shell after the next login"),
        Err(error) => said.warn(format!("host search not registered: {error}")),
    }
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn register_search_providers(_said: &mut Said) {}

struct UpdateRequest<'a> {
    /// The local file to update from, and the name it was given as.
    from: Option<(&'a Path, &'a str)>,
    version_req: Option<&'a str>,
    allow_downgrade: bool,
    dry_run: bool,
    trust: &'a Trust,
    ignore_missing_runtime: bool,
    /// Where payload blobs a local file does not carry come from.
    payload_from: Option<app_registry::HttpBlobs>,
}

fn find_installed(target: &Target<'_>, app: &str) -> Result<InstalledPackage, Error> {
    target
        .catalog
        .layers(app)
        .map_err(|e| Error::RegistryRead(e.to_string()))?
        .running()
        .cloned()
        .ok_or_else(|| Error::NotInstalled(app.into()))
}

/// Fetch a newer version and swap it in, keeping the person's per-workspace data.
fn update(
    target: &Target<'_>,
    app: Option<&str>,
    request: &UpdateRequest<'_>,
    said: &mut Said,
) -> Result<(), Error> {
    let targets: Vec<InstalledPackage> = if let Some(id) = app {
        vec![find_installed(target, id)?]
    } else {
        if request.from.is_some() {
            return Err(Error::Policy(
                "--from names one .tapp, so it needs one package id".into(),
            ));
        }
        if !request.dry_run {
            drop_superseded(target, said)?;
        }
        target
            .catalog
            .list()
            .map_err(|e| Error::RegistryRead(e.to_string()))?
    };
    if targets.is_empty() {
        said.line("(no packages installed)");
        return Ok(());
    }
    let mut updated: u32 = 0;
    let mut current: u32 = 0;
    for entry in &targets {
        match update_one(target, entry, request, said)? {
            installer::Outcome::Planned => {}
            installer::Outcome::AlreadyCurrent => current = current.saturating_add(1),
            _ => updated = updated.saturating_add(1),
        }
    }
    if targets.len() > 1 {
        said.line(format!("{updated} updated, {current} already current"));
    }
    Ok(())
}

/// Remove the person's copies of apps the image now ships a newer version of: nothing runs them any
/// more.
fn drop_superseded(target: &Target<'_>, said: &mut Said) -> Result<(), Error> {
    let superseded = target
        .catalog
        .superseded()
        .map_err(|e| Error::RegistryRead(e.to_string()))?;
    for entry in superseded {
        target.store.remove(&entry.id, true).map_err(store_error)?;
        if !entry.cli_commands.is_empty() {
            let _ =
                shim::remove_shims(&target.store.layout().bin(), &entry.id, &entry.cli_commands);
        }
        said.line(format!(
            "{} {}: the system image ships a newer version; removed this copy",
            entry.id, entry.version
        ));
    }
    Ok(())
}

fn update_one(
    target: &Target<'_>,
    entry: &InstalledPackage,
    request: &UpdateRequest<'_>,
    said: &mut Said,
) -> Result<installer::Outcome, Error> {
    // The candidate is a local file or the configured registries' offer; either way the installer
    // verifies it before anything is stored. A registry serves thin packages: their payload comes
    // from the node that served them, and only the blobs the store lacks are fetched.
    let (staged, candidate, trust, provider) = if let Some((path, source)) = request.from {
        (
            None,
            installer::Candidate::staged(path, source),
            installer_trust(request.trust),
            None,
        )
    } else {
        let offered = offered::fetch_from_registries(entry, request.version_req, request.trust)?;
        let candidate = installer::Candidate::staged(offered.staged.path(), offered.source);
        (
            Some(offered.staged),
            candidate,
            installer_trust(&offered.trust),
            offered.payload,
        )
    };
    let payload: Option<&dyn BlobFetcher> = match (&provider, &request.payload_from) {
        (Some(provider), _) => Some(provider),
        (None, Some(server)) => Some(server),
        (None, None) => None,
    };
    let report = installer::update(
        target.store,
        target.catalog,
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
            ignore_missing_runtime: request.ignore_missing_runtime,
            dry_run: request.dry_run,
        },
        payload,
    );
    drop(staged);
    let report = report?;
    let outcome = report.outcome;
    said.take(report);
    Ok(outcome)
}

/// Uninstall the person's copy of `app`, deciding about its saved data separately: the payload is
/// the publisher's, reproducible by reinstalling; the per-workspace data is the person's and does not
/// come back, so data is never deleted unless the person said so. A pinned package goes only with
/// `force`.
pub fn uninstall(
    target: &Target<'_>,
    app: &str,
    saved_data: SavedData,
    dry_run: bool,
    force: bool,
    said: &mut impl SaysChanges,
) -> Result<(), Error> {
    // Only the person's copy can go; removing it puts the image's back.
    let layers = target
        .catalog
        .layers(app)
        .map_err(|e| Error::RegistryRead(e.to_string()))?;
    let Some(entry) = layers.user.clone() else {
        return Err(match layers.image {
            Some(_) => Error::ImageApp(app.into()),
            None => Error::NotInstalled(app.into()),
        });
    };
    let data_home = data_home_root()?;
    let workspaces = package_store::state::find(&data_home, app);
    let bin = target.store.layout().bin();

    if dry_run {
        said.say(format!("plan: uninstall {} {}", entry.id, entry.version));
        if let Some(image) = &layers.image {
            said.say(format!(
                "  then runs : {} from the system image",
                image.version
            ));
        }
        said.say(format!(
            "  payload   : remove {}",
            entry.payload_root.display()
        ));
        said.say("  registry  : drop the entry");
        if entry.cli_commands.is_empty() {
            said.say("  shims     : none");
        } else {
            said.say(format!(
                "  shims     : remove {} from {}",
                entry.cli_commands.join(", "),
                bin.display()
            ));
        }
        said.say(format!(
            "  content   : evict the blobs no other package uses from {}",
            target.store.layout().content().display()
        ));
        for line in package_store::state::summary(&workspaces) {
            said.say(line);
        }
        said.say(format!(
            "  state     : {}",
            match saved_data {
                SavedData::Delete => "DELETE (--purge)",
                SavedData::Keep => "keep (--keep-state)",
                SavedData::Ask => "you would be asked",
            }
        ));
        said.say("(nothing was removed)");
        return Ok(());
    }

    let removed = target.store.remove(app, force).map_err(store_error)?;
    if !entry.cli_commands.is_empty() {
        shim::remove_shims(&bin, &entry.id, &entry.cli_commands)
            .map_err(|e| Error::Shim(e.to_string()))?;
        said.say(format!(
            "  removed {} PATH shim(s) from {}",
            entry.cli_commands.len(),
            bin.display()
        ));
    }
    said.say(format!("uninstalled: {app} {}", entry.version));
    if let Some(image) = &layers.image {
        said.say(format!(
            "  now runs : {} {} from the system image",
            image.id, image.version
        ));
    }
    said.say(format!(
        "  content  : evicted {} blob(s) no other package uses",
        removed.evicted_blobs
    ));
    if saved_data == SavedData::Delete {
        let (removed, failures) = package_store::state::remove(&workspaces);
        said.say(format!("  state    : deleted in {removed} workspace(s)"));
        for (path, e) in failures {
            said.complain(format!("could not remove {}: {e}", path.display()));
        }
    } else if !workspaces.is_empty() {
        said.say(format!(
            "  state    : kept — {} workspace(s), {} under {}",
            workspaces.len(),
            package_store::state::human_bytes(package_store::state::total_bytes(&workspaces)),
            data_home
                .join(package_store::state::WORKSPACES_DIR)
                .display()
        ));
        said.say("             reinstalling picks it back up; `--purge` deletes it.");
    }
    Ok(())
}

/// Where a change's words go: a report the terminal shows, or nowhere a person reads (a Store
/// removal, whose answer is only whether it worked).
pub trait SaysChanges {
    fn say(&mut self, line: impl Into<String>);
    fn complain(&mut self, warning: impl Into<String>);
}

impl SaysChanges for Said {
    fn say(&mut self, line: impl Into<String>) {
        self.line(line);
    }

    fn complain(&mut self, warning: impl Into<String>) {
        self.warn(warning);
    }
}

/// Words nobody reads: what a change says is logged instead.
pub struct Logged;

impl SaysChanges for Logged {
    fn say(&mut self, _line: impl Into<String>) {}

    fn complain(&mut self, warning: impl Into<String>) {
        tracing::warn!(warning = %warning.into(), "package store change");
    }
}

/// Check `app`'s installed tree, or every installed app's, against the package it was installed
/// from, saying how each stands.
fn verify(target: &Target<'_>, app: Option<&str>, said: &mut Said) -> Result<(), Error> {
    let installed: Vec<InstalledPackage> = match app {
        Some(app) => vec![
            target
                .catalog
                .find(app)
                .map_err(|e| Error::RegistryRead(e.to_string()))?
                .ok_or_else(|| Error::NotInstalled(app.into()))?,
        ],
        None => target
            .catalog
            .list()
            .map_err(|e| Error::RegistryRead(e.to_string()))?,
    };
    if installed.is_empty() {
        said.line("(no packages installed)");
        return Ok(());
    }
    let mut failed: usize = 0;
    for entry in &installed {
        match package_store::integrity::verify(entry, &entry.payload_root) {
            Ok(report) if report.is_intact() => {
                said.line(format!(
                    "ok       {} {}  ({} file(s) checked)",
                    entry.id, entry.version, report.files_checked
                ));
                for extra in &report.unexpected {
                    said.line(format!("         note: not part of the package: {extra}"));
                }
            }
            Ok(report) => {
                failed = failed.saturating_add(1);
                said.line(format!(
                    "FAILED   {} {}  ({} of {} file(s) do not match)",
                    entry.id,
                    entry.version,
                    report.faults.len(),
                    report.files_checked
                ));
                for fault in &report.faults {
                    said.line(format!("         {fault}"));
                }
            }
            Err(e) => {
                failed = failed.saturating_add(1);
                said.line(format!("FAILED   {} {}  {e}", entry.id, entry.version));
            }
        }
        let runtime = entry
            .runtime
            .split_once('/')
            .map_or("", |(_, version)| version);
        if !entry.runtime.is_empty() && !installer::runtime_bundle_present(runtime) {
            said.line(format!(
                "         note: declared runtime {} is not installed, so this package cannot \
                 launch",
                entry.runtime
            ));
        }
    }
    if failed > 0 {
        return Err(Error::VerificationFailed {
            failed,
            checked: installed.len(),
        });
    }
    Ok(())
}

fn runtime_bundles() -> Result<runtime_bundle::gc::BundleRoot, Error> {
    runtime_bundle::gc::BundleRoot::from_env().map_err(|e| Error::RuntimeBundle(e.to_string()))
}

/// Collect after an install, update or uninstall that began at `started` (Unix seconds): reclaim
/// what it left unneeded, keeping every app it wrote, which is about to be opened. Never fails the
/// change it follows, and says what it did as a warning, so the change's own lines stay as scripts
/// read them.
fn collect_after(target: &Target<'_>, started: u64, said: &mut Said) {
    let written = match package_store::gc::written_since(target.store, started) {
        Ok(written) => written,
        Err(error) => {
            said.warn(format!("warning: nothing reclaimed: {error}"));
            return;
        }
    };
    match collect_after_change(target, written) {
        Ok(report) if report.freed_bytes() > 0 || !report.evicted.is_empty() => said.warn(format!(
            "reclaimed {} ({} runtime build(s), {} app(s) evicted)",
            human_bytes(report.freed_bytes()),
            report.runtimes.removed.len(),
            report.evicted.len()
        )),
        Ok(_) => {}
        Err(error) => said.warn(format!("warning: nothing reclaimed: {error}")),
    }
}

/// What runs after every install, update and uninstall, and at every start of the daemon (a new
/// image or a swapped userspace changes the system store without any install running): the store's
/// size cap, payload trees and staging nothing names, and runtime builds no installed app runs any
/// more. A tree a launch holds stays, and so do the apps `keep` names.
///
/// # Errors
///
/// The first step that failed; the steps before it took effect.
pub fn collect_after_change(
    target: &Target<'_>,
    keep: Vec<String>,
) -> Result<package_store::gc::Report, String> {
    let request = package_store::gc::Request::after_change(target.max_store_bytes).keeping(keep);
    match runtime_bundles() {
        Ok(runtimes) => {
            package_store::gc::collect(target.store, target.catalog, &runtimes, request)
        }
        Err(error) => {
            tracing::warn!(%error, "no runtime bundle root; applying the size cap only");
            let keep: Vec<&str> = request.keep.iter().map(String::as_str).collect();
            target
                .store
                .sweep_keeping(target.max_store_bytes, &keep)
                .map(|_| package_store::gc::Report::default())
                .map_err(|error| error.to_string())
        }
    }
}

fn human_bytes(bytes: u64) -> String {
    package_store::state::human_bytes(bytes)
}

fn collect(
    target: &Target<'_>,
    request: package_store::gc::Request,
    said: &mut Said,
) -> Result<(), Error> {
    let dry_run = request.dry_run;
    let max_store_bytes = request.max_store_bytes;
    let report =
        package_store::gc::collect(target.store, target.catalog, &runtime_bundles()?, request)
            .map_err(Error::Gc)?;
    let runtimes = &report.runtimes;
    if runtimes.busy {
        said.line("runtimes: an install is running; collected by the next change");
    }
    for id in &report.evicted {
        said.line(format!(
            "evicted  : {id} (store over {max_store_bytes} bytes)"
        ));
    }
    for path in &report.orphans.removed {
        said.line(format!(
            "removed  : {} (named by no package)",
            path.display()
        ));
    }
    if report.orphans.staging_bytes > 0 {
        said.line(format!(
            "removed  : {} of staging crashed installs left",
            human_bytes(report.orphans.staging_bytes)
        ));
    }
    for (version, bytes) in &runtimes.removed {
        let verb = if dry_run { "would go" } else { "removed " };
        said.line(format!(
            "{verb} : runtime {version} ({})",
            human_bytes(*bytes)
        ));
    }
    for (version, why) in &runtimes.kept {
        let why = match why {
            runtime_bundle::gc::Kept::Referenced => "an installed package runs on it",
            runtime_bundle::gc::Kept::Pinned => "pinned",
            runtime_bundle::gc::Kept::Leased => "installed within the grace period",
            runtime_bundle::gc::Kept::InUse => "a running app has it mounted",
            runtime_bundle::gc::Kept::Launching => "an app launched on it holds it",
        };
        said.line(format!("kept     : runtime {version} ({why})"));
    }
    let verb = if dry_run { "would free" } else { "freed" };
    said.line(format!("{verb}: {}", human_bytes(report.freed_bytes())));
    Ok(())
}

/// The images the person's store at `root` holds, with the system stores' beside them and the
/// publishers the device trusts for images.
///
/// # Errors
///
/// An unreadable image trust file.
pub fn image_store(root: &Path, trust: &Trust) -> Result<package_store::Store, Error> {
    let mut store = package_store::Store::new(PackageStore::at(root));
    let system = package_store::SystemStore::from_env();
    if let Some(system) = &system {
        store = store
            .with_system(package_store::Layout::new(&system.data_dir))
            .with_system(package_store::Layout::new(&system.payload_dir));
    }
    let trust = package_store::ImageTrust::from_env(system.as_ref())
        .map_err(store_error)?
        .with_attestors(&trust.attestors)
        .with_revoked(RevocationList {
            revoked: trust.revoked.clone(),
        });
    Ok(store.with_trust(trust))
}

fn add_image(
    target: &Target<'_>,
    tapp: &Path,
    source: &str,
    publisher: Option<&str>,
    payload_from: Option<String>,
    trust_flags: &Trust,
    said: &mut Said,
) -> Result<(), Error> {
    let store = image_store(target.root(), trust_flags)?;
    let mut trust = store.trust().clone();
    if let Some(publisher) = publisher {
        trust = trust
            .with_publisher(decode_hex_32(publisher).map_err(Error::InvalidRequiredPublisher)?);
    }
    let payload = payload_from.map(app_registry::HttpBlobs::new);
    let file = std::io::BufReader::new(fs::File::open(tapp)?);
    let meta = store
        .user()
        .install_image(
            file,
            |meta| {
                let attested_by = trust.check(meta).map_err(|error| error.to_string())?;
                Ok(Admission {
                    pinned: false,
                    attested_by,
                    source: source.to_owned(),
                    held: false,
                })
            },
            payload.as_ref().map(|fetcher| fetcher as &dyn BlobFetcher),
        )
        .map_err(store_error)?;
    said.line(format!(
        "{} {} {} {}",
        meta.id,
        meta.manifest.package.kind,
        meta.manifest.package.id,
        meta.manifest.package.version
    ));
    Ok(())
}

fn install_runtime(
    version: &str,
    from: Option<&Path>,
    arch: Option<&str>,
    identity: Option<&Path>,
    force: bool,
    said: &mut Said,
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
    let manifest = runtime_bundle::build_bundle_with(
        &dest,
        version,
        &runtime_bundle::BuildOptions {
            from,
            arch,
            identity,
            extra_libs: &[],
            extra_bins: &[],
        },
    )
    .map_err(|e| Error::RuntimeBundle(e.to_string()))?;
    said.line(format!("installed runtime bundle: tilo-runtime/{version}"));
    said.line(format!("  path     : {}", dest.display()));
    said.line(format!("  arch     : {}", manifest.arch));
    said.line(format!("  files    : {}", manifest.files.len()));
    said.line(format!("  root-hash: blake3:{}", manifest.root_hash));
    if let Some(pk) = &manifest.publisher_hex {
        said.line(format!("  publisher: ed25519:{pk}"));
    }
    let mut report = installer::Report::default();
    installer::seal_runtime_bundle(&dest, &manifest, &mut report);
    said.take(report);
    Ok(())
}

fn remove_runtime(version: &str, said: &mut Said) -> Result<(), Error> {
    let bundles = runtime_bundles()?;
    let _collection_held_off = bundles.install_lock()?;
    bundles.unpin(version)?;
    // Out of the version's directory in one rename first: a removal stopped halfway would otherwise
    // leave a bundle a launch picks.
    let trashed = bundles
        .retire(version)?
        .ok_or_else(|| Error::RuntimeBundleNotInstalled(version.to_string()))?;
    if bundles.purge(&trashed, &package_store::gc::in_use_now())? {
        said.line(format!("removed runtime bundle: tilo-runtime/{version}"));
    } else {
        said.line(format!(
            "removed runtime bundle: tilo-runtime/{version}; a running app still has it \
             mounted, so its files go once that app closes (`tilo-install gc`)"
        ));
    }
    Ok(())
}

fn pin_runtime(version: &str, pinned: bool, said: &mut Said) -> Result<(), Error> {
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
        said.line(format!("pinned runtime bundle: tilo-runtime/{version}"));
    } else {
        bundles.unpin(version)?;
        said.line(format!("unpinned runtime bundle: tilo-runtime/{version}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tilo_actions::remote::Caller;

    /// A development build is signed with the device's own key: only the person on that device's
    /// own daemon connection may ask for one.
    #[test]
    fn only_the_person_on_this_devices_connection_installs_development_builds() {
        for personal in [true, false] {
            assert!(
                super::dev_build_admitted(&Caller::own_person(String::from("person")), personal)
                    .is_ok(),
                "the person on their own device's connection installs one"
            );
            for refused in [
                Caller::authenticated([7; 32], [8; 32]),
                Caller::visitor([9; 32]),
                Caller::local_person(String::from("person")),
                Caller::local_agent(String::from("an-agent")),
            ] {
                assert!(
                    super::dev_build_admitted(&refused, personal).is_err(),
                    "a key, a visitor, a confined device's person or an agent asks for no \
                     development build: {refused:?}"
                );
            }
        }
        // The system daemon serves accounts it is no person for, and its system caller is not
        // one: only a daemon this computer's own account runs answers its system caller for the
        // device.
        assert!(
            super::dev_build_admitted(&Caller::local_system(), false).is_err(),
            "the system daemon installs no development build for an account"
        );
        assert!(
            super::dev_build_admitted(&Caller::local_system(), true).is_ok(),
            "this computer's own daemon answers its account as the device"
        );
    }
}
