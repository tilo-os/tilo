//! Fetching a package the machine does not have yet.
//!
//! Three steps in one blocking call: find the entry in the catalogue,
//! download the `.tapp` to a staging file, hand the path to
//! `tilo-install`.
//!
//! Trust is decided there, not here: `tilo-install` recomputes the
//! content-addressed package id and checks the publisher signature
//! fail-closed before extracting, so a tampered download fails at
//! install rather than becoming an installed app. Nothing in this
//! module passes `--allow-self-signed` — an install started by typing
//! a name into search is the last place to relax a signature check.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::CatalogueApp;

/// How long a .tapp download may take before it's abandoned;
/// overridable via `TILO_APP_DOWNLOAD_TIMEOUT` (seconds).
const DEFAULT_DOWNLOAD_TIMEOUT_SECS: u64 = 120;

fn download_timeout() -> Duration {
    std::env::var("TILO_APP_DOWNLOAD_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map_or(
            Duration::from_secs(DEFAULT_DOWNLOAD_TIMEOUT_SECS),
            Duration::from_secs,
        )
}

fn safe_download_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw).map_err(|error| format!("invalid package URL: {error}"))?;
    let loopback_http =
        url.scheme() == "http" && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if (url.scheme() != "https" && !loopback_http)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
        || url.fragment().is_some()
    {
        return Err(
            "package URL must be credential-free HTTPS (or loopback HTTP for development)"
                .to_string(),
        );
    }
    Ok(url)
}

/// Why an install did not happen; every variant renders to something a person can act on — reaches a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// No configured registry offers this id (any more).
    NotOffered { package_id: String },
    /// Nowhere writable to stage the download.
    NoStagingDir,
    /// The download itself failed.
    Download { detail: String },
    /// `tilo-install` could not be run at all.
    InstallerMissing { detail: String },
    /// `tilo-install` ran and refused; `detail` is its own output — usually the signature or hash complaint.
    Refused { code: i32, detail: String },
    /// Downloaded bytes did not match the signed registry metadata.
    Integrity { detail: String },
    /// The app needs a runtime build this device lacks and its registry
    /// does not offer; nothing was changed.
    RuntimeUnavailable {
        package_id: String,
        runtime: String,
        registry: String,
    },
}

impl InstallError {
    /// One-line summary for a notification body.
    pub fn user_message(&self) -> String {
        match self {
            InstallError::NotOffered { package_id } => {
                format!("no configured registry offers {package_id}")
            }
            InstallError::NoStagingDir => "there is nowhere writable to download it to".to_string(),
            InstallError::Download { detail } => format!("the download failed: {detail}"),
            InstallError::InstallerMissing { detail } => {
                format!("tilo-install could not be run: {detail}")
            }
            InstallError::Refused { code, detail } => {
                let trimmed = detail.trim();
                if trimmed.is_empty() {
                    format!("tilo-install refused it (exit {code})")
                } else {
                    format!("tilo-install refused it: {trimmed}")
                }
            }
            InstallError::Integrity { detail } => {
                format!("the downloaded package failed its registry check: {detail}")
            }
            InstallError::RuntimeUnavailable {
                package_id,
                runtime,
                registry,
            } => format!(
                "{package_id} needs runtime {runtime}, the exact runtime build it was made for; \
                 this device does not have it and the {registry} registry does not offer it, so \
                 nothing was changed"
            ),
        }
    }
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_message())
    }
}

/// Where downloaded .tapps are staged before install, under the cache dir so a read-only home in a sandbox isn't fatal.
pub fn staging_dir() -> Option<PathBuf> {
    let base = crate::config::cache_dir()?;
    let staged = base.join("staged-tapp");
    std::fs::create_dir_all(&staged).ok()?;
    Some(staged)
}

/// Names the `tilo-install` binary instead of the one on `PATH`.
pub const INSTALL_BIN_ENV: &str = "TILO_INSTALL_BIN";

/// The installer binary. [`INSTALL_BIN_ENV`] overrides it in a developer
/// build only (see [`package_store::dev_env`]), matching the compositor's
/// launcher resolution; a release build runs `tilo-install` from `PATH`.
pub fn tilo_install_bin() -> String {
    tilo_install_bin_from(package_store::dev_env::dev_override(INSTALL_BIN_ENV))
}

/// [`tilo_install_bin`] given the override the build's gate let through.
fn tilo_install_bin_from(allowed_override: Option<std::ffi::OsString>) -> String {
    allowed_override
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| "tilo-install".to_string())
}

/// The exact `tilo-install` invocation [`install`] runs. Split out so a
/// test can assert on argv without a network or launching the installer — in
/// particular, that --allow-self-signed is not in it.
pub fn install_argv(
    staged_tapp: &Path,
    publisher_pubkey_hex: &str,
    tapp_root_hash_hex: &str,
    payload_from: Option<&str>,
) -> Vec<String> {
    let mut argv = vec![
        "install".to_string(),
        staged_tapp.display().to_string(),
        // Replace an older copy of the same id rather than failing — opening
        // an app the user asked for shouldn't stall on a stale install of it.
        "--force".to_string(),
        // A valid tapp from another publisher is not the package the
        // signed registry entry vouched for.
        "--require-publisher".to_string(),
        publisher_pubkey_hex.to_string(),
        "--require-package-root".to_string(),
        tapp_root_hash_hex.to_string(),
    ];
    if let Some(url) = payload_from {
        // A thin package's payload comes from the registry that offered it;
        // every blob is still checked against the signed collection.
        argv.extend(["--payload-from".to_string(), url.to_string()]);
    }
    argv
}

/// The base URL of the registry that offered `app`, which also serves the
/// payload blobs of the packages it reviewed.
fn payload_source(app: &CatalogueApp) -> Option<String> {
    crate::config::load_registries_no_seed()
        .registries
        .into_iter()
        .find(|registry| registry.name == app.registry)
        .map(|registry| registry.base_url)
}

/// Largest runtime `.tapp` fetched as one file. A runtime's large libraries
/// travel as content-addressed payload blobs from the registry, so the
/// package itself is its signed metadata and small files.
const MAX_RUNTIME_TAPP_BYTES: u64 = 64 * 1024 * 1024;

fn tapp_limit(app: &CatalogueApp) -> u64 {
    if registry_types::is_runtime_package_id(&app.package_id) {
        MAX_RUNTIME_TAPP_BYTES
    } else {
        tilo_app_package::MAX_APP_TAPP_BYTES
    }
}

fn validate_offer_metadata(app: &CatalogueApp) -> Result<(), InstallError> {
    let limit = tapp_limit(app);
    if app.tapp_size_bytes == 0 || app.tapp_size_bytes > limit {
        return Err(InstallError::Integrity {
            detail: format!(
                "advertised size {} is outside the tapp limit (1..={limit} bytes)",
                app.tapp_size_bytes,
            ),
        });
    }
    let expected = hex::decode(&app.tapp_root_hash_hex).map_err(|e| InstallError::Integrity {
        detail: format!("registry supplied an invalid package root: {e}"),
    })?;
    if expected.len() != 32 {
        return Err(InstallError::Integrity {
            detail: "registry supplied a package root that is not 32 bytes".to_string(),
        });
    }
    let expected_tapp = hex::decode(&app.tapp_blake3_hex).map_err(|e| InstallError::Integrity {
        detail: format!("registry supplied an invalid tapp content hash: {e}"),
    })?;
    if expected_tapp.len() != 32 {
        return Err(InstallError::Integrity {
            detail: "registry supplied a tapp content hash that is not 32 bytes".to_string(),
        });
    }
    let publisher =
        hex::decode(&app.publisher_pubkey_hex).map_err(|e| InstallError::Integrity {
            detail: format!("registry supplied an invalid publisher key: {e}"),
        })?;
    if publisher.len() != 32 {
        return Err(InstallError::Integrity {
            detail: "registry supplied a publisher key that is not 32 bytes".to_string(),
        });
    }
    Ok(())
}

fn verify_download_metadata(app: &CatalogueApp, received: u64) -> Result<(), InstallError> {
    validate_offer_metadata(app)?;
    if received != app.tapp_size_bytes {
        return Err(InstallError::Integrity {
            detail: format!(
                "size mismatch (received {received} bytes, expected {})",
                app.tapp_size_bytes
            ),
        });
    }
    Ok(())
}

/// How much has to arrive before another progress report. A report
/// per read syscall would be thousands of lines for a few MB; 64 KiB
/// is about a percent of a small .tapp.
const PROGRESS_STEP_BYTES: u64 = 64 * 1024;

/// What a caller learns while a package is fetched. `total` is the
/// server's Content-Length when declared; otherwise the caller shows
/// an indeterminate indication instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    pub received: u64,
    pub total: Option<u64>,
    pub phase: InstallPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallPhase {
    Downloading,
    Installing { elapsed: Duration },
}

/// HTTP-download `url` to `dest`, reporting as the bytes arrive. Blocking.
///
/// `on_progress` fires once when headers land, then at most once per
/// `PROGRESS_STEP_BYTES`, and once more on the last byte — possible
/// because the body is streamed rather than buffered whole, which
/// also keeps a large package off the heap.
///
/// The bytes are not trusted here — see the module header.
///
/// # Errors
///
/// Returns an error (as a display-ready string) if the HTTP client
/// can't be built, the request/response fails, `dest`'s parent can't
/// be created, the file can't be written, or the server closes the
/// connection with nothing received.
pub fn download_reporting(
    url: &str,
    dest: &Path,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<u64, String> {
    download_reporting_limited(url, dest, u64::MAX, on_progress)
}

fn download_reporting_limited(
    url: &str,
    dest: &Path,
    max_bytes: u64,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<u64, String> {
    let url = safe_download_url(url)?;
    let response = package_client()?
        .get(url)
        .send()
        .map_err(|e| format!("{e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "package endpoint returned HTTP {} (redirects are not followed)",
            response.status()
        ));
    }
    save(response, dest, Body::Raw { limit: max_bytes }, on_progress)
}

/// Download the package at the signed `url`, whose signed length is
/// `signed_bytes`, to `dest`, and return how many bytes it holds.
///
/// A publisher keeps a compressed twin beside a package, at `<url>.zst`
/// ([`tilo_app_package::compressed`]); this asks for that first and
/// decompresses it to `dest`, never past `signed_bytes`. A host without one
/// answers 404 and the raw package is fetched instead. Either way `dest`
/// then holds plain package bytes, which the caller checks against the
/// signed length and hash exactly as before; progress counts the bytes on
/// the wire.
fn download_package(
    url: &str,
    dest: &Path,
    signed_bytes: u64,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<u64, String> {
    let parsed = safe_download_url(url)?;
    if let Some(twin) = twin_url(&parsed) {
        let response = package_client()?
            .get(twin)
            .send()
            .map_err(|e| format!("{e}"))?;
        if response.status().is_success() {
            return save(
                response,
                dest,
                Body::Compressed {
                    limit: signed_bytes,
                },
                on_progress,
            );
        }
        // No twin: a package published before twins, or a host that keeps
        // none. Any other answer, a redirect included, is not followed either.
    }
    download_reporting_limited(url, dest, signed_bytes, on_progress)
}

/// `url` with the compressed-twin suffix on its path, or `None` for a URL
/// whose query would end up after it.
fn twin_url(url: &reqwest::Url) -> Option<reqwest::Url> {
    if url.query().is_some() || url.path().ends_with('/') {
        return None;
    }
    let mut twin = url.clone();
    twin.set_path(&format!(
        "{}{}",
        url.path(),
        tilo_app_package::compressed::SUFFIX
    ));
    Some(twin)
}

fn package_client() -> Result<reqwest::blocking::Client, String> {
    tilo_net::http::blocking_client()
        .user_agent("tilo-app/0.1")
        .connect_timeout(Duration::from_secs(10))
        .timeout(download_timeout())
        // The publisher-signed URL is the only endpoint authorized to serve
        // these bytes. Following a redirect would silently widen that trust
        // and can cross from a public origin into a private network.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("http client: {e}"))
}

/// What a response body is: the package itself, or its compressed twin.
/// `limit` is the most package bytes either may yield.
#[derive(Clone, Copy)]
enum Body {
    Raw { limit: u64 },
    Compressed { limit: u64 },
}

/// Stream `response` into `dest` as plain package bytes, returning their
/// count. The body is refused past its wire limit — the package limit, or
/// for a twin the most a twin of that many bytes can take — and a twin is
/// refused once it would decompress past the package limit, so neither a
/// long body nor a small bomb gets more than `limit + 1` bytes onto disk.
fn save(
    response: reqwest::blocking::Response,
    dest: &Path,
    body: Body,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<u64, String> {
    let wire_limit = match body {
        Body::Raw { limit } => limit,
        Body::Compressed { limit } => tilo_app_package::compressed::compressed_bound(limit),
    };
    let total = response.content_length();
    if let Some(size) = total
        && size > wire_limit
    {
        return Err(format!(
            "server declared {size} bytes, above the {wire_limit}-byte limit"
        ));
    }
    on_progress(DownloadProgress {
        received: 0,
        total,
        phase: InstallPhase::Downloading,
    });

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let file = std::fs::File::create(dest).map_err(|e| format!("write {}: {e}", dest.display()))?;
    let mut writer = std::io::BufWriter::new(file);
    let mut wire = Wire {
        response,
        limit: wire_limit,
        total,
        received: 0,
        reported: 0,
        on_progress,
    };
    let written = match body {
        Body::Raw { .. } => std::io::copy(&mut wire, &mut writer),
        Body::Compressed { limit } => {
            tilo_app_package::compressed::decompress_bounded(&mut wire, limit, &mut writer)
        }
    }
    .map_err(|e| format!("{e}"))?;
    writer
        .flush()
        .map_err(|e| format!("write {}: {e}", dest.display()))?;
    drop(writer);

    if written == 0 {
        return Err("the server returned an empty file".to_string());
    }
    let received = wire.received;
    (wire.on_progress)(DownloadProgress {
        received,
        total,
        phase: InstallPhase::Downloading,
    });
    Ok(written)
}

/// A response body as it crosses the network: refused past `limit` bytes,
/// and reported at most once per [`PROGRESS_STEP_BYTES`].
struct Wire<'a> {
    response: reqwest::blocking::Response,
    limit: u64,
    total: Option<u64>,
    received: u64,
    reported: u64,
    on_progress: &'a mut dyn FnMut(DownloadProgress),
}

impl Read for Wire<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.response.read(buf)?;
        let received = self.received.saturating_add(n as u64);
        if received > self.limit {
            // Refused before the caller sees these bytes, so the over-limit
            // read never reaches disk.
            return Err(std::io::Error::other(format!(
                "download exceeded the {}-byte limit",
                self.limit
            )));
        }
        self.received = received;
        if received.saturating_sub(self.reported) >= PROGRESS_STEP_BYTES {
            self.reported = received;
            (self.on_progress)(DownloadProgress {
                received,
                total: self.total,
                phase: InstallPhase::Downloading,
            });
        }
        Ok(n)
    }
}

/// Download `app`'s package to `dest` and check it against the signed
/// registry entry: its length, then its BLAKE3. Returns the package length.
/// Whether it travelled compressed changes nothing here — `dest` holds the
/// plain bytes either way. On any failure `dest` is removed.
fn fetch_verified(
    app: &CatalogueApp,
    dest: &Path,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<u64, InstallError> {
    let refuse = |error: InstallError| {
        let _ = std::fs::remove_file(dest);
        error
    };
    let downloaded = download_package(&app.download_url, dest, app.tapp_size_bytes, on_progress)
        .map_err(|detail| refuse(InstallError::Download { detail }))?;
    verify_download_metadata(app, downloaded).map_err(refuse)?;
    let downloaded_bytes = std::fs::read(dest).map_err(|error| {
        refuse(InstallError::Integrity {
            detail: format!("read staged tapp for content verification: {error}"),
        })
    })?;
    let actual_hash = blake3::hash(&downloaded_bytes).to_hex().to_string();
    if !actual_hash.eq_ignore_ascii_case(&app.tapp_blake3_hex) {
        return Err(refuse(InstallError::Integrity {
            detail: "downloaded tapp content hash does not match the signed registry entry".into(),
        }));
    }
    Ok(downloaded)
}

/// [`download_reporting`] for callers with nothing to report to.
///
/// # Errors
///
/// Returns an error under the same conditions as [`download_reporting`].
pub fn download(url: &str, dest: &Path) -> Result<u64, String> {
    download_reporting(url, dest, &mut |_| {})
}

/// Download and install `app`, blocking until `tilo-install` exits.
/// The staged file is removed either way: on success the bytes now
/// live in the installed payload tree; on failure a half-written
/// download isn't worth keeping.
///
/// # Errors
///
/// Returns an error under the same conditions as [`install_app_reporting`].
pub fn install_app(app: &CatalogueApp) -> Result<(), InstallError> {
    install_app_reporting(app, &mut |_| {})
}

/// [`install_app`], reporting download progress. Reporting stops at
/// the download — `tilo-install` verifies+extracts as one opaque step,
/// and inventing a fraction for it would be a bar that lies.
///
/// # Errors
///
/// Returns an error if there's nowhere writable to stage the
/// download, the download itself fails, `tilo-install` can't be run,
/// or it runs and refuses the package.
pub fn install_app_reporting(
    app: &CatalogueApp,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<(), InstallError> {
    install_or_update_app_reporting(app, on_progress, Change::Install)
}

/// Install an exact runtime from a configured repository without replacing an app.
/// The normal downloader and installer retain registry, signature, payload and
/// runtime publisher checks; an installed version follows the normal update policy.
///
/// # Errors
/// Returns registry verification or offer errors, or the normal install refusal.
pub fn install_runtime_reporting(
    runtime: &str,
    registry: &str,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<CatalogueApp, InstallError> {
    let offer = crate::runtime_offer(runtime, registry)
        .map_err(|detail| InstallError::Download { detail })?
        .ok_or_else(|| InstallError::NotOffered {
            package_id: runtime.to_string(),
        })?;
    let installed = crate::installed_version(&offer.package_id)
        .map_err(|detail| InstallError::Integrity { detail })?;
    if installed.is_some() {
        update_app_reporting(&offer, on_progress)?;
    } else {
        install_app_reporting(&offer, on_progress)?;
    }
    Ok(offer)
}

/// What [`install_or_update_app_reporting`] does with the verified package.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Change {
    Install,
    Update,
    /// Replace the installed version with an older one.
    Rollback,
}

fn run_installer_with_progress(
    command: Command,
    downloaded: u64,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<bounded_command::Output, bounded_command::Error> {
    let mut running =
        bounded_command::spawn_prepared(command, None, 1024 * 1024, Duration::from_secs(600))?;
    let started = Instant::now();
    let mut last_heartbeat = Instant::now();
    loop {
        if let Some(result) = running.try_wait() {
            return result;
        }
        if last_heartbeat.elapsed() >= Duration::from_secs(1) {
            on_progress(DownloadProgress {
                received: downloaded,
                total: Some(downloaded),
                phase: InstallPhase::Installing {
                    elapsed: started.elapsed(),
                },
            });
            last_heartbeat = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn install_or_update_app_reporting(
    app: &CatalogueApp,
    on_progress: &mut dyn FnMut(DownloadProgress),
    change: Change,
) -> Result<(), InstallError> {
    validate_offer_metadata(app)?;
    // The runtime first: an app whose runtime is missing does not start, so
    // replacing a working build with it would break the app.
    if let Some(runtime) = app.runtime.as_deref()
        && !registry_types::is_runtime_package_id(&app.package_id)
    {
        ensure_runtime(app, runtime, on_progress)?;
    }
    let staging = staging_dir().ok_or(InstallError::NoStagingDir)?;
    // The package id comes from a signed registry entry, but it still
    // becomes a filename here — refuse anything that could escape the
    // staging directory rather than trusting it.
    let safe_id: String = app
        .package_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let dest = staging.join(format!("{safe_id}-{}.tapp", app.version.replace('/', "_")));

    let downloaded = fetch_verified(app, &dest, on_progress)?;

    install_optional_runtimes(app, &dest, on_progress);
    let payload_from = payload_source(app);
    let mut command = Command::new(tilo_install_bin());
    if change == Change::Install {
        command.args(install_argv(
            &dest,
            &app.publisher_pubkey_hex,
            &app.tapp_root_hash_hex,
            payload_from.as_deref(),
        ));
    } else {
        command.args([
            "update".to_string(),
            app.package_id.clone(),
            "--from".to_string(),
            dest.display().to_string(),
            "--require-publisher".to_string(),
            app.publisher_pubkey_hex.clone(),
            "--require-package-root".to_string(),
            app.tapp_root_hash_hex.clone(),
        ]);
        if let Some(url) = &payload_from {
            command.args(["--payload-from", url.as_str()]);
        }
        if change == Change::Rollback {
            command.arg("--allow-downgrade");
        }
    }
    on_progress(DownloadProgress {
        received: downloaded,
        total: Some(downloaded),
        phase: InstallPhase::Installing {
            elapsed: Duration::ZERO,
        },
    });
    let output = run_installer_with_progress(command, downloaded, on_progress);
    let _ = std::fs::remove_file(&dest);

    match output {
        Err(e) => Err(InstallError::InstallerMissing {
            detail: e.to_string(),
        }),
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let mut detail = String::new();
            detail.push_str(&String::from_utf8_lossy(&out.stderr));
            if detail.trim().is_empty() {
                detail.push_str(&String::from_utf8_lossy(&out.stdout));
            }
            Err(InstallError::Refused {
                code: out.status.code().unwrap_or(1),
                detail: installer_complaint(&detail),
            })
        }
    }
}

/// Make the runtime build `runtime` that `app` needs available: installed
/// already, or fetched from the registry that offers the app.
fn ensure_runtime(
    app: &CatalogueApp,
    runtime: &str,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<(), InstallError> {
    if crate::runtime_installed(runtime) {
        return Ok(());
    }
    let unavailable = || InstallError::RuntimeUnavailable {
        package_id: app.package_id.clone(),
        runtime: runtime.to_string(),
        registry: app.registry.clone(),
    };
    let offer = crate::runtime_offer(runtime, &app.registry)
        .map_err(|detail| InstallError::Download { detail })?
        .ok_or_else(unavailable)?;
    install_or_update_app_reporting(&offer, on_progress, Change::Install)?;
    if crate::runtime_installed(runtime) {
        Ok(())
    } else {
        Err(unavailable())
    }
}

/// Install the optional runtimes the downloaded app mounts when present —
/// a browser's engine — that its registry offers. The app starts without
/// one and says so, so a runtime that cannot be fetched now is reported and
/// does not stop the install.
fn install_optional_runtimes(
    app: &CatalogueApp,
    staged: &Path,
    on_progress: &mut dyn FnMut(DownloadProgress),
) {
    let Ok(file) = std::fs::File::open(staged) else {
        return;
    };
    let Ok(tapp) = tilo_app_package::tapp::read(
        std::io::BufReader::new(file),
        &mut tilo_app_package::DiscardBlobs,
    ) else {
        return;
    };
    for runtime in &tapp.meta.manifest.package.optional_runtimes {
        if crate::runtime_installed(runtime) {
            continue;
        }
        let outcome = crate::runtime_offer(runtime, &app.registry)
            .map_err(|detail| InstallError::Download { detail })
            .and_then(|offer| match offer {
                Some(offer) => {
                    install_or_update_app_reporting(&offer, on_progress, Change::Install)
                }
                None => Err(InstallError::NotOffered {
                    package_id: runtime.clone(),
                }),
            });
        if let Err(error) = outcome {
            eprintln!(
                "app: {} can use runtime {runtime}, which was not installed: {error}",
                app.package_id
            );
        }
    }
}

/// Download and atomically update an installed app from a verified offer.
///
/// # Errors
///
/// Returns an error if the package cannot be staged or downloaded, its signed
/// metadata does not match the downloaded bytes, or `tilo-install` cannot run
/// or refuses the update.
pub fn update_app_reporting(
    app: &CatalogueApp,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<(), InstallError> {
    install_or_update_app_reporting(app, on_progress, Change::Update)
}

/// Download and atomically replace an installed app with an older version
/// from a verified offer, keeping its saved state.
///
/// # Errors
///
/// Returns an error under the same conditions as [`update_app_reporting`].
pub fn rollback_app_reporting(
    app: &CatalogueApp,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<(), InstallError> {
    install_or_update_app_reporting(app, on_progress, Change::Rollback)
}

/// Resolve `package_id` against the live catalogue, then install it.
///
/// # Errors
///
/// Returns an error under the same conditions as [`install_reporting`].
pub fn install(package_id: &str) -> Result<CatalogueApp, InstallError> {
    install_reporting(package_id, &mut |_| {})
}

/// [`install`], reporting the download's progress as it happens.
///
/// # Errors
///
/// Returns [`InstallError::NotOffered`] if no configured registry
/// offers `package_id`, or the same errors as
/// [`install_app_reporting`] once resolved.
pub fn install_reporting(
    package_id: &str,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<CatalogueApp, InstallError> {
    let listing = crate::available();
    let app = listing
        .apps
        .into_iter()
        .find(|a| a.package_id == package_id)
        .ok_or_else(|| InstallError::NotOffered {
            package_id: package_id.to_string(),
        })?;
    install_app_reporting(&app, on_progress)?;
    Ok(app)
}

/// How much of the installer's complaint survives into a
/// notification; the toast is two lines tall, so more than this
/// clips mid-word, which reads worse than a short sentence.
const DETAIL_MAX_CHARS: usize = 110;

/// The installer's actual complaint, reduced to one readable line.
///
/// Only the **last** non-empty line survives (the lines above are
/// context the card has no room for); a leading `tilo-install:` is
/// stripped (the caller already says who refused it); anything still
/// over [`DETAIL_MAX_CHARS`] is elided rather than cut off mid-word.
fn installer_complaint(text: &str) -> String {
    let Some(line) = text.lines().map(str::trim).rfind(|l| !l.is_empty()) else {
        return String::new();
    };
    let line = line
        .strip_prefix("tilo-install:")
        .unwrap_or(line)
        .trim()
        .to_string();
    if line.chars().count() <= DETAIL_MAX_CHARS {
        return line;
    }
    let kept: String = line.chars().take(DETAIL_MAX_CHARS).collect();
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_installer_invocation_is_fixed_and_never_relaxes_signatures() {
        // Phase `a_release_build_runs_the_installer_from_path`.
        {
            use package_store::dev_env::dev_override_from;
            let gated = dev_override_from(false, INSTALL_BIN_ENV, Some("/tmp/evil-install".into()));
            assert_eq!(tilo_install_bin_from(gated), "tilo-install");
            let honoured =
                dev_override_from(true, INSTALL_BIN_ENV, Some("/dev/tilo-install".into()));
            assert_eq!(tilo_install_bin_from(honoured), "/dev/tilo-install");
        }
        // Phase `the_install_invocation_never_relaxes_signature_checking`.
        // The one argv assertion that is a security property rather than
        // a formatting detail.
        {
            let publisher = "ab".repeat(32);
            let root = "cd".repeat(32);
            let argv = install_argv(
                Path::new("/tmp/staged.tapp"),
                &publisher,
                &root,
                Some("https://registry.example"),
            );
            assert_eq!(argv[0], "install");
            assert_eq!(argv[1], "/tmp/staged.tapp");
            assert!(
                !argv.iter().any(|a| a == "--allow-self-signed"),
                "an install started from a search box must not opt out of \
                 the publisher signature check: {argv:?}"
            );
            assert!(
                argv.windows(2)
                    .any(|pair| pair[0] == "--require-package-root" && pair[1] == root),
                "the installer must pin the signed registry package root: {argv:?}"
            );
            assert!(
                argv.windows(2)
                    .any(|pair| pair[0] == "--require-publisher" && pair[1] == publisher),
                "the installer must pin the signed registry publisher: {argv:?}"
            );
            assert!(
                !argv.iter().any(|a| a.starts_with("--require-attestor")),
                "attestor policy is the user's to set, not this path's: {argv:?}"
            );
            assert!(
                argv.windows(2).any(
                    |pair| pair[0] == "--payload-from" && pair[1] == "https://registry.example"
                ),
                "a thin package's payload comes from the offering registry: {argv:?}"
            );
        }
    }

    fn registry_app_for(bytes: &[u8]) -> CatalogueApp {
        CatalogueApp {
            package_id: "so.tilo.test".into(),
            display_name: "Test".into(),
            description: String::new(),
            tagline: String::new(),
            images: Vec::new(),
            supported_languages: vec!["en".into()],
            category: None,
            version: "1.0.0".into(),
            registry: "test".into(),
            download_url: "https://example.invalid/test.tapp".into(),
            tapp_root_hash_hex: "cd".repeat(32),
            tapp_size_bytes: bytes.len() as u64,
            tapp_blake3_hex: blake3::hash(bytes).to_hex().to_string(),
            architecture: "x86_64".into(),
            isa: "x86_64-v3".into(),
            runtime: None,
            publisher_pubkey_hex: "ab".repeat(32),
            trust: "verified".into(),
        }
    }

    #[test]
    fn package_downloads_are_explicit_bounded_and_match_signed_metadata() {
        // Phase `staged_download_must_match_the_signed_registry_metadata`.
        {
            let bytes = b"signed package bytes";
            let app = registry_app_for(bytes);
            verify_download_metadata(&app, bytes.len() as u64).expect("matching metadata");

            let mut malformed_root = app.clone();
            malformed_root.tapp_root_hash_hex = "00".repeat(31);
            assert!(matches!(
                verify_download_metadata(&malformed_root, bytes.len() as u64),
                Err(InstallError::Integrity { detail }) if detail.contains("not 32 bytes")
            ));
            assert!(matches!(
                verify_download_metadata(&app, bytes.len() as u64 + 1),
                Err(InstallError::Integrity { detail }) if detail.contains("size mismatch")
            ));

            let mut oversized = app;
            oversized.tapp_size_bytes = tilo_app_package::MAX_APP_TAPP_BYTES + 1;
            assert!(matches!(
                validate_offer_metadata(&oversized),
                Err(InstallError::Integrity { detail }) if detail.contains("outside")
            ));
        }
        // Phase `streaming_download_stops_when_body_exceeds_the_offer_limit`.
        {
            use std::io::{Read as _, Write as _};
            use std::net::TcpListener;

            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let address = listener.local_addr().expect("listener address");
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept request");
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request);
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n12345678")
                    .expect("write response");
            });
            let temp = tempfile::tempdir().expect("temp dir");
            let destination = temp.path().join("bounded.tapp");

            let error = download_reporting_limited(
                &format!("http://{address}/app.tapp"),
                &destination,
                4,
                &mut |_| {},
            )
            .expect_err("an undeclared oversized body must stop mid-stream");
            server.join().expect("server thread");
            assert!(error.contains("exceeded"), "{error}");
            assert_eq!(
                std::fs::metadata(destination).expect("staging file").len(),
                0,
                "the over-limit chunk is rejected before it reaches disk"
            );
        }
        // Phase `package_download_origin_is_explicit_and_credential_free`.
        {
            assert!(
                safe_download_url("https://apps.tilo.so/artifacts/app.tapp?token=signed").is_ok()
            );
            assert!(safe_download_url("http://127.0.0.1:8080/app.tapp").is_ok());
            for unsafe_url in [
                "http://apps.tilo.so/artifacts/app.tapp",
                "https://user:secret@apps.tilo.so/artifacts/app.tapp",
                "file:///tmp/app.tapp",
                "https://apps.tilo.so/artifacts/app.tapp#alternate",
            ] {
                assert!(
                    safe_download_url(unsafe_url).is_err(),
                    "unsafe package origin was accepted: {unsafe_url}"
                );
            }
        }
        // Phase `package_download_refuses_redirect_response`.
        {
            use std::io::{Read as _, Write as _};
            use std::net::TcpListener;

            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let address = listener.local_addr().expect("listener address");
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept request");
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request);
                stream
                    .write_all(
                        b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .expect("write redirect");
            });
            let temp = tempfile::tempdir().expect("temp dir");
            let error = download_reporting_limited(
                &format!("http://{address}/app.tapp"),
                &temp.path().join("redirect.tapp"),
                64,
                &mut |_| {},
            )
            .expect_err("redirect must not become a second authorized origin");
            server.join().expect("server thread");
            assert!(error.contains("redirects are not followed"), "{error}");
        }
    }

    /// A loopback host answering `GET <path>` from `objects` and 404 for
    /// anything else, recording every path asked for.
    fn package_host(
        objects: std::collections::HashMap<String, Vec<u8>>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let address = listener.local_addr().expect("listener address");
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = asked.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut request = [0u8; 2048];
                let read = stream.read(&mut request).unwrap_or(0);
                let text = String::from_utf8_lossy(&request[..read]).to_string();
                let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push(path.clone());
                let reply = match objects.get(&path) {
                    Some(body) => {
                        let mut reply = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .into_bytes();
                        reply.extend_from_slice(body);
                        reply
                    }
                    None => {
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_vec()
                    }
                };
                let _ = stream.write_all(&reply);
            }
        });
        (format!("http://{address}"), asked)
    }

    #[test]
    fn compressed_tapps_are_bounded_and_verified_as_the_signed_bytes() {
        use tilo_app_package::compressed;
        let package = b"signed package bytes, compressed for the wire ".repeat(256);
        let twin = compressed::compress(&package).expect("compress");
        let temp = tempfile::tempdir().expect("temp dir");
        let dest = temp.path().join("staged.tapp");

        // Phase `compressed_tapp_download_verifies_the_uncompressed_hash`:
        // the twin is fetched instead of the package, decompressed, and the
        // plain bytes pass the signed length and hash; progress counts the
        // compressed bytes that crossed the network.
        {
            let (base, asked) = package_host(std::collections::HashMap::from([
                ("/app.tapp.zst".to_string(), twin.clone()),
                ("/app.tapp".to_string(), b"never fetched".to_vec()),
            ]));
            let mut app = registry_app_for(&package);
            app.download_url = format!("{base}/app.tapp");
            let mut wire = 0;
            let len = fetch_verified(&app, &dest, &mut |progress| wire = progress.received)
                .expect("the twin decompresses to the signed package");
            assert_eq!(len, package.len() as u64);
            assert_eq!(std::fs::read(&dest).expect("staged"), package);
            assert_eq!(wire, twin.len() as u64, "progress is wire bytes");
            assert_eq!(*asked.lock().unwrap(), ["/app.tapp.zst"]);
        }
        // Phase `compressed_tapp_beyond_the_signed_size_is_refused`: one
        // byte more than the entry signed is a bomb, however small the twin.
        {
            let mut longer = package.clone();
            longer.push(b'!');
            let (base, _) = package_host(std::collections::HashMap::from([(
                "/app.tapp.zst".to_string(),
                compressed::compress(&longer).expect("compress"),
            )]));
            let mut app = registry_app_for(&package);
            app.download_url = format!("{base}/app.tapp");
            let error = fetch_verified(&app, &dest, &mut |_| {}).expect_err("refused");
            assert!(
                matches!(&error, InstallError::Download { detail } if detail.contains("bound")),
                "{error:?}"
            );
            assert!(!dest.exists(), "a refused download leaves nothing staged");

            let (base, _) = package_host(std::collections::HashMap::from([(
                "/bomb.tapp.zst".to_string(),
                compressed::compress(&vec![0; 64 << 20]).expect("compress"),
            )]));
            app.download_url = format!("{base}/bomb.tapp");
            assert!(fetch_verified(&app, &dest, &mut |_| {}).is_err());
            assert!(!dest.exists());
        }
        // Phase `a_twin_of_other_bytes_fails_the_signed_hash`.
        {
            let mut other = package.clone();
            other[0] ^= 1;
            let (base, _) = package_host(std::collections::HashMap::from([(
                "/app.tapp.zst".to_string(),
                compressed::compress(&other).expect("compress"),
            )]));
            let mut app = registry_app_for(&package);
            app.download_url = format!("{base}/app.tapp");
            let error = fetch_verified(&app, &dest, &mut |_| {}).expect_err("refused");
            assert!(
                matches!(&error, InstallError::Integrity { detail } if detail.contains("hash")),
                "{error:?}"
            );
            assert!(!dest.exists());
        }
        // Phase `a_host_without_twins_serves_the_raw_package`: what every
        // package published before twins, and every other host, looks like.
        {
            let (base, asked) = package_host(std::collections::HashMap::from([(
                "/app.tapp".to_string(),
                package.clone(),
            )]));
            let mut app = registry_app_for(&package);
            app.download_url = format!("{base}/app.tapp");
            fetch_verified(&app, &dest, &mut |_| {}).expect("raw fallback");
            assert_eq!(std::fs::read(&dest).expect("staged"), package);
            assert_eq!(*asked.lock().unwrap(), ["/app.tapp.zst", "/app.tapp"]);
        }
        // Phase `a_signed_url_with_a_query_is_fetched_as_signed`: a suffix
        // would land in the query, so no twin is asked for.
        {
            let parsed = reqwest::Url::parse("https://apps.tilo.so/a/app.tapp?token=1").unwrap();
            assert_eq!(twin_url(&parsed), None);
            let parsed = reqwest::Url::parse("https://apps.tilo.so/a/app.tapp").unwrap();
            assert_eq!(
                twin_url(&parsed).map(String::from).as_deref(),
                Some("https://apps.tilo.so/a/app.tapp.zst")
            );
        }
    }

    #[test]
    fn installer_errors_render_one_actionable_line() {
        // Phase `errors_render_something_a_person_can_act_on`.
        {
            let cases = [
                InstallError::NotOffered {
                    package_id: "so.tilo.ghost".into(),
                },
                InstallError::NoStagingDir,
                InstallError::Download {
                    detail: "connection refused".into(),
                },
                InstallError::InstallerMissing {
                    detail: "No such file or directory".into(),
                },
                InstallError::Refused {
                    code: 2,
                    detail: "publisher signature did not verify".into(),
                },
                InstallError::Integrity {
                    detail: "package root mismatch".into(),
                },
            ];
            for c in &cases {
                let msg = c.user_message();
                assert!(!msg.trim().is_empty(), "{c:?} rendered nothing");
                assert!(
                    msg.chars()
                        .next()
                        .is_some_and(|ch| ch.is_lowercase() || ch.is_alphanumeric()),
                    "{c:?} → {msg:?} should read as a sentence fragment"
                );
            }
            assert!(
                InstallError::Refused {
                    code: 2,
                    detail: "publisher signature did not verify".into()
                }
                .user_message()
                .contains("publisher signature"),
                "the installer's own reason has to survive to the user"
            );
        }
        // Phase `a_silent_refusal_still_reports_the_exit_code`.
        // An empty installer complaint must still say *something* — "tilo-install refused it" with a code beats a blank line.
        {
            let msg = InstallError::Refused {
                code: 7,
                detail: "   \n  ".into(),
            }
            .user_message();
            assert!(msg.contains('7'), "{msg:?}");
        }
        // Phase `the_complaint_is_the_last_line_and_nothing_else`.
        {
            let text = "reading header\n\nverifying\ntapp parse: unexpected EOF\n";
            assert_eq!(installer_complaint(text), "tapp parse: unexpected EOF");
            assert_eq!(installer_complaint(""), "");
            assert_eq!(installer_complaint("   \n\n  "), "");
        }
        // Phase `the_installers_own_name_prefix_is_stripped`.
        // The rendered toast already says who refused it, so the installer's
        // own name prefix would stutter — caught by looking at the rendered notification.
        {
            let msg = InstallError::Refused {
                code: 1,
                detail: installer_complaint("tilo-install: tapp parse: unexpected EOF during skip"),
            }
            .user_message();
            assert_eq!(
                msg,
                "tilo-install refused it: tapp parse: unexpected EOF during skip"
            );
            assert!(
                !msg.contains("tilo-install: "),
                "the sentence must not name the installer twice: {msg:?}"
            );
        }
        // Phase `an_overlong_complaint_is_elided_rather_than_clipped`.
        // The notification card is two lines tall; a longer complaint is
        // elided here rather than cut off mid-word by the renderer.
        {
            let long = "x".repeat(400);
            let out = installer_complaint(&long);
            assert!(
                out.chars().count() <= DETAIL_MAX_CHARS + 1,
                "elided to {} chars",
                out.chars().count()
            );
            assert!(out.ends_with('…'));
        }
    }
}
