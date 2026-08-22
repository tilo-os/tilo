//! The app catalog — what the palette offers when the app you typed is
//! **not** installed.
//!
//! `launcher::gui_apps()` answers "what is on this machine". This module
//! answers "what could be": it reads the same `registries.toml` the Store
//! app uses, fetches each registry's `/entries`, and keeps a snapshot the
//! catalog search provider reads on the main thread without blocking.
//!
//! Picking a catalog row runs the Store's own fetch-on-demand sequence —
//! download the `.qpkg`, hand it to `qos-install install` (which
//! recomputes the content hash and checks the publisher signature
//! fail-closed before extracting a byte), then launch — except that the
//! launch is the shell's own, so the window lands where the palette said
//! it would and in the workspace's sandbox + room, exactly like an
//! installed app.
//!
//! Everything slow happens on worker threads. Results come back as
//! [`InstallEvent`]s the chrome drains once per frame, mirroring
//! [`crate::shell::launcher::drain_events`].

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use registry_types::{RegistryEntry, verify_entry_signature};
use serde::Deserialize;

/// One installable app, as the palette sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogApp {
    /// Manifest package id — the same namespace `launcher::InstalledApp`
    /// uses, so "is it installed?" is a plain id comparison.
    pub package_id: String,
    pub display_name: String,
    pub categories: Vec<String>,
    /// The newest version the registry lists (first in its list, as the
    /// Store ranks them).
    pub version: String,
    pub qpkg_url: String,
    pub qpkg_size_bytes: u64,
    /// Registry's short name, for the row and the notification.
    pub registry_name: String,
}

impl CatalogApp {
    pub fn category(&self) -> Option<&str> {
        self.categories.first().map(String::as_str)
    }
}

// ---------------------------------------------------------------------
// registries.toml — shared with the Store app.
// ---------------------------------------------------------------------

/// One configured registry. Field names match the Store's
/// `apps/store/src/config.rs` so one file serves both.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct RegistryConfig {
    pub name: String,
    pub base_url: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Default, Deserialize)]
struct RegistriesFile {
    #[serde(default, rename = "registry")]
    registries: Vec<RegistryConfig>,
}

/// The Store's config directory: `$QOS_STORE_CONFIG_DIR`, else
/// `$XDG_CONFIG_HOME/qos-store`. Read-only from here — the shell never
/// seeds a default registry; that is the Store's decision to make.
pub fn registries_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("QOS_STORE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("registries.toml"));
    }
    dirs::config_dir().map(|d| d.join("qos-store").join("registries.toml"))
}

/// Enabled registries from `registries.toml`, or nothing when the file
/// is absent or unparsable. A missing file is the common case on a
/// fresh machine and is not an error: the palette just has no catalog.
pub fn enabled_registries() -> Vec<RegistryConfig> {
    let Some(path) = registries_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    parse_registries(&text)
}

pub fn parse_registries(text: &str) -> Vec<RegistryConfig> {
    let Ok(file) = toml::from_str::<RegistriesFile>(text) else {
        return Vec::new();
    };
    file.registries.into_iter().filter(|r| r.enabled).collect()
}

// ---------------------------------------------------------------------
// /entries → CatalogApp
// ---------------------------------------------------------------------

/// `GET /entries` body: a bare array, or `{ "entries": [...] }`. Both
/// shapes are accepted, as the Store does.
#[derive(Deserialize)]
#[serde(untagged)]
enum EntriesResponse {
    Bare(Vec<RegistryEntry>),
    Wrapped { entries: Vec<RegistryEntry> },
}

/// Decode an `/entries` body into catalog rows.
///
/// An entry whose publisher signature does not verify is dropped, not
/// listed: the palette has no room for a trust badge, and a row that
/// says "Install" is a promise the shell should only make for an entry
/// that is at least what its publisher signed. (`qos-install` checks the
/// package itself again, independently, before extracting.) Entries with
/// no versions are dropped too — there is nothing to download.
pub fn parse_entries(body: &str, registry_name: &str) -> Vec<CatalogApp> {
    let Ok(resp) = serde_json::from_str::<EntriesResponse>(body) else {
        return Vec::new();
    };
    let entries = match resp {
        EntriesResponse::Bare(v) => v,
        EntriesResponse::Wrapped { entries } => entries,
    };
    entries
        .into_iter()
        .filter_map(|e| catalog_app_from_entry(&e, registry_name))
        .collect()
}

fn catalog_app_from_entry(entry: &RegistryEntry, registry_name: &str) -> Option<CatalogApp> {
    if !verify_entry_signature(entry) {
        tracing::warn!(
            target: "shell::app_catalog",
            package_id = %entry.package_id,
            "dropping registry entry: publisher signature does not verify"
        );
        return None;
    }
    let latest = entry.versions.first()?;
    if entry.package_id.trim().is_empty() || latest.qpkg_url.trim().is_empty() {
        return None;
    }
    let display_name = if entry.name.trim().is_empty() {
        crate::shell::launcher::display_name_for_package_id(&entry.package_id)
    } else {
        entry.name.clone()
    };
    Some(CatalogApp {
        package_id: entry.package_id.clone(),
        display_name,
        categories: entry.categories.clone(),
        version: latest.version.clone(),
        qpkg_url: latest.qpkg_url.clone(),
        qpkg_size_bytes: latest.qpkg_size_bytes,
        registry_name: registry_name.to_string(),
    })
}

fn http_client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent("qos-shell/0.1")
        .timeout(timeout)
        .build()
        .map_err(|e| format!("http client: {e}"))
}

/// Fetch one registry's catalogue. Blocking; worker thread only.
pub fn fetch_registry(reg: &RegistryConfig) -> Result<Vec<CatalogApp>, String> {
    let url = format!("{}/entries", reg.base_url.trim_end_matches('/'));
    let client = http_client(Duration::from_secs(10))?;
    let text = client
        .get(&url)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
        .map_err(|e| format!("{url}: {e}"))?;
    Ok(parse_entries(&text, &reg.name))
}

// ---------------------------------------------------------------------
// The snapshot the provider reads.
// ---------------------------------------------------------------------

/// Refresh cadence for a catalog that loaded once. A registry changes
/// on the order of releases, not keystrokes.
const REFRESH_EVERY: Duration = Duration::from_secs(10 * 60);

/// Cheap handle to the latest fetched catalogue.
#[derive(Clone, Default)]
pub struct CatalogHandle {
    apps: Arc<Mutex<Vec<CatalogApp>>>,
}

impl CatalogHandle {
    /// An empty catalogue that never refreshes. What tests and headless
    /// scenario runs get, and what a machine with no `registries.toml`
    /// effectively has.
    pub fn empty() -> Self {
        Self::default()
    }

    /// A catalogue pre-filled with `apps`, for tests.
    pub fn with_apps(apps: Vec<CatalogApp>) -> Self {
        Self {
            apps: Arc::new(Mutex::new(apps)),
        }
    }

    /// The current snapshot. Never blocks on the network — only on the
    /// snapshot mutex, which the refresher holds for one `Vec` swap.
    pub fn apps(&self) -> Vec<CatalogApp> {
        self.apps.lock().expect("catalog poisoned").clone()
    }

    pub fn replace(&self, apps: Vec<CatalogApp>) {
        *self.apps.lock().expect("catalog poisoned") = apps;
    }

    /// Start the background refresher and return the handle it fills.
    ///
    /// Does nothing — and spawns nothing — when there is no
    /// `registries.toml` or `QOS_CATALOG_OFFLINE` is set, so a unit test
    /// that builds the overlay host never reaches the network.
    pub fn spawn_refresher() -> Self {
        let handle = Self::default();
        if std::env::var_os("QOS_CATALOG_OFFLINE").is_some() {
            return handle;
        }
        let registries = enabled_registries();
        if registries.is_empty() {
            return handle;
        }
        let sink = handle.clone();
        let builder = std::thread::Builder::new().name("qos-app-catalog".to_string());
        let spawned = builder.spawn(move || loop {
            let mut all = Vec::new();
            for reg in &registries {
                match fetch_registry(reg) {
                    Ok(mut apps) => all.append(&mut apps),
                    Err(e) => tracing::warn!(
                        target: "shell::app_catalog",
                        registry = %reg.name,
                        "catalog fetch failed: {e}"
                    ),
                }
            }
            // Keep the last good snapshot across a transient outage:
            // an empty answer would make every catalog row vanish
            // mid-session for no user-visible reason.
            if !all.is_empty() {
                sink.replace(all);
            }
            std::thread::sleep(REFRESH_EVERY);
        });
        if let Err(e) = spawned {
            tracing::warn!(target: "shell::app_catalog", "could not start catalog thread: {e}");
        }
        handle
    }
}

// ---------------------------------------------------------------------
// Install: download → qos-install → event.
// ---------------------------------------------------------------------

/// What happened to an install after it left the frame thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallEvent {
    pub package_id: String,
    pub display_name: String,
    pub kind: InstallEventKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallEventKind {
    /// `qos-install` verified and cached the package; the chrome should
    /// launch it now.
    Installed,
    /// Download or install failed. `detail` is the reason, in words a
    /// notification can quote.
    Failed { detail: String },
}

static EVENTS: Mutex<Vec<InstallEvent>> = Mutex::new(Vec::new());
/// Package ids with an install in flight — a second Enter on the same
/// row must not start a second download.
static IN_FLIGHT: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn push_event(event: InstallEvent) {
    EVENTS.lock().expect("EVENTS poisoned").push(event);
}

/// Take every install event queued since the last call. Once per frame,
/// by the chrome.
pub fn drain_events() -> Vec<InstallEvent> {
    std::mem::take(&mut *EVENTS.lock().expect("EVENTS poisoned"))
}

pub fn is_installing(package_id: &str) -> bool {
    IN_FLIGHT
        .lock()
        .expect("IN_FLIGHT poisoned")
        .iter()
        .any(|p| p == package_id)
}

fn mark_in_flight(package_id: &str) -> bool {
    let mut set = IN_FLIGHT.lock().expect("IN_FLIGHT poisoned");
    if set.iter().any(|p| p == package_id) {
        return false;
    }
    set.push(package_id.to_string());
    true
}

fn clear_in_flight(package_id: &str) {
    IN_FLIGHT
        .lock()
        .expect("IN_FLIGHT poisoned")
        .retain(|p| p != package_id);
}

/// Where a downloaded `.qpkg` waits for `qos-install`. The Store's
/// staging dir, so the two never hold two copies of one package.
pub fn staging_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("QOS_STORE_CACHE_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => dirs::cache_dir()?.join("qos-store"),
    };
    let staged = base.join("staged-qpkg");
    std::fs::create_dir_all(&staged).ok()?;
    Some(staged)
}

/// Where `qos-install` lives: `QOS_INSTALL_BIN` (dev trees and tests),
/// then beside the compositor binary, then the system paths — the same
/// order [`crate::shell::launcher::resolve_launcher_bin`] uses for
/// `qos-launcher`, for the same reason: a dev tree has both binaries in
/// `target/debug/`, a shipped image has both in `/usr/libexec`.
pub fn resolve_install_bin() -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Some(explicit) = std::env::var_os("QOS_INSTALL_BIN") {
        if !explicit.is_empty() {
            candidates.push(PathBuf::from(explicit));
        }
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        candidates.push(dir.join("qos-install"));
    }
    candidates.push(PathBuf::from("/usr/libexec/qos-install"));
    candidates.push(PathBuf::from("/usr/bin/qos-install"));
    candidates
        .iter()
        .find(|c| c.exists())
        .cloned()
        .ok_or_else(|| {
            let list: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
            format!("qos-install not found (looked in {})", list.join(", "))
        })
}

/// The exact `qos-install` invocation for a staged package.
///
/// `--allow-self-signed` and `--force` match the Store app's plan
/// (`apps/store/src/actions.rs`): publisher identities are self-signed
/// during bring-up, and `--force` lets a re-fetch replace a stale cache
/// entry. The content hash and the publisher signature are still
/// verified; what is skipped is the attestor allowlist.
pub fn install_args(staged: &Path) -> Vec<String> {
    vec![
        "install".into(),
        staged.to_string_lossy().into_owned(),
        "--allow-self-signed".into(),
        "--force".into(),
    ]
}

/// Download `url` to `dest`. The bytes are not trusted here —
/// `qos-install` re-verifies them — but an empty body is refused early
/// because it would only produce a confusing install error later.
pub fn download_qpkg(url: &str, dest: &Path) -> Result<u64, String> {
    let client = http_client(Duration::from_secs(120))?;
    let bytes = client
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes())
        .map_err(|e| format!("download {url}: {e}"))?;
    if bytes.is_empty() {
        return Err(format!("download {url}: empty body"));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    std::fs::write(dest, &bytes).map_err(|e| format!("write {}: {e}", dest.display()))?;
    Ok(bytes.len() as u64)
}

/// Run the whole install on the calling thread, with the download
/// pluggable so tests can stage a package without a server.
///
/// Returns what the chrome should hear. The staged file is removed
/// afterwards either way — once installed the bytes live in the
/// content-addressed store; on failure a half-written file must not be
/// retried as if it were whole.
pub fn run_install(
    app: &CatalogApp,
    staging: &Path,
    install_bin: &Path,
    fetch: impl FnOnce(&str, &Path) -> Result<u64, String>,
) -> InstallEventKind {
    let staged = staging.join(format!("{}-{}.qpkg", app.package_id, app.version));
    if let Err(detail) = fetch(&app.qpkg_url, &staged) {
        let _ = std::fs::remove_file(&staged);
        return InstallEventKind::Failed { detail };
    }
    let output = Command::new(install_bin).args(install_args(&staged)).output();
    let _ = std::fs::remove_file(&staged);
    match output {
        Ok(out) if out.status.success() => InstallEventKind::Installed,
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stderr).into_owned();
            if text.trim().is_empty() {
                text = String::from_utf8_lossy(&out.stdout).into_owned();
            }
            let last = text
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_string())
                .unwrap_or_else(|| format!("qos-install exited with status {}", out.status));
            InstallEventKind::Failed { detail: last }
        }
        Err(e) => InstallEventKind::Failed {
            detail: format!("could not run {}: {e}", install_bin.display()),
        },
    }
}

/// Start installing `app` on a worker thread. The result arrives through
/// [`drain_events`]. Returns `false` (and does nothing) when the same
/// package is already being installed.
pub fn install(app: CatalogApp) -> bool {
    if !mark_in_flight(&app.package_id) {
        return false;
    }
    let builder = std::thread::Builder::new().name("qos-app-install".to_string());
    let identity = (app.package_id.clone(), app.display_name.clone());
    let spawned = builder.spawn(move || {
        let kind = match (staging_dir(), resolve_install_bin()) {
            (Some(staging), Ok(bin)) => run_install(&app, &staging, &bin, download_qpkg),
            (None, _) => InstallEventKind::Failed {
                detail: "no cache directory to stage the download in".to_string(),
            },
            (_, Err(detail)) => InstallEventKind::Failed { detail },
        };
        clear_in_flight(&app.package_id);
        push_event(InstallEvent {
            package_id: app.package_id,
            display_name: app.display_name,
            kind,
        });
    });
    if let Err(e) = spawned {
        clear_in_flight(&identity.0);
        push_event(InstallEvent {
            package_id: identity.0,
            display_name: identity.1,
            kind: InstallEventKind::Failed {
                detail: format!("could not start install thread: {e}"),
            },
        });
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use registry_types::{RegistryVersion, sign_entry};

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn entry(id: &str, name: &str, version: &str) -> RegistryEntry {
        let key = signing_key();
        let mut e = RegistryEntry {
            package_id: id.to_string(),
            name: name.to_string(),
            description: String::new(),
            categories: vec!["Office".to_string()],
            icon_url: None,
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: hex::encode(key.verifying_key().to_bytes()),
            versions: vec![RegistryVersion {
                version: version.to_string(),
                qpkg_url: format!("http://registry.test/{id}-{version}.qpkg"),
                qpkg_root_hash_hex: "00".repeat(32),
                qpkg_size_bytes: 1234,
                released_at_unix: 1,
            }],
            updated_at_unix: 1,
            signature_hex: String::new(),
        };
        sign_entry(&mut e, &key);
        e
    }

    #[test]
    fn registries_file_keeps_only_enabled_registries() {
        let text = r#"
[[registry]]
name = "main"
base_url = "https://registry.example/"

[[registry]]
name = "off"
base_url = "https://off.example"
enabled = false
"#;
        let regs = parse_registries(text);
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].name, "main");
        assert!(regs[0].enabled);
    }

    #[test]
    fn unparsable_registries_file_yields_nothing() {
        assert!(parse_registries("this is not toml = = =").is_empty());
        assert!(parse_registries("").is_empty());
    }

    #[test]
    fn entries_decode_from_bare_and_wrapped_bodies() {
        let e = entry("dev.qos.word-processor", "Word Processor", "2026.08");
        let bare = serde_json::to_string(&vec![e.clone()]).unwrap();
        let wrapped = serde_json::json!({ "entries": [e] }).to_string();
        for body in [bare, wrapped] {
            let apps = parse_entries(&body, "local");
            assert_eq!(apps.len(), 1, "body: {body}");
            let app = &apps[0];
            assert_eq!(app.package_id, "dev.qos.word-processor");
            assert_eq!(app.display_name, "Word Processor");
            assert_eq!(app.version, "2026.08");
            assert_eq!(app.category(), Some("Office"));
            assert_eq!(app.registry_name, "local");
            assert!(app.qpkg_url.ends_with(".qpkg"));
        }
    }

    /// A row that says "Install" is a promise the shell only makes for
    /// what the publisher signed: a tampered entry is not listed at all.
    #[test]
    fn an_entry_whose_signature_fails_is_dropped() {
        let mut e = entry("dev.qos.evil", "Evil", "1");
        e.name = "Totally Fine".to_string(); // mutate after signing
        let body = serde_json::to_string(&vec![e]).unwrap();
        assert!(parse_entries(&body, "local").is_empty());
    }

    #[test]
    fn an_entry_without_versions_is_dropped() {
        let key = signing_key();
        let mut e = entry("dev.qos.empty", "Empty", "1");
        e.versions.clear();
        sign_entry(&mut e, &key);
        let body = serde_json::to_string(&vec![e]).unwrap();
        assert!(parse_entries(&body, "local").is_empty());
    }

    #[test]
    fn a_missing_name_falls_back_to_the_package_id() {
        let key = signing_key();
        let mut e = entry("dev.qos.hex-editor", "", "1");
        sign_entry(&mut e, &key);
        let body = serde_json::to_string(&vec![e]).unwrap();
        let apps = parse_entries(&body, "local");
        assert_eq!(apps[0].display_name, "Hex Editor");
    }

    #[test]
    fn install_args_match_the_store_plan() {
        let args = install_args(Path::new("/tmp/x.qpkg"));
        assert_eq!(args, ["install", "/tmp/x.qpkg", "--allow-self-signed", "--force"]);
    }

    fn app() -> CatalogApp {
        CatalogApp {
            package_id: "dev.qos.word-processor".into(),
            display_name: "Word Processor".into(),
            categories: vec![],
            version: "2026.08".into(),
            qpkg_url: "http://registry.test/wp.qpkg".into(),
            qpkg_size_bytes: 3,
            registry_name: "local".into(),
        }
    }

    /// Write an executable script standing in for `qos-install`. It
    /// records its argv and exits with `code`.
    fn fake_install_bin(dir: &Path, code: i32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let log = dir.join("argv.log");
        let bin = dir.join("qos-install");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\necho 'signature: publisher key not trusted' >&2\nexit {code}\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    #[test]
    fn a_successful_install_stages_downloads_runs_qos_install_and_cleans_up() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = fake_install_bin(tmp.path(), 0);
        let staging = tmp.path().join("staged");
        let kind = run_install(&app(), &staging, &bin, |url, dest| {
            assert_eq!(url, "http://registry.test/wp.qpkg");
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(dest, b"pkg").unwrap();
            Ok(3)
        });
        assert_eq!(kind, InstallEventKind::Installed);
        let argv = std::fs::read_to_string(tmp.path().join("argv.log")).unwrap();
        let lines: Vec<&str> = argv.lines().collect();
        assert_eq!(lines[0], "install");
        assert!(lines[1].ends_with("dev.qos.word-processor-2026.08.qpkg"), "{argv}");
        assert_eq!(&lines[2..], ["--allow-self-signed", "--force"]);
        assert!(
            std::fs::read_dir(&staging).unwrap().next().is_none(),
            "the staged package is removed once qos-install has it"
        );
    }

    #[test]
    fn a_failed_install_reports_qos_installs_last_line() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = fake_install_bin(tmp.path(), 3);
        let kind = run_install(&app(), &tmp.path().join("staged"), &bin, |_, dest| {
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(dest, b"pkg").unwrap();
            Ok(3)
        });
        assert_eq!(
            kind,
            InstallEventKind::Failed {
                detail: "signature: publisher key not trusted".to_string()
            }
        );
    }

    #[test]
    fn a_failed_download_never_runs_qos_install() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = fake_install_bin(tmp.path(), 0);
        let kind = run_install(&app(), &tmp.path().join("staged"), &bin, |url, _| {
            Err(format!("download {url}: connection refused"))
        });
        assert_eq!(
            kind,
            InstallEventKind::Failed {
                detail: "download http://registry.test/wp.qpkg: connection refused".to_string()
            }
        );
        assert!(!tmp.path().join("argv.log").exists(), "qos-install must not run");
    }

    /// The real download path against a real socket: a one-shot HTTP
    /// server on loopback serves the bytes, and they land on disk
    /// intact. No mock client.
    #[test]
    fn download_writes_the_served_bytes() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let payload = b"QPKG-BYTES".to_vec();
        let served = payload.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                served.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&served).unwrap();
        });
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("staged").join("x.qpkg");
        let n = download_qpkg(&format!("http://127.0.0.1:{port}/x.qpkg"), &dest).unwrap();
        server.join().unwrap();
        assert_eq!(n, payload.len() as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
    }

    #[test]
    fn in_flight_guard_refuses_a_second_install_of_the_same_package() {
        assert!(mark_in_flight("dev.qos.guard-test"));
        assert!(is_installing("dev.qos.guard-test"));
        assert!(!mark_in_flight("dev.qos.guard-test"));
        clear_in_flight("dev.qos.guard-test");
        assert!(!is_installing("dev.qos.guard-test"));
    }
}
