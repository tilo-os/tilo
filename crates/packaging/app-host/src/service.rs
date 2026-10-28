//! Cap'n Proto `AppHost::Server` implementation. Wires the RPC methods to
//! the device's [`PackageStore`] and its [`Catalog`]. The transport (Unix
//! socket listener) lives in `transport.rs`.
//!
//! capnp 0.26 generates async server methods on `capnp::capability::Rc<Self>`
//! returning an `impl Future<Output = Result<(), capnp::Error>>`; blocking
//! work (fs IO, content-store sessions, materialisation) is routed through
//! `tokio::task::spawn_blocking` so the capnp event loop stays responsive.

use std::{io::Write as _, sync::Arc};

use capnp::Error;

use crate::app_host_capnp::{
    CatalogueRefusal as WireCatalogueRefusal, FlatpakAction as WireFlatpakAction,
    Pinning as WirePinning, RemoteAction as WireRemoteAction, app_host,
};
use crate::client::Refusal;
use crate::install::{InstallExpectation, install_verified_tapp};
use package_store::{Catalog, InstalledPackage, PackageStore, Pinning};
use tilo_app_package::id::PackageId;

/// Service state shared across every accepted connection.
/// What `tilo-install` runs after every install, update and uninstall, run
/// after this daemon's own, and at every start of the daemon (a new image or a
/// swapped userspace changes the system store without any install running): the store's size cap, payload trees and staging
/// nothing names, and the runtime builds nothing installed runs on any
/// more. A tree a launch holds or a running app has mounted stays, and so do
/// the apps `keep` names: what the change just installed, which the person
/// is about to open. Never fails the change it follows.
pub fn collect_after_change(
    store: &PackageStore,
    catalog: &Catalog,
    max_store_bytes: u64,
    keep: &[&str],
) {
    let runtimes = match runtime_bundle::gc::BundleRoot::from_env() {
        Ok(runtimes) => runtimes,
        Err(error) => {
            tracing::warn!(%error, "no runtime bundle root; applying the size cap only");
            if let Err(error) = store.sweep_keeping(max_store_bytes, keep) {
                tracing::warn!(%error, "post-change sweep failed");
            }
            return;
        }
    };
    let request = ::install::gc::Request::after_change(max_store_bytes)
        .keeping(keep.iter().map(|id| (*id).to_owned()));
    match ::install::gc::collect(store, catalog, &runtimes, request) {
        Ok(report) => tracing::debug!(
            freed_bytes = report.freed_bytes(),
            evicted = report.evicted.len(),
            runtimes = report.runtimes.removed.len(),
            "collected after a change"
        ),
        Err(error) => tracing::warn!(%error, "collection after a change failed"),
    }
}

pub struct AppHostServer {
    store: Arc<PackageStore>,
    catalog: Arc<Catalog>,
    max_store_bytes: u64,
    // One runtime operation per issuing connection, including after disconnect
    // while a non-cancellable blocking installer is completing.
    runtime_gate: Arc<tokio::sync::Semaphore>,
}

impl AppHostServer {
    /// Build a new server backed by `store`, listing what `catalog` (the
    /// same user store plus the image's preinstalled packages) can run.
    /// `max_store_bytes` is the soft cap used by the post-install sweep;
    /// pass `0` to disable it.
    pub fn new(store: Arc<PackageStore>, catalog: Arc<Catalog>, max_store_bytes: u64) -> Self {
        Self {
            store,
            catalog,
            max_store_bytes,
            runtime_gate: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }
}

fn wire_pinning(p: Pinning) -> WirePinning {
    match p {
        Pinning::Tried => WirePinning::Tried,
        Pinning::Pinned => WirePinning::Pinned,
    }
}

fn parse_pinning(p: WirePinning) -> Pinning {
    match p {
        WirePinning::Tried => Pinning::Tried,
        WirePinning::Pinned => Pinning::Pinned,
    }
}

/// Decode raw 32-byte `Data` id into a [`PackageId`].
fn parse_id(bytes: &[u8]) -> Result<PackageId, Error> {
    let raw: [u8; 32] = bytes
        .try_into()
        .map_err(|_| Error::failed(format!("package id must be 32 bytes, got {}", bytes.len())))?;
    Ok(PackageId::from_bytes(raw))
}

fn failed(error: impl std::fmt::Display) -> Error {
    Error::failed(error.to_string())
}

/// A visible package with what the wire reports about it.
struct Listed {
    package: InstalledPackage,
    id: PackageId,
    manifest_text: String,
    size_bytes: u64,
}

fn listed(package: InstalledPackage) -> Option<Listed> {
    // A row without a signed package root has no identity to hand out.
    let id = package.package_id()?;
    let manifest_text = std::fs::read_to_string(&package.manifest_path).ok()?;
    let size_bytes = payload_bytes(&package);
    Some(Listed {
        package,
        id,
        manifest_text,
        size_bytes,
    })
}

/// Bytes of `package`'s payload tree: the size recorded at install, or —
/// for a row written before sizes were recorded — the tree measured on disk.
fn payload_bytes(package: &InstalledPackage) -> u64 {
    if package.payload_bytes > 0 {
        package.payload_bytes
    } else {
        package_store::tree_size(&package.payload_root)
    }
}

/// The visible package with signed root `id`.
fn find(catalog: &Catalog, id: PackageId) -> Result<InstalledPackage, Error> {
    catalog
        .find_package(id)
        .map_err(failed)?
        .ok_or_else(|| Error::failed(format!("unknown package {id}")))
}

/// The user-managed package with signed root `id`; system packages cannot
/// be pinned or removed here.
fn find_user(catalog: &Catalog, id: PackageId) -> Result<InstalledPackage, Error> {
    let package = find(catalog, id)?;
    if !package.user_managed() {
        return Err(Error::failed(format!(
            "{} is a system package and cannot be changed here",
            package.id
        )));
    }
    Ok(package)
}

/// Populate a wire `PackageEntry` builder from a listed package.
fn fill_entry(mut out: crate::app_host_capnp::package_entry::Builder<'_>, row: &Listed) {
    out.set_id(row.id.as_bytes());
    out.set_pinning(wire_pinning(row.package.pinning()));
    out.set_payload_root(row.package.payload_root.to_string_lossy());
    out.set_manifest_text(&row.manifest_text);
    out.set_size_bytes(row.size_bytes);
    out.set_last_used_unix(row.package.last_used);
    out.set_user_managed(row.package.user_managed());
    out.set_manifest_path(row.package.manifest_path.to_string_lossy());
}

/// Run blocking store work off the capnp event loop.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| Error::failed(format!("join: {e}")))?
}

/// The override the build's gate let through, else the image default.
fn path_or_default(
    allowed_override: Option<std::ffi::OsString>,
    default: &str,
) -> std::path::PathBuf {
    allowed_override.map_or_else(
        || std::path::PathBuf::from(default),
        std::path::PathBuf::from,
    )
}

/// Names the `tilo-launcher` binary instead of `/usr/libexec/tilo/tilo-launcher`.
const LAUNCHER_BIN_ENV: &str = "TILO_LAUNCHER_BIN";
/// Names the `app` binary instead of `/usr/bin/app`.
const APP_BIN_ENV: &str = "TILO_APP_BIN";

/// The launcher that builds every app's sandbox. [`LAUNCHER_BIN_ENV`]
/// re-points it in developer builds only.
fn launcher_bin() -> std::path::PathBuf {
    path_or_default(
        package_store::dev_env::dev_override(LAUNCHER_BIN_ENV),
        "/usr/libexec/tilo/tilo-launcher",
    )
}

fn app_bin() -> std::path::PathBuf {
    path_or_default(
        package_store::dev_env::dev_override(APP_BIN_ENV),
        "/usr/bin/app",
    )
}

fn require_offer_identity(
    offer: &app_cli::CatalogueApp,
    expected: &InstallExpectation,
) -> Result<(), Error> {
    let same_app = offer.package_id == expected.app_id;
    if !same_app
        || offer.version != expected.version
        || offer.tapp_root_hash_hex != expected.package_root.to_hex()
        || !offer
            .publisher_pubkey_hex
            .eq_ignore_ascii_case(&hex::encode(expected.publisher))
        || offer.tapp_size_bytes != expected.size_bytes
    {
        return Err(Error::failed(
            "The app catalogue changed; refresh Store before opening this version.".into(),
        ));
    }
    Ok(())
}

/// The version a first open installs instead of `requested`: when
/// `requested` names a runtime this device lacks, the newest offer of the
/// same signed entry, no newer than `requested`, whose runtime is present.
fn first_open_choice<'a>(
    requested: &'a app_cli::CatalogueApp,
    offers: &'a [app_cli::CatalogueApp],
) -> &'a app_cli::CatalogueApp {
    if requested
        .runtime
        .as_deref()
        .is_none_or(app_cli::runtime_installed)
    {
        return requested;
    }
    let same_entry = offers.iter().filter(|offer| {
        offer.package_id == requested.package_id
            && offer
                .publisher_pubkey_hex
                .eq_ignore_ascii_case(&requested.publisher_pubkey_hex)
    });
    app_cli::prefer_installed_runtime(same_entry, &requested.version, app_cli::runtime_installed)
        .unwrap_or(requested)
}

fn install_offer_from_catalogue(
    catalog: &Catalog,
    registry: &str,
    expected: &InstallExpectation,
) -> Result<PackageId, Error> {
    let offers = app_cli::version_offers(&expected.app_id, registry).map_err(failed)?;
    let offer = offers
        .iter()
        .find(|offer| {
            offer.version == expected.version
                && offer.tapp_root_hash_hex == expected.package_root.to_hex()
        })
        .ok_or_else(|| {
            Error::failed(
                "This app version is no longer offered for this device; refresh Store.".into(),
            )
        })?;
    require_offer_identity(offer, expected)?;
    // The shared CLI pipeline fetches missing content-addressed blobs, verifies
    // exact archive/root/publisher identities and provisions the signed runtime
    // before replacing an app. Store must not implement another installer.
    let installed = catalog
        .list()
        .map_err(failed)?
        .iter()
        .any(|package| package.id == expected.app_id);
    let installed_root = if installed {
        app_cli::install::update_app_reporting(offer, &mut |_| {}).map_err(failed)?;
        expected.package_root
    } else {
        // A first open takes the newest signed version of this same entry
        // that runs on a runtime the device already has, rather than
        // downloading a whole runtime for a few-megabyte app. Updating moves
        // it to the newest version later.
        let offer = first_open_choice(offer, &offers);
        app_cli::install::install_app(offer).map_err(failed)?;
        format!("tapp:{}", offer.tapp_root_hash_hex)
            .parse::<PackageId>()
            .map_err(|_| {
                Error::failed("the installed version has an invalid package root".into())
            })?
    };
    let package = find(catalog, installed_root)?;
    package
        .package_id()
        .ok_or_else(|| Error::failed("installed app has no package identity".into()))
}

fn flatpak_status_from(binary: &std::path::Path) -> Result<crate::client::FlatpakStatus, Error> {
    let probe = |args: &[&str]| {
        let mut command = std::process::Command::new(binary);
        command.args(args);
        bounded_command::run_prepared(
            command,
            None,
            1024 * 1024,
            std::time::Duration::from_secs(5),
        )
        .map_err(failed)
        .and_then(|output| {
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).into_owned())
            } else {
                Err(Error::failed(format!(
                    "Flatpak query failed: {}",
                    safe_host_diagnostic(&output.stderr)
                )))
            }
        })
    };
    // Missing Flatpak is a supported host state. Failure to read an existing
    // installation is an error, rather than pretending it has no apps/remotes.
    if !binary.exists() {
        return Ok(crate::client::FlatpakStatus::default());
    }
    probe(&["--version"])?;
    Ok(crate::client::FlatpakStatus {
        available: true,
        installed: probe(&[
            "list",
            "--app",
            "--columns=application,version,origin,installation",
        ])?,
        remotes: probe(&["remotes", "--columns=name,url,title,options"])?,
    })
}

fn require_flatpak_selector(value: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.len() > 255
        || value.starts_with('-')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(Error::failed(
            "invalid Flatpak repository or application identity".into(),
        ));
    }
    Ok(())
}

/// A catalogue read that gave no rows: refused for a reason the Store
/// explains (carried typed on the wire), or failed.
#[derive(Debug)]
enum ReadError {
    Refused(Refusal, String),
    Failed(Error),
}

impl From<Error> for ReadError {
    fn from(error: Error) -> Self {
        Self::Failed(error)
    }
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refusal, detail) => write!(f, "{refusal:?}: {detail}"),
            Self::Failed(error) => error.fmt(f),
        }
    }
}

/// Write a catalogue read's outcome into its results: the rows, or the
/// refusal with its detail; anything else is raised.
fn set_catalogue(
    read: Result<String, ReadError>,
    set: impl FnOnce(&str, WireCatalogueRefusal, &str),
) -> Result<(), Error> {
    match read {
        Ok(rows) => {
            set(&rows, WireCatalogueRefusal::None, "");
            Ok(())
        }
        Err(ReadError::Refused(refusal, detail)) => {
            set("", refusal.to_wire(), &detail);
            Ok(())
        }
        Err(ReadError::Failed(error)) => Err(error),
    }
}

fn flatpak_catalogue_from(
    binary: &std::path::Path,
    remote: &str,
    user_managed: bool,
) -> Result<String, ReadError> {
    flatpak_catalogue_with_timeout(
        binary,
        remote,
        user_managed,
        std::time::Duration::from_secs(8),
    )
}

fn flatpak_catalogue_with_timeout(
    binary: &std::path::Path,
    remote: &str,
    user_managed: bool,
    timeout: std::time::Duration,
) -> Result<String, ReadError> {
    require_flatpak_selector(remote)?;
    let started = std::time::Instant::now();
    let cached = flatpak_catalogue_read(binary, remote, user_managed, true, timeout)?;
    let missing = format!("No cached summary for remote '{remote}'");
    let output =
        if !cached.status.success() && String::from_utf8_lossy(&cached.stderr).contains(&missing) {
            // A newly configured remote may not have a summary yet. Flatpak's normal
            // fetch verifies it with the configured GPG policy; never install apps or
            // change remote settings here. Both reads share the same bounded deadline.
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(ReadError::Refused(
                    Refusal::TimedOut,
                    "Flatpak repository query timed out".into(),
                ));
            }
            flatpak_catalogue_read(binary, remote, user_managed, false, remaining)?
        } else {
            cached
        };
    if !output.status.success() {
        let diagnostic = safe_host_diagnostic(&output.stderr);
        // Flatpak names a device-provided OCI repository without a summary
        // only in its diagnostic; this is the one place that reads it, and
        // the Store gets the refusal, not the text.
        if !user_managed && diagnostic.contains("GenerateOciSummary") {
            return Err(ReadError::Refused(
                Refusal::DeviceRepositoryStale,
                format!("Flatpak repository query failed: {diagnostic}"),
            ));
        }
        return Err(ReadError::Failed(Error::failed(format!(
            "Flatpak repository query failed: {diagnostic}"
        ))));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn flatpak_catalogue_read(
    binary: &std::path::Path,
    remote: &str,
    user_managed: bool,
    cached: bool,
    timeout: std::time::Duration,
) -> Result<bounded_command::Output, ReadError> {
    let mut command = std::process::Command::new(binary);
    // Classify only Flatpak's explicit cache absence, independently of the user's
    // locale. No diagnostic text is used as catalogue or authority data.
    command.env("LC_ALL", "C").arg("remote-ls");
    if cached {
        command.arg("--cached");
    }
    command.args([
        if user_managed { "--user" } else { "--system" },
        "--app",
        "--arch",
        std::env::consts::ARCH,
        "--columns=application,name,description,version,branch,origin",
        "--",
        remote,
    ]);
    bounded_command::run_prepared(command, None, 4 * 1024 * 1024, timeout).map_err(|error| {
        match error {
            bounded_command::Error::Timeout(_) => ReadError::Refused(
                Refusal::TimedOut,
                format!("Flatpak repository query: {error}"),
            ),
            other => ReadError::Failed(failed(other)),
        }
    })
}

fn flatpak_remote_enabled_from(
    binary: &std::path::Path,
    name: &str,
    enabled: bool,
) -> Result<String, Error> {
    require_flatpak_selector(name)?;
    let mut command = std::process::Command::new(binary);
    command.args([
        "remote-modify",
        "--user",
        if enabled { "--enable" } else { "--disable" },
        "--",
        name,
    ]);
    run_host_command(command)
}

/// Update or uninstall (`verb`) the account's `app_id`.
fn flatpak_app_from(binary: &std::path::Path, verb: &str, app_id: &str) -> Result<String, Error> {
    require_flatpak_selector(app_id)?;
    let mut command = std::process::Command::new(binary);
    command.args([verb, "--user", "--noninteractive", "--", app_id]);
    run_host_command(command)
}

/// Uninstall the account's `app_id`, keeping its data (`~/.var/app/<id>`), as
/// Flatpak does, then the runtimes no app of the account uses any more:
/// nothing else removes them from the person's installation, so each removed
/// app would leave its runtime behind for good. Flatpak decides what is
/// unused and keeps every runtime an installed app needs. The app is removed
/// whether or not that cleanup succeeds.
fn flatpak_remove_from(binary: &std::path::Path, app_id: &str) -> Result<String, Error> {
    let status = flatpak_app_from(binary, "uninstall", app_id)?;
    let mut unused = std::process::Command::new(binary);
    unused.args(["uninstall", "--user", "--unused", "--noninteractive"]);
    if let Err(error) = run_host_command(unused) {
        tracing::warn!(app = %app_id, %error, "could not remove runtimes no app uses");
    }
    Ok(status)
}

/// Add the account source `name` at `url`, unless one of that name exists.
fn flatpak_remote_add_from(
    binary: &std::path::Path,
    name: &str,
    url: &str,
) -> Result<String, Error> {
    require_flatpak_selector(name)?;
    let mut command = std::process::Command::new(binary);
    command.args(["remote-add", "--user", "--if-not-exists", "--", name, url]);
    run_host_command(command)
}

fn flatpak_remote_delete_from(binary: &std::path::Path, name: &str) -> Result<String, Error> {
    require_flatpak_selector(name)?;
    let mut command = std::process::Command::new(binary);
    command.args(["remote-delete", "--user", "--force", "--", name]);
    run_host_command(command)
}

fn flatpak_install_remote_from(
    binary: &std::path::Path,
    remote: &str,
    app_id: &str,
    branch: &str,
) -> Result<String, Error> {
    require_flatpak_selector(remote)?;
    require_flatpak_selector(app_id)?;
    require_flatpak_selector(branch)?;
    let mut command = std::process::Command::new(binary);
    command.args([
        "install",
        "--user",
        "--noninteractive",
        "--",
        remote,
        &format!("{app_id}//{branch}"),
    ]);
    run_host_command(command)
}

/// Install from the configured official source into this person's installation.
/// An account source takes precedence over the immutable device default.
fn flatpak_install_flathub_from(binary: &std::path::Path, app_id: &str) -> Result<String, Error> {
    require_flatpak_selector(app_id)?;
    let mut query = std::process::Command::new(binary);
    query.args(["remotes", "--columns=name,url,title,options"]);
    let output =
        bounded_command::run_prepared(query, None, 1024 * 1024, std::time::Duration::from_secs(5))
            .map_err(failed)?;
    if !output.status.success() {
        return Err(Error::failed(format!(
            "Could not read configured Flatpak sources: {}",
            safe_host_diagnostic(&output.stderr)
        )));
    }
    require_enabled_flathub(&String::from_utf8_lossy(&output.stdout))?;
    let mut remote = std::process::Command::new(binary);
    remote.args([
        "remote-add",
        "--user",
        "--if-not-exists",
        "flathub",
        "https://dl.flathub.org/repo/flathub.flatpakrepo",
    ]);
    run_host_command(remote)?;
    let mut install = std::process::Command::new(binary);
    install.args([
        "install",
        "--user",
        "--noninteractive",
        "--",
        "flathub",
        app_id,
    ]);
    run_host_command(install)
}

fn require_enabled_flathub(rows: &str) -> Result<(), Error> {
    let mut device_enabled = false;
    for row in rows.lines() {
        let columns: Vec<_> = row.split('\t').collect();
        if columns.first().copied() != Some("flathub") {
            continue;
        }
        let url = columns.get(1).copied().unwrap_or("").trim_end_matches('/');
        let options: Vec<_> = columns
            .get(3)
            .copied()
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .collect();
        let official = matches!(
            url,
            "https://dl.flathub.org/repo" | "https://flathub.org/repo"
        );
        let enabled = !options.contains(&"disabled") && !options.contains(&"no-gpg-verify");
        if !options.contains(&"system") {
            return if official && enabled {
                Ok(())
            } else {
                Err(Error::failed("The account Flathub source must be enabled, verify signatures and use the official repository.".into()))
            };
        }
        device_enabled |= official && enabled;
    }
    if device_enabled {
        Ok(())
    } else {
        Err(Error::failed(
            "No enabled official Flathub source is configured.".into(),
        ))
    }
}

/// The person's app repositories, as `app repo list --json` prints them.
/// The Store reads them here because its sandbox has a home of its own: a
/// copy there would be a second list `app repo` never edits.
fn repositories_json() -> Result<String, Error> {
    repositories_json_from(&app_bin())
}

fn repositories_json_from(app: &std::path::Path) -> Result<String, Error> {
    let mut command = std::process::Command::new(app);
    command.args(["repo", "list", "--json"]);
    let output = bounded_command::run_prepared(
        command,
        None,
        256 * 1024,
        std::time::Duration::from_secs(30),
    )
    .map_err(|error| Error::failed(format!("app repo list failed: {error}")))?;
    if !output.status.success() {
        return Err(Error::failed(format!(
            "app repo list exited {}: {}",
            output.status.code().unwrap_or(1),
            safe_host_diagnostic(&output.stderr)
        )));
    }
    String::from_utf8(output.stdout)
        .map_err(|_| Error::failed("app repo list printed invalid UTF-8".to_owned()))
}

const MAX_REGISTRY_CATALOGUE_BYTES: usize = 4 * 1024 * 1024;

fn registry_catalogue(registry: &str) -> Result<String, ReadError> {
    system_declaration_readable(&package_store::interfaces::system_interfaces_path())?;
    let configured = app_cli::config::load_registries();
    registry_catalogue_from(&configured, registry, app_cli::client::fetch_entries)
}

/// The catalogue last accepted from `registry`, without the network, under
/// the same configuration and size rules as a fresh one.
fn last_verified_catalogue(registry: &str) -> Result<String, ReadError> {
    system_declaration_readable(&package_store::interfaces::system_interfaces_path())?;
    let configured = app_cli::config::load_registries();
    registry_catalogue_from(
        &configured,
        registry,
        app_cli::client::last_verified_entries,
    )
}

/// Refuse to read any repository while the system's interface declaration
/// at `path` cannot be read ([`Refusal::SystemUpdateNeeded`]).
///
/// Which builds of an app run here is decided against that declaration.
/// Unreadable, it is written for another version of the system than this
/// app host (a component swapped over an older image), so the Store must
/// say that the system needs an update, not list apps judged against
/// nothing or show the repository as unavailable.
fn system_declaration_readable(path: &std::path::Path) -> Result<(), ReadError> {
    match package_store::interfaces::read_interfaces(path) {
        Ok(_) => Ok(()),
        Err(error) => {
            tracing::error!(%error, "the system's interface declaration cannot be read; repositories are refused until the system is updated");
            Err(ReadError::Refused(
                Refusal::SystemUpdateNeeded,
                error.to_string(),
            ))
        }
    }
}

fn registry_catalogue_from(
    configured: &app_cli::config::RegistriesFile,
    name: &str,
    fetch: impl FnOnce(
        &app_cli::config::RegistryConfig,
    )
        -> Result<Vec<app_cli::client::VerifiedEntry>, app_cli::client::RegistryError>,
) -> Result<String, ReadError> {
    if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err(Error::failed("invalid repository name".into()).into());
    }
    let registry = configured
        .registries
        .iter()
        .find(|registry| registry.name == name && registry.enabled)
        .ok_or_else(|| Error::failed("repository is not configured and enabled".into()))?;
    let entries = fetch(registry).map_err(|error| match error {
        app_cli::client::RegistryError::Expired { .. } => {
            ReadError::Refused(Refusal::Expired, error.to_string())
        }
        other => ReadError::Failed(failed(other)),
    })?;
    let json = serde_json::to_string(&entries).map_err(failed)?;
    if json.len() > MAX_REGISTRY_CATALOGUE_BYTES {
        return Err(Error::failed(
            "verified repository catalogue exceeds the response limit".into(),
        )
        .into());
    }
    Ok(json)
}

fn wayland_socket() -> Option<std::path::PathBuf> {
    let display = std::env::var_os("WAYLAND_DISPLAY")?;
    let display = std::path::PathBuf::from(display);
    if display.is_absolute() {
        Some(display)
    } else {
        tilo_paths::xdg::runtime_dir().map(|root| root.join(display))
    }
}

/// Start `package` through the host-owned launcher, on `uri` when given.
pub(crate) fn launch_package(package: &InstalledPackage, uri: Option<&str>) -> Result<(), String> {
    let mut command = std::process::Command::new(launcher_bin());
    command
        .arg("run")
        .arg("--manifest")
        .arg(&package.manifest_path)
        .arg("--payload-root")
        .arg(&package.payload_root)
        .arg("--app-id")
        .arg(&package.id);
    if let Some(socket) = wayland_socket() {
        command.arg("--wayland-socket").arg(socket);
    }
    if let Some(uri) = uri {
        command.arg("--open-uri").arg(uri);
    }
    spawn_detached(command)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn spawn_detached(mut command: std::process::Command) -> Result<String, Error> {
    command
        .spawn()
        .map(|child| format!("started process {}", child.id()))
        .map_err(|error| Error::failed(format!("launch failed: {error}")))
}

pub(crate) fn safe_host_diagnostic(bytes: &[u8]) -> String {
    const MAX_DIAGNOSTIC_BYTES: usize = 4096;
    // Invalid UTF-8 expands to a three-byte replacement character. Limit the
    // source window accordingly so even adversarial bytes stay under the RPC
    // response ceiling after lossy decoding.
    const MAX_SOURCE_BYTES: usize = MAX_DIAGNOSTIC_BYTES / 3;
    let start = bytes.len().saturating_sub(MAX_SOURCE_BYTES);
    String::from_utf8_lossy(bytes.get(start..).unwrap_or_default())
        .chars()
        .map(|character| {
            if character.is_control() && !matches!(character, '\n' | '\t') {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn run_host_command(command: std::process::Command) -> Result<String, Error> {
    let output = bounded_command::run_prepared(
        command,
        None,
        1024 * 1024,
        std::time::Duration::from_secs(1800),
    )
    .map_err(|error| Error::failed(format!("host command failed: {error}")))?;
    let mut text = safe_host_diagnostic(&output.stdout);
    if text.is_empty() {
        text = safe_host_diagnostic(&output.stderr);
    }
    if output.status.success() {
        Ok(text)
    } else {
        Err(Error::failed(format!(
            "host command exited {}: {text}",
            output.status.code().unwrap_or(1)
        )))
    }
}

// capnp-rpc capability methods are driven from a single-threaded
// `LocalSet`: `capnp::capability::Rc` (captured as `self`) is a
// non-atomic, non-`Send` reference count by capnp's own design, so
// these futures can never be `Send` and are never meant to cross a
// thread. Forcing `Send` here would use a different threading model.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc Server methods run on a single-threaded LocalSet via the non-atomic capnp::capability::Rc; the futures are never sent across threads"
)]
impl app_host::Server for AppHostServer {
    async fn inspect_optional_runtime(
        self: capnp::capability::Rc<Self>,
        params: app_host::InspectOptionalRuntimeParams,
        mut results: app_host::InspectOptionalRuntimeResults,
    ) -> Result<(), Error> {
        let p = params.get()?;
        let app = p.get_app_id()?.to_str()?;
        let root = parse_id(p.get_consumer_root()?)?;
        let runtime = p.get_runtime()?.to_str()?;
        if app.len() > 256 || runtime.len() > 256 {
            return Err(failed("runtime request exceeds input limit"));
        }
        let app = app.to_owned();
        let runtime = runtime.to_owned();
        let permit = Arc::clone(&self.runtime_gate)
            .try_acquire_owned()
            .map_err(|_| failed("a runtime operation is already active on this connection"))?;
        let catalog = Arc::clone(&self.catalog);
        let review = blocking(move || {
            let _permit = permit;
            crate::runtime_transaction::inspect(&catalog, &app, root, &runtime).map_err(failed)
        })
        .await?;
        results
            .get()
            .set_review_json(serde_json::to_string(&review).map_err(failed)?);
        Ok(())
    }

    async fn acquire_optional_runtime(
        self: capnp::capability::Rc<Self>,
        params: app_host::AcquireOptionalRuntimeParams,
        _results: app_host::AcquireOptionalRuntimeResults,
    ) -> Result<(), Error> {
        let p = params.get()?;
        let json = p.get_review_json()?.to_str()?;
        if json.len() > 16 * 1024 {
            return Err(failed("runtime review exceeds input limit"));
        }
        let review = serde_json::from_str(json).map_err(failed)?;
        let mut observer = if p.has_progress() {
            Some(p.get_progress()?)
        } else {
            None
        };
        let permit = Arc::clone(&self.runtime_gate)
            .try_acquire_owned()
            .map_err(|_| failed("a runtime operation is already active on this connection"))?;
        let catalog = Arc::clone(&self.catalog);
        let (sender, mut receiver) =
            tokio::sync::watch::channel(crate::runtime_transaction::RuntimeProgress::default());
        let work = blocking(move || {
            let _permit = permit;
            crate::runtime_transaction::acquire(&catalog, &review, &mut |progress| {
                sender.send_replace(crate::runtime_transaction::RuntimeProgress {
                    installing: matches!(
                        progress.phase,
                        app_cli::install::InstallPhase::Installing { .. }
                    ),
                    received: progress.received,
                    total: progress.total,
                });
            })
            .map_err(failed)
        });
        tokio::pin!(work);
        loop {
            tokio::select! {
                biased;
                changed = receiver.changed() => {
                    if changed.is_err() { return work.await; }
                    let latest = *receiver.borrow_and_update();
                    if let Some(client) = &observer {
                        let mut request = client.update_request();
                        let mut output = request.get();
                        output.set_installing(latest.installing);
                        output.set_received(latest.received);
                        output.set_total(latest.total.unwrap_or_default());
                        output.set_total_known(latest.total.is_some());
                        // A stalled/disconnected observer cannot block the installer
                        // or build an unbounded notification queue.
                        let reported = tokio::time::timeout(
                            std::time::Duration::from_secs(2), request.send().promise,
                        ).await;
                        if !matches!(reported, Ok(Ok(_))) { observer = None; }
                    }
                }
                result = &mut work => return result,
            }
        }
    }

    fn list(
        self: capnp::capability::Rc<Self>,
        _params: app_host::ListParams,
        mut results: app_host::ListResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let catalog = Arc::clone(&self.catalog);
        async move {
            let rows = blocking(move || {
                Ok(catalog
                    .list()
                    .map_err(failed)?
                    .into_iter()
                    .filter_map(listed)
                    .collect::<Vec<_>>())
            })
            .await?;
            let count = u32::try_from(rows.len())
                .map_err(|_| Error::failed("too many packages to list".to_string()))?;
            let mut list = results.get().init_entries(count);
            for (index, row) in (0..count).zip(rows.iter()) {
                fill_entry(list.reborrow().get(index), row);
            }
            Ok(())
        }
    }

    fn resolve(
        self: capnp::capability::Rc<Self>,
        params: app_host::ResolveParams,
        mut results: app_host::ResolveResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let catalog = Arc::clone(&self.catalog);
        async move {
            let id = parse_id(params.get().map_err(failed)?.get_id().map_err(failed)?)?;
            let row = blocking(move || {
                listed(find(&catalog, id)?)
                    .ok_or_else(|| Error::failed(format!("package {id} has no readable manifest")))
            })
            .await?;
            fill_entry(results.get().init_entry(), &row);
            Ok(())
        }
    }

    fn pin(
        self: capnp::capability::Rc<Self>,
        params: app_host::PinParams,
        _results: app_host::PinResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let (store, catalog) = (Arc::clone(&self.store), Arc::clone(&self.catalog));
        async move {
            let id = parse_id(params.get().map_err(failed)?.get_id().map_err(failed)?)?;
            blocking(move || {
                let package = find_user(&catalog, id)?;
                store.set_pinned(&package.id, true).map_err(failed)
            })
            .await
        }
    }

    fn unpin(
        self: capnp::capability::Rc<Self>,
        params: app_host::UnpinParams,
        _results: app_host::UnpinResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let (store, catalog) = (Arc::clone(&self.store), Arc::clone(&self.catalog));
        async move {
            let id = parse_id(params.get().map_err(failed)?.get_id().map_err(failed)?)?;
            blocking(move || {
                let package = find_user(&catalog, id)?;
                store.set_pinned(&package.id, false).map_err(failed)
            })
            .await
        }
    }

    fn remove(
        self: capnp::capability::Rc<Self>,
        params: app_host::RemoveParams,
        _results: app_host::RemoveResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let (store, catalog) = (Arc::clone(&self.store), Arc::clone(&self.catalog));
        let max_bytes = self.max_store_bytes;
        async move {
            let id = parse_id(params.get().map_err(failed)?.get_id().map_err(failed)?)?;
            blocking(move || {
                let package = find_user(&catalog, id)?;
                store.remove(&package.id, false).map_err(failed)?;
                collect_after_change(&store, &catalog, max_bytes, &[]);
                Ok(())
            })
            .await
        }
    }

    fn mark_used(
        self: capnp::capability::Rc<Self>,
        params: app_host::MarkUsedParams,
        _results: app_host::MarkUsedResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let (store, catalog) = (Arc::clone(&self.store), Arc::clone(&self.catalog));
        async move {
            let id = parse_id(params.get().map_err(failed)?.get_id().map_err(failed)?)?;
            blocking(move || {
                let package = find_user(&catalog, id)?;
                store.mark_used(&package.id).map_err(failed)
            })
            .await
        }
    }

    async fn review_local(
        self: capnp::capability::Rc<Self>,
        params: app_host::ReviewLocalParams,
        mut results: app_host::ReviewLocalResults,
    ) -> Result<(), Error> {
        let p = params.get().map_err(failed)?;
        let handed = p.get_tapp_bytes().map_err(failed)?;
        if handed.len() > crate::local_package::MAX_LOCAL_BYTES {
            return Err(Error::failed(
                "Local package exceeds the file size limit".into(),
            ));
        }
        let bytes = handed.to_vec();
        let review = blocking(move || crate::local_package::review(&bytes).map_err(failed)).await?;
        let json = serde_json::to_string(&review).map_err(failed)?;
        results.get().set_review_json(&json);
        Ok(())
    }

    async fn install_local(
        self: capnp::capability::Rc<Self>,
        params: app_host::InstallLocalParams,
        mut results: app_host::InstallLocalResults,
    ) -> Result<(), Error> {
        let p = params.get().map_err(failed)?;
        let handed = p.get_tapp_bytes().map_err(failed)?;
        if handed.len() > crate::local_package::MAX_LOCAL_BYTES {
            return Err(Error::failed(
                "Local package exceeds the file size limit".into(),
            ));
        }
        let bytes = handed.to_vec();
        let root = p
            .get_reviewed_root()
            .map_err(failed)?
            .to_string()
            .map_err(failed)?;
        let hash = p
            .get_reviewed_archive_hash()
            .map_err(failed)?
            .to_string()
            .map_err(failed)?;
        let id = blocking(move || {
            let root = crate::local_package::install(&bytes, &root, &hash).map_err(failed)?;
            format!("tapp:{root}")
                .parse::<PackageId>()
                .map_err(|_| Error::failed("Invalid local package root".into()))
        })
        .await?;
        results.get().set_id(id.as_bytes());
        Ok(())
    }

    fn install(
        self: capnp::capability::Rc<Self>,
        params: app_host::InstallParams,
        mut results: app_host::InstallResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let (store, catalog) = (Arc::clone(&self.store), Arc::clone(&self.catalog));
        let max_bytes = self.max_store_bytes;
        async move {
            let p = params.get().map_err(failed)?;
            let tapp_bytes = p.get_tapp_bytes().map_err(failed)?.to_vec();
            let pinning = parse_pinning(p.get_pinning().map_err(failed)?);
            let expected_root = parse_id(p.get_expected_root().map_err(failed)?)?;
            let publisher_bytes = p.get_expected_publisher().map_err(failed)?;
            let publisher: [u8; 32] = publisher_bytes.try_into().map_err(|_| {
                Error::failed(format!(
                    "publisher key must be 32 bytes, got {}",
                    publisher_bytes.len()
                ))
            })?;
            let expected = InstallExpectation {
                package_root: expected_root,
                publisher,
                app_id: p
                    .get_expected_app_id()
                    .map_err(failed)?
                    .to_string()
                    .map_err(failed)?,
                version: p
                    .get_expected_version()
                    .map_err(failed)?
                    .to_string()
                    .map_err(failed)?,
                size_bytes: p.get_expected_size_bytes(),
            };
            let id = blocking(move || {
                let installed =
                    install_verified_tapp(&store, &tapp_bytes, pinning, &expected, None)
                        .map_err(failed)?;
                // Best-effort: the package is already committed.
                collect_after_change(&store, &catalog, max_bytes, &[installed.id.as_str()]);
                installed
                    .package_id()
                    .ok_or_else(|| Error::failed("installed package has no package id".into()))
            })
            .await?;
            results.get().set_id(id.as_bytes());
            Ok(())
        }
    }

    fn disk_usage(
        self: capnp::capability::Rc<Self>,
        _params: app_host::DiskUsageParams,
        mut results: app_host::DiskUsageResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let store = Arc::clone(&self.store);
        async move {
            let usage = blocking(move || store.disk_usage().map_err(failed)).await?;
            let mut out = results.get();
            out.set_total_bytes(usage.total_bytes);
            let mut by = out.init_by_pinning();
            by.set_pinned_bytes(usage.pinned_bytes);
            by.set_tried_bytes(usage.tried_bytes);
            Ok(())
        }
    }

    fn launch(
        self: capnp::capability::Rc<Self>,
        params: app_host::LaunchParams,
        mut results: app_host::LaunchResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let (store, catalog) = (Arc::clone(&self.store), Arc::clone(&self.catalog));
        async move {
            let id = parse_id(params.get().map_err(failed)?.get_id().map_err(failed)?)?;
            let status = blocking(move || {
                let package = find(&catalog, id)?;
                let mut command = std::process::Command::new(launcher_bin());
                command
                    .arg("run")
                    .arg("--manifest")
                    .arg(&package.manifest_path)
                    .arg("--payload-root")
                    .arg(&package.payload_root);
                if let Some(socket) = wayland_socket() {
                    command.arg("--wayland-socket").arg(socket);
                }
                let status = spawn_detached(command)?;
                if package.user_managed() {
                    store.mark_used(&package.id).map_err(failed)?;
                }
                Ok(status)
            })
            .await?;
            results.get().set_status(&status);
            Ok(())
        }
    }

    #[allow(
        clippy::manual_async_fn,
        reason = "the generated Cap'n Proto Server trait requires an opaque Future return type"
    )]
    fn launch_desktop(
        self: capnp::capability::Rc<Self>,
        params: app_host::LaunchDesktopParams,
        mut results: app_host::LaunchDesktopResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        async move {
            let desktop_id = params.get()?.get_desktop_id()?.to_string()?;
            let status = tokio::task::spawn_blocking(move || {
                let mut command = std::process::Command::new(launcher_bin());
                command.arg("run-host").arg("--desktop-id").arg(desktop_id);
                if let Some(socket) = wayland_socket() {
                    command.arg("--wayland-socket").arg(socket);
                }
                spawn_detached(command)
            })
            .await
            .map_err(|e| Error::failed(format!("join: {e}")))??;
            results.get().set_status(&status);
            Ok(())
        }
    }

    #[allow(
        clippy::manual_async_fn,
        reason = "the generated Cap'n Proto Server trait requires an opaque Future return type"
    )]
    fn flatpak(
        self: capnp::capability::Rc<Self>,
        params: app_host::FlatpakParams,
        mut results: app_host::FlatpakResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        async move {
            let p = params.get()?;
            let action = p.get_action()?;
            let app_id = p.get_app_id()?.to_string()?;
            let reference = p.get_reference()?.to_vec();
            if reference.len() > 64 * 1024 {
                return Err(Error::failed("Flatpak reference exceeds 64 KiB".into()));
            }
            let status = tokio::task::spawn_blocking(move || {
                let flatpak = std::path::Path::new("/usr/bin/flatpak");
                match action {
                    WireFlatpakAction::Install => {
                        if reference.is_empty() {
                            return flatpak_install_flathub_from(flatpak, &app_id);
                        }
                        let mut staged = tempfile::NamedTempFile::new()
                            .map_err(|e| Error::failed(format!("stage Flatpak reference: {e}")))?;
                        staged
                            .write_all(&reference)
                            .map_err(|e| Error::failed(format!("stage Flatpak reference: {e}")))?;
                        let mut command = std::process::Command::new(flatpak);
                        command.args(["install", "--user", "--noninteractive", "--"]);
                        command.arg(staged.path());
                        run_host_command(command)
                    }
                    WireFlatpakAction::Update => flatpak_app_from(flatpak, "update", &app_id),
                    WireFlatpakAction::Remove => flatpak_remove_from(flatpak, &app_id),
                }
            })
            .await
            .map_err(|e| Error::failed(format!("join: {e}")))??;
            results.get().set_status(&status);
            Ok(())
        }
    }

    #[allow(
        clippy::manual_async_fn,
        reason = "the generated Cap'n Proto Server trait requires an opaque Future return type"
    )]
    fn remote(
        self: capnp::capability::Rc<Self>,
        params: app_host::RemoteParams,
        mut results: app_host::RemoteResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        async move {
            let p = params.get()?;
            let action = p.get_action()?;
            let name = p.get_name()?.to_string()?;
            let url = p.get_url()?.to_string()?;
            let status = tokio::task::spawn_blocking(move || {
                let flatpak = std::path::Path::new("/usr/bin/flatpak");
                match action {
                    WireRemoteAction::Add => flatpak_remote_add_from(flatpak, &name, &url),
                    WireRemoteAction::Remove => flatpak_remote_delete_from(flatpak, &name),
                    WireRemoteAction::Enable | WireRemoteAction::Disable => {
                        flatpak_remote_enabled_from(
                            flatpak,
                            &name,
                            action == WireRemoteAction::Enable,
                        )
                    }
                }
            })
            .await
            .map_err(|e| Error::failed(format!("join: {e}")))??;
            results.get().set_status(&status);
            Ok(())
        }
    }

    fn install_offer(
        self: capnp::capability::Rc<Self>,
        params: app_host::InstallOfferParams,
        mut results: app_host::InstallOfferResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let catalog = Arc::clone(&self.catalog);
        async move {
            let params = params.get().map_err(failed)?;
            let registry = params
                .get_registry()
                .map_err(failed)?
                .to_string()
                .map_err(failed)?;
            let expected = InstallExpectation {
                package_root: parse_id(params.get_expected_root().map_err(failed)?)?,
                publisher: params
                    .get_expected_publisher()
                    .map_err(failed)?
                    .try_into()
                    .map_err(|_| Error::failed("publisher key must be 32 bytes".into()))?,
                app_id: params
                    .get_app_id()
                    .map_err(failed)?
                    .to_string()
                    .map_err(failed)?,
                version: params
                    .get_version()
                    .map_err(failed)?
                    .to_string()
                    .map_err(failed)?,
                size_bytes: params.get_expected_size_bytes(),
            };
            let id = blocking(move || install_offer_from_catalogue(&catalog, &registry, &expected))
                .await?;
            results.get().set_id(id.as_bytes());
            Ok(())
        }
    }

    #[allow(
        clippy::manual_async_fn,
        reason = "the generated Cap'n Proto Server trait requires an opaque Future return type"
    )]
    fn flatpak_status(
        self: capnp::capability::Rc<Self>,
        _params: app_host::FlatpakStatusParams,
        mut results: app_host::FlatpakStatusResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        async move {
            let status = tokio::task::spawn_blocking(|| {
                flatpak_status_from(std::path::Path::new("/usr/bin/flatpak"))
            })
            .await
            .map_err(failed)??;
            let mut result = results.get();
            result.set_available(status.available);
            result.set_installed(&status.installed);
            result.set_remotes(&status.remotes);
            Ok(())
        }
    }

    #[allow(
        clippy::manual_async_fn,
        reason = "the generated Cap'n Proto Server trait requires an opaque Future return type"
    )]
    fn repositories(
        self: capnp::capability::Rc<Self>,
        _params: app_host::RepositoriesParams,
        mut results: app_host::RepositoriesResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        async move {
            let json = tokio::task::spawn_blocking(repositories_json)
                .await
                .map_err(|e| Error::failed(format!("join: {e}")))??;
            results.get().set_json(&json);
            Ok(())
        }
    }
    async fn runtime_offer(
        self: capnp::capability::Rc<Self>,
        params: app_host::RuntimeOfferParams,
        mut results: app_host::RuntimeOfferResults,
    ) -> Result<(), Error> {
        let parameters = params.get().map_err(failed)?;
        let registry = parameters
            .get_registry()
            .map_err(failed)?
            .to_string()
            .map_err(failed)?;
        let runtime = parameters
            .get_runtime()
            .map_err(failed)?
            .to_string()
            .map_err(failed)?;
        if registry.is_empty()
            || registry.len() > 256
            || registry.chars().any(char::is_control)
            || runtime.is_empty()
            || runtime.len() > 256
            || runtime.chars().any(char::is_control)
        {
            return Err(Error::failed("invalid runtime offer request".into()));
        }
        let json = tokio::task::spawn_blocking(move || {
            let offer = app_cli::runtime_offer(&runtime, &registry).map_err(failed)?;
            serde_json::to_string(&offer).map_err(failed)
        })
        .await
        .map_err(failed)??;
        if json.len() > 256 * 1024 {
            return Err(Error::failed(
                "runtime offer exceeds the response limit".into(),
            ));
        }
        results.get().set_offer(&json);
        Ok(())
    }

    #[allow(
        clippy::manual_async_fn,
        reason = "the generated Cap'n Proto Server trait requires an opaque Future return type"
    )]
    fn registry_catalogue(
        self: capnp::capability::Rc<Self>,
        params: app_host::RegistryCatalogueParams,
        mut results: app_host::RegistryCatalogueResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        async move {
            let registry = params
                .get()
                .map_err(failed)?
                .get_registry()
                .map_err(failed)?
                .to_string()
                .map_err(failed)?;
            let read = tokio::task::spawn_blocking(move || registry_catalogue(&registry))
                .await
                .map_err(failed)?;
            set_catalogue(read, |rows, refusal, detail| {
                let mut out = results.get();
                out.set_entries(rows);
                out.set_refusal(refusal);
                out.set_detail(detail);
            })
        }
    }

    #[allow(
        clippy::manual_async_fn,
        reason = "the generated Cap'n Proto Server trait requires an opaque Future return type"
    )]
    fn last_verified_catalogue(
        self: capnp::capability::Rc<Self>,
        params: app_host::LastVerifiedCatalogueParams,
        mut results: app_host::LastVerifiedCatalogueResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        async move {
            let registry = params
                .get()
                .map_err(failed)?
                .get_registry()
                .map_err(failed)?
                .to_string()
                .map_err(failed)?;
            let read = tokio::task::spawn_blocking(move || last_verified_catalogue(&registry))
                .await
                .map_err(failed)?;
            set_catalogue(read, |rows, refusal, detail| {
                let mut out = results.get();
                out.set_entries(rows);
                out.set_refusal(refusal);
                out.set_detail(detail);
            })
        }
    }
    #[allow(
        clippy::manual_async_fn,
        reason = "the generated Cap'n Proto Server trait requires an opaque Future return type"
    )]
    fn flatpak_catalogue(
        self: capnp::capability::Rc<Self>,
        params: app_host::FlatpakCatalogueParams,
        mut results: app_host::FlatpakCatalogueResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        async move {
            let p = params.get().map_err(failed)?;
            let remote = p
                .get_remote()
                .map_err(failed)?
                .to_string()
                .map_err(failed)?;
            let user_managed = p.get_user_managed();
            let rows = tokio::task::spawn_blocking(move || {
                flatpak_catalogue_from(
                    std::path::Path::new("/usr/bin/flatpak"),
                    &remote,
                    user_managed,
                )
            })
            .await
            .map_err(failed)?;
            set_catalogue(rows, |rows, refusal, detail| {
                let mut out = results.get();
                out.set_rows(rows);
                out.set_refusal(refusal);
                out.set_detail(detail);
            })
        }
    }

    #[allow(
        clippy::manual_async_fn,
        reason = "the generated Cap'n Proto Server trait requires an opaque Future return type"
    )]
    fn flatpak_install_remote(
        self: capnp::capability::Rc<Self>,
        params: app_host::FlatpakInstallRemoteParams,
        mut results: app_host::FlatpakInstallRemoteResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        async move {
            let p = params.get().map_err(failed)?;
            let remote = p
                .get_remote()
                .map_err(failed)?
                .to_string()
                .map_err(failed)?;
            let app_id = p
                .get_app_id()
                .map_err(failed)?
                .to_string()
                .map_err(failed)?;
            let branch = p
                .get_branch()
                .map_err(failed)?
                .to_string()
                .map_err(failed)?;
            require_flatpak_selector(&branch)?;
            require_flatpak_selector(&remote)?;
            require_flatpak_selector(&app_id)?;
            let status = tokio::task::spawn_blocking(move || {
                flatpak_install_remote_from(
                    std::path::Path::new("/usr/bin/flatpak"),
                    &remote,
                    &app_id,
                    &branch,
                )
            })
            .await
            .map_err(failed)??;
            results.get().set_status(&status);
            Ok(())
        }
    }
}

#[cfg(test)]
mod command_tests {
    use super::{repositories_json_from, safe_host_diagnostic};

    fn catalogue_configuration() -> app_cli::config::RegistriesFile {
        app_cli::config::RegistriesFile {
            registries: vec![app_cli::config::RegistryConfig {
                name: "Configured source".into(),
                base_url: "https://registry.example.test/".into(),
                owner_pubkey_hex: "17".repeat(32),
                enabled: true,
                channel: tilo_app_package::version::ReleaseChannel::Stable,
            }],
        }
    }

    /// When a swap does leave the app host with a declaration it cannot read
    /// (the laptop on 2026-10-22: a one-version-per-interface file over an
    /// app host reading lists), reading a repository fails with the reason
    /// the Store shows, never an empty or silently unjudged catalogue.
    #[test]
    fn an_unreadable_system_declaration_refuses_the_repository_saying_the_system_needs_an_update() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("system-interfaces.toml");
        std::fs::write(&path, "[interfaces]\nsystem = [1]\nshell-ui = [1]\n").unwrap();
        let refused = super::system_declaration_readable(&path).unwrap_err();
        assert!(
            matches!(&refused, super::ReadError::Refused(crate::client::Refusal::SystemUpdateNeeded, detail) if detail.contains("system-interfaces.toml")),
            "{refused}"
        );

        std::fs::write(
            &path,
            "[interfaces]\nsystem = 1\nshell-ui = 11\nruntime = 2\n\n[runtime]\nservices = [\"app-host\"]\n",
        )
        .unwrap();
        assert!(super::system_declaration_readable(&path).is_ok());
        // A host without a declaration (a development machine) judges
        // nothing and reads repositories as before.
        std::fs::remove_file(&path).unwrap();
        assert!(super::system_declaration_readable(&path).is_ok());
    }

    #[test]
    fn registry_catalogue_restricts_fetch_to_enabled_host_configuration() {
        let mut configured = catalogue_configuration();
        let json = super::registry_catalogue_from(&configured, "Configured source", |registry| {
            assert_eq!(registry.base_url, "https://registry.example.test/");
            assert_eq!(registry.owner_pubkey_hex, "17".repeat(32));
            Ok(Vec::new())
        })
        .unwrap();
        assert_eq!(json, "[]");
        for name in ["", "https://attacker.example.test/", "Configured source\n"] {
            assert!(
                super::registry_catalogue_from(&configured, name, |_| panic!("unconfigured fetch"))
                    .is_err()
            );
        }
        configured.registries[0].enabled = false;
        assert!(
            super::registry_catalogue_from(&configured, "Configured source", |_| panic!(
                "disabled fetch"
            ))
            .is_err()
        );
    }

    #[test]
    fn an_expired_catalogue_reaches_the_store_as_expired() {
        let configured = catalogue_configuration();
        let error = super::registry_catalogue_from(&configured, "Configured source", |_| {
            Err(app_cli::client::RegistryError::Expired {
                expired_at_unix: 1_792_587_897,
            })
        })
        .unwrap_err();
        assert!(
            matches!(
                error,
                super::ReadError::Refused(crate::client::Refusal::Expired, _)
            ),
            "{error}"
        );
        let other = super::registry_catalogue_from(&configured, "Configured source", |_| {
            Err(app_cli::client::RegistryError::Http(
                "connection refused".into(),
            ))
        })
        .unwrap_err();
        assert!(matches!(other, super::ReadError::Failed(_)), "{other}");
    }

    #[test]
    fn registry_catalogue_preserves_checkpoint_failure_and_refuses_oversized_metadata() {
        let configured = catalogue_configuration();
        let error = super::registry_catalogue_from(&configured, "Configured source", |_| {
            Err(app_cli::client::RegistryError::Rollback)
        })
        .unwrap_err();
        assert!(error.to_string().contains("rolled back or replayed"));
        let entry: app_cli::client::VerifiedEntry = serde_json::from_value(serde_json::json!({
            "entry": {
                "package_id": "so.tilo.catalogue-test", "kind": "app", "name": {"en": "Catalogue test"}, "description": {"en": "x".repeat(super::MAX_REGISTRY_CATALOGUE_BYTES)},
                "tagline": {"en": ""}, "images": {}, "supported_languages": [], "categories": [],
                "source_url": null, "homepage": null, "publisher_pubkey_hex": "17".repeat(32),
                "versions": [], "updated_at_unix": 0, "signature_hex": ""
            }, "trust": "PublisherSigned", "version_index": null
        })).unwrap();
        let error =
            super::registry_catalogue_from(&configured, "Configured source", |_| Ok(vec![entry]))
                .unwrap_err();
        assert!(error.to_string().contains("exceeds the response limit"));
    }

    #[test]
    fn remote_enabled_changes_use_only_account_scope_and_preserve_refusal() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("flatpak");
        std::fs::write(
            &binary,
            r#"#!/bin/sh
printf '%s\n' "$@"
"#,
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        for (enabled, flag) in [(true, "--enable"), (false, "--disable")] {
            let result =
                super::flatpak_remote_enabled_from(&binary, "custom-tools", enabled).unwrap();
            assert_eq!(
                result.trim(),
                format!("remote-modify\n--user\n{flag}\n--\ncustom-tools")
            );
        }
        for name in ["--system", "../other", ""] {
            assert!(super::flatpak_remote_enabled_from(&binary, name, true).is_err());
        }
        std::fs::write(
            &binary,
            r"#!/bin/sh
echo 'No account source with that name' >&2
exit 1
",
        )
        .unwrap();
        assert!(
            super::flatpak_remote_enabled_from(&binary, "custom-tools", false)
                .unwrap_err()
                .to_string()
                .contains("No account source with that name")
        );
    }

    /// Removing an app keeps its data, as Flatpak does, and then removes the
    /// runtimes no app of the account uses any more, which nothing else
    /// removes from the person's installation. A failed cleanup does not
    /// make the removal fail.
    #[test]
    fn removing_an_app_keeps_its_data_and_removes_runtimes_nothing_uses() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("flatpak");
        let log = dir.path().join("calls");
        for unused_exit in [0, 1] {
            std::fs::write(
                &binary,
                format!(
                    "#!/bin/sh\necho \"$*\" >> '{}'\ncase \"$*\" in *--unused*) exit {unused_exit};; esac\n",
                    log.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
            let _ = std::fs::remove_file(&log);
            super::flatpak_remove_from(&binary, "org.example.App").expect("removed");
            let calls = std::fs::read_to_string(&log).unwrap();
            assert_eq!(
                calls.lines().collect::<Vec<_>>(),
                [
                    "uninstall --user --noninteractive -- org.example.App",
                    "uninstall --user --unused --noninteractive",
                ],
                "no --delete-data: the app's data stays"
            );
        }
        assert!(super::flatpak_remove_from(&binary, "--system").is_err());
    }

    #[test]
    fn app_and_source_changes_end_options_before_caller_values() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("flatpak");
        std::fs::write(&binary, "#!/bin/sh\nprintf '%s ' \"$@\"\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            super::flatpak_app_from(&binary, "uninstall", "org.example.App")
                .unwrap()
                .trim(),
            "uninstall --user --noninteractive -- org.example.App"
        );
        assert_eq!(
            super::flatpak_remote_add_from(&binary, "tools", "--no-gpg-verify")
                .unwrap()
                .trim(),
            "remote-add --user --if-not-exists -- tools --no-gpg-verify"
        );
        assert_eq!(
            super::flatpak_remote_delete_from(&binary, "tools")
                .unwrap()
                .trim(),
            "remote-delete --user --force -- tools"
        );
        // `--all` would uninstall every app the account has.
        for selector in ["--all", "--delete-data", "", "a b"] {
            assert!(super::flatpak_app_from(&binary, "uninstall", selector).is_err());
            assert!(super::flatpak_remote_add_from(&binary, selector, "https://x").is_err());
            assert!(super::flatpak_remote_delete_from(&binary, selector).is_err());
        }
    }

    #[test]
    fn catalogue_install_requires_the_exact_selected_signed_identity() {
        let expected = crate::install::InstallExpectation {
            package_root: tilo_app_package::PackageId::from_bytes([7; 32]),
            publisher: [8; 32],
            app_id: "so.tilo.notes".into(),
            version: "1.2.3".into(),
            size_bytes: 42,
        };
        let offer = app_cli::CatalogueApp {
            package_id: expected.app_id.clone(),
            display_name: "Notes".into(),
            description: String::new(),
            tagline: String::new(),
            images: Vec::new(),
            supported_languages: Vec::new(),
            category: None,
            version: expected.version.clone(),
            registry: "Tilo main".into(),
            download_url: "https://apps.tilo.so/notes.tapp".into(),
            tapp_root_hash_hex: expected.package_root.to_hex(),
            tapp_size_bytes: expected.size_bytes,
            tapp_blake3_hex: "09".repeat(32),
            architecture: std::env::consts::ARCH.into(),
            isa: std::env::consts::ARCH.into(),
            runtime: None,
            publisher_pubkey_hex: hex::encode(expected.publisher),
            trust: "verified".into(),
        };
        super::require_offer_identity(&offer, &expected).unwrap();
        let mut replacements = Vec::new();
        let mut changed = offer.clone();
        changed.package_id = "so.tilo.settings".into();
        replacements.push(changed);
        let mut changed = offer.clone();
        changed.version = "1.2.4".into();
        replacements.push(changed);
        let mut changed = offer.clone();
        changed.tapp_root_hash_hex = "0a".repeat(32);
        replacements.push(changed);
        let mut changed = offer.clone();
        changed.publisher_pubkey_hex = "0b".repeat(32);
        replacements.push(changed);
        let mut changed = offer;
        changed.tapp_size_bytes += 1;
        replacements.push(changed);
        for replacement in replacements {
            assert!(super::require_offer_identity(&replacement, &expected).is_err());
        }

        let missing = format!("tilo-runtime/2026.07+native.{}", "f".repeat(32));
        let mut newest = offer_template(&expected);
        newest.version = "1.2.4".into();
        newest.runtime = Some(missing);
        // The older build needs no runtime at all, so it runs here.
        let mut runnable = offer_template(&expected);
        runnable.version = "1.2.2".into();
        let mut foreign = runnable.clone();
        foreign.version = "1.2.3".into();
        foreign.publisher_pubkey_hex = "0b".repeat(32);
        let mut later = runnable.clone();
        later.version = "1.2.5".into();
        let offers = vec![later, newest.clone(), foreign, runnable.clone()];
        assert_eq!(super::first_open_choice(&newest, &offers), &runnable);
        // Nothing runnable: the requested version, runtime and all.
        let only = vec![newest.clone()];
        assert_eq!(super::first_open_choice(&newest, &only), &newest);
        // A requested version that runs here is never replaced.
        assert_eq!(super::first_open_choice(&runnable, &offers), &runnable);
    }

    fn offer_template(expected: &crate::install::InstallExpectation) -> app_cli::CatalogueApp {
        app_cli::CatalogueApp {
            package_id: expected.app_id.clone(),
            display_name: "Notes".into(),
            description: String::new(),
            tagline: String::new(),
            images: Vec::new(),
            supported_languages: Vec::new(),
            category: None,
            version: expected.version.clone(),
            registry: "Tilo main".into(),
            download_url: "https://apps.tilo.so/notes.tapp".into(),
            tapp_root_hash_hex: expected.package_root.to_hex(),
            tapp_size_bytes: expected.size_bytes,
            tapp_blake3_hex: "09".repeat(32),
            architecture: std::env::consts::ARCH.into(),
            isa: std::env::consts::ARCH.into(),
            runtime: None,
            publisher_pubkey_hex: hex::encode(expected.publisher),
            trust: "verified".into(),
        }
    }

    #[test]
    fn configured_remote_queries_and_installs_keep_scope_branch_and_failures() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("flatpak");
        std::fs::write(&binary, "#!/bin/sh\nprintf '%s\\n' \"$*\"\n").unwrap();
        let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&binary, permissions).unwrap();
        for (user, scope) in [(true, "--user"), (false, "--system")] {
            let result = catalogue_within_hang_bound(&binary, user).unwrap();
            assert_eq!(
                result.trim(),
                format!(
                    "remote-ls --cached {scope} --app --arch {} --columns=application,name,description,version,branch,origin -- tools",
                    std::env::consts::ARCH
                )
            );
        }
        let installed =
            super::flatpak_install_remote_from(&binary, "tools", "org.example.Editor", "beta")
                .unwrap();
        assert_eq!(
            installed.trim(),
            "install --user --noninteractive -- tools org.example.Editor//beta"
        );
        assert!(super::flatpak_catalogue_from(&binary, "--system", true).is_err());
        assert!(
            super::flatpak_install_remote_from(&binary, "tools", "--system", "stable").is_err()
        );
        assert!(
            super::flatpak_install_remote_from(&binary, "tools", "org.example.Editor", "../main")
                .is_err()
        );
        std::fs::write(
            &binary,
            "#!/bin/sh\necho 'repository is unavailable' >&2\nexit 4\n",
        )
        .unwrap();
        assert!(
            catalogue_within_hang_bound(&binary, true)
                .unwrap_err()
                .to_string()
                .contains("repository is unavailable")
        );
    }

    #[test]
    fn cold_configured_catalogue_fetches_summary_then_reuses_cached_rows() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("flatpak");
        std::fs::write(&binary, r#"#!/bin/sh
root=$(dirname "$0")
printf '%s\n' "$*" >> "$root/calls"
case "$*" in
  *--cached*)
    if [ ! -f "$root/summary" ]; then
      echo "error: Unable to load summary from remote tools: No cached summary for remote 'tools'" >&2
      exit 1
    fi;;
  *) touch "$root/summary";;
esac
printf 'org.example.Editor\tEditor\tA signed editor\t1.0\tstable\ttools\n'
"#).unwrap();
        let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&binary, permissions).unwrap();
        let first = catalogue_within_hang_bound(&binary, true).unwrap();
        assert!(first.starts_with("org.example.Editor\tEditor"));
        assert_eq!(catalogue_within_hang_bound(&binary, true).unwrap(), first);
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        let calls: Vec<_> = calls.lines().collect();
        assert_eq!(calls.len(), 3);
        assert!(calls[0].contains("--cached --user"));
        assert!(!calls[1].contains("--cached"));
        assert!(calls[1].contains("--user --app --arch"));
        assert!(calls[2].contains("--cached --user"));
        assert!(calls.iter().all(|call| call.ends_with("-- tools")));
    }

    /// The catalogue read with only the hang bound as its deadline: the
    /// fixtures answer at once, however slowly a loaded machine runs them.
    fn catalogue_within_hang_bound(
        binary: &std::path::Path,
        user_managed: bool,
    ) -> Result<String, super::ReadError> {
        super::flatpak_catalogue_with_timeout(
            binary,
            "tools",
            user_managed,
            tilo_test_env::hang_bound(),
        )
    }

    fn catalogue_fixture(dir: &std::path::Path, script: &str) -> std::path::PathBuf {
        let binary = dir.join("flatpak");
        std::fs::write(&binary, format!("#!/bin/sh\n{script}\n")).unwrap();
        let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&binary, permissions).unwrap();
        binary
    }

    #[test]
    fn catalogue_does_not_retry_verification_disabled_or_foreign_cache_errors() {
        for diagnostic in [
            "GPG verification failed",
            "Remote tools is disabled",
            "No cached summary for remote 'other'",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let script = format!(
                r#"printf '%s\n' "$*" >> "$(dirname "$0")/calls"
echo "{diagnostic}" >&2
exit 1"#
            );
            let binary = catalogue_fixture(dir.path(), &script);
            let error = catalogue_within_hang_bound(&binary, false).unwrap_err();
            assert!(error.to_string().contains(diagnostic));
            let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
            assert_eq!(calls.lines().count(), 1);
            assert!(calls.contains("--cached --system"));
        }
    }

    #[test]
    fn cold_catalogue_preserves_verified_fetch_failure() {
        let dir = tempfile::tempdir().unwrap();
        let binary = catalogue_fixture(
            dir.path(),
            r#"
case "$*" in
  *--cached*) echo "No cached summary for remote 'tools'" >&2;;
  *) echo "GPG verification failed" >&2;;
esac
exit 1
"#,
        );
        assert!(
            catalogue_within_hang_bound(&binary, true)
                .unwrap_err()
                .to_string()
                .contains("GPG verification failed")
        );
    }

    #[test]
    fn cold_catalogue_fetch_is_killed_at_the_shared_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let binary = catalogue_fixture(
            dir.path(),
            r#"
case "$*" in
  *--cached*) echo "No cached summary for remote 'tools'" >&2; exit 1;;
  *) echo $$ > "$(dirname "$0")/fetch.pid"; exec sleep infinity;;
esac
"#,
        );
        // The fetch never ends on its own: only the deadline's kill ends the call.
        let (done, fetched) = std::sync::mpsc::channel();
        let fetch = binary;
        std::thread::spawn(move || {
            let fetched = super::flatpak_catalogue_with_timeout(
                &fetch,
                "tools",
                true,
                std::time::Duration::from_millis(100),
            );
            let _ = done.send(fetched.is_err());
        });
        assert!(tilo_test_env::recv(
            "the timed-out catalogue fetch",
            &fetched
        ));
        // A loaded machine can reach the deadline before the fetch starts, or
        // between the shell creating the pid file and writing it: an empty
        // file names no process, and `/proc/` itself never goes away.
        if let Ok(pid) = std::fs::read_to_string(dir.path().join("fetch.pid"))
            && let Ok(pid) = pid.trim().parse::<u32>()
        {
            tilo_test_env::wait_until("the killed fetch to be gone", || {
                !std::path::Path::new("/proc").join(pid.to_string()).exists()
            });
        }
    }

    #[test]
    fn official_system_flathub_install_materializes_only_the_account_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let binary = flathub_install_fixture(dir.path());
        std::fs::write(
            dir.path().join("rows"),
            "flathub\thttps://dl.flathub.org/repo/\tFlathub\tsystem\n",
        )
        .unwrap();
        assert_eq!(
            super::flatpak_install_flathub_from(&binary, "org.example.Editor")
                .unwrap()
                .trim(),
            "Installed"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls")).unwrap(),
            concat!(
                "remotes --columns=name,url,title,options\n",
                "remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo\n",
                "install --user --noninteractive -- flathub org.example.Editor\n"
            )
        );
    }

    #[test]
    fn official_flathub_install_preserves_disabled_foreign_and_missing_sources() {
        let dir = tempfile::tempdir().unwrap();
        let binary = flathub_install_fixture(dir.path());
        for rows in [
            "flathub\thttps://dl.flathub.org/repo/\tFlathub\tsystem\nflathub\thttps://dl.flathub.org/repo/\tFlathub\tuser,disabled\n",
            "flathub\thttps://dl.flathub.org/repo/\tFlathub\tsystem\nflathub\thttps://foreign.example/repo\tOther\tuser\n",
            "flathub\thttps://dl.flathub.org/repo/\tFlathub\tsystem,disabled\n",
            "flathub\thttps://dl.flathub.org/repo/\tFlathub\tsystem\nflathub\thttps://dl.flathub.org/repo/\tFlathub\tuser,no-gpg-verify\n",
            "",
        ] {
            std::fs::write(dir.path().join("rows"), rows).unwrap();
            std::fs::write(dir.path().join("calls"), "").unwrap();
            let error =
                super::flatpak_install_flathub_from(&binary, "org.example.Editor").unwrap_err();
            assert!(error.to_string().contains("Flathub source"), "{error}");
            assert_eq!(
                std::fs::read_to_string(dir.path().join("calls")).unwrap(),
                "remotes --columns=name,url,title,options\n"
            );
        }
        assert!(super::flatpak_install_flathub_from(&binary, "--no-gpg-verify").is_err());
    }

    fn flathub_install_fixture(dir: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let binary = dir.join("flatpak");
        std::fs::write(
            &binary,
            r#"#!/bin/sh
fixture_dir=$(dirname "$0")
printf '%s\n' "$*" >> "$fixture_dir/calls"
case "$1" in
  remotes) cat "$fixture_dir/rows";;
  remote-add) exit 0;;
  install) echo Installed;;
  *) exit 11;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        binary
    }

    #[test]
    fn flatpak_status_reads_host_apps_and_remotes_and_reports_failure() {
        let dir = tempfile::tempdir().unwrap();
        let flatpak = dir.path().join("flatpak");
        assert!(!super::flatpak_status_from(&flatpak).unwrap().available);
        std::fs::write(&flatpak, r#"#!/bin/sh
case "$*" in
  --version) echo 'Flatpak 1.16.1';;
  'list --app --columns=application,version,origin,installation') printf 'org.gnome.Calculator\t48.0\n';;
  'remotes --columns=name,url,title,options') printf 'flathub\thttps://dl.flathub.org/repo/\tFlathub\tsystem\n';;
  *) exit 12;;
esac
"#).unwrap();
        let mut permissions = std::fs::metadata(&flatpak).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&flatpak, permissions).unwrap();
        let status = super::flatpak_status_from(&flatpak).unwrap();
        assert!(status.available);
        assert!(status.installed.contains("org.gnome.Calculator\t48.0"));
        assert!(
            status
                .remotes
                .contains("flathub\thttps://dl.flathub.org/repo/")
        );
        std::fs::write(&flatpak, "#!/bin/sh\necho unavailable >&2\nexit 3\n").unwrap();
        let error = super::flatpak_status_from(&flatpak)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unavailable"), "{error}");
    }

    #[test]
    fn repositories_are_what_app_repo_list_prints() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("app");
        let script = |body: &str| {
            std::fs::write(&app, format!("#!/bin/sh\n{body}\n")).unwrap();
            let mut permissions = std::fs::metadata(&app).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
            std::fs::set_permissions(&app, permissions).unwrap();
        };
        script(r#"[ "$*" = "repo list --json" ] && echo '{"registry":[]}'"#);
        assert_eq!(
            repositories_json_from(&app).unwrap().trim(),
            r#"{"registry":[]}"#
        );
        script("echo broken >&2; exit 3");
        let error = repositories_json_from(&app).unwrap_err().to_string();
        assert!(
            error.contains("exited 3") && error.contains("broken"),
            "{error}"
        );
    }

    #[test]
    fn host_diagnostics_are_small_and_strip_terminal_controls() {
        let mut bytes = vec![b'x'; 8192];
        bytes.extend_from_slice(b"\n\x1b[31mfailure\0");
        let diagnostic = safe_host_diagnostic(&bytes);
        assert!(diagnostic.len() <= 4096);
        assert!(!diagnostic.contains('\x1b'));
        assert!(!diagnostic.contains('\0'));
        assert!(diagnostic.ends_with("[31mfailure"));
    }
}

#[cfg(test)]
mod dev_override_tests {
    use super::{LAUNCHER_BIN_ENV, path_or_default};
    use package_store::dev_env::dev_override_from;
    use std::path::PathBuf;

    #[test]
    fn a_release_build_keeps_the_image_launcher() {
        let default = "/usr/libexec/tilo/tilo-launcher";
        let gated = dev_override_from(false, LAUNCHER_BIN_ENV, Some("/tmp/attacker".into()));
        assert_eq!(path_or_default(gated, default), PathBuf::from(default));
        let honoured = dev_override_from(true, LAUNCHER_BIN_ENV, Some("/dev/overlay".into()));
        assert_eq!(
            path_or_default(honoured, default),
            PathBuf::from("/dev/overlay")
        );
    }
}
