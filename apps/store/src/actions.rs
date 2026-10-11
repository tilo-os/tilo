//! Store action dispatch. Every privileged operation crosses the
//! launcher-issued app-host capability; this sandbox never discovers or
//! executes host package-management binaries.
//!
//! The Tilo side deliberately has **no "Install" button**. `Open`
//! fetches the package the first time and launches it every time;
//! `Keep on device` (pin) is the separate, optional step that keeps an
//! app and ranks it first. app-host binds the download to the signed
//! catalogue's root, publisher, app id, version, and size before extracting.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::model::{ActionKind, StoreItem, StoreSource};

/// The outcome of a fully-executed [`execute`] call: whether the last
/// meaningful step succeeded and the human-readable status the status
/// bar should show.
#[derive(Clone, Debug)]
pub struct ActionOutcome {
    pub ok: bool,
    pub status_line: String,
    pub launch: Option<ApplicationLaunch>,
}

/// A prepared installed application, launched through the Store's original
/// authenticated workspace channel rather than the package custodian.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationLaunch {
    pub app_id: String,
    pub package_root: Option<String>,
}

impl ActionOutcome {
    fn ok(msg: impl Into<String>) -> Self {
        Self {
            ok: true,
            status_line: msg.into(),
            launch: None,
        }
    }
    fn fail(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            status_line: msg.into(),
            launch: None,
        }
    }
}

/// The staging directory for downloaded `.tapp`s. Lives under the
/// store's cache dir so a container-mounted read-only home isn't a
/// problem. Best-effort creation.
pub fn tapp_staging_dir() -> Option<PathBuf> {
    let base = crate::config::cache_dir()?;
    let staged = base.join("staged-tapp");
    let _ = std::fs::create_dir_all(&staged);
    Some(staged)
}

// ---- download ------------------------------------------------------

/// HTTP-download a `.tapp` to `dest`. Blocking — runs on the worker
/// thread. The bytes are *not* trusted here: app-host
/// re-verifies the content hash + publisher signature before it
/// extracts anything, so a truncated or tampered download fails
/// closed at the install step.
///
/// # Errors
///
/// Returns an error message if the HTTP client can't be built, the request
/// fails or comes back with a non-success status or an empty body, or `dest`
/// (or its parent directory) can't be written.
pub fn download_tapp(url: &str, dest: &Path, expected_size: u64) -> Result<u64, String> {
    const MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;
    if expected_size == 0 || expected_size > MAX_PACKAGE_BYTES {
        return Err(format!(
            "registry package size {expected_size} is outside the accepted range"
        ));
    }
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid package URL: {e}"))?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err("package URL must be HTTPS without credentials or a fragment".into());
    }
    let client = tilo_net::http::blocking_client()
        .user_agent("tilo-store/0.1")
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut response = client
        .get(parsed)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|e| format!("download {url}: {e}"))?;
    if response.status().is_redirection() {
        return Err(format!("download {url}: redirects are not allowed"));
    }
    if let Some(actual) = response.content_length()
        && actual != expected_size
    {
        return Err(format!(
            "download {url}: expected {expected_size} bytes, server announced {actual}"
        ));
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(expected_size.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| format!("download {url}: {e}"))?;
    if bytes.len() as u64 != expected_size {
        return Err(format!(
            "download {url}: expected {expected_size} bytes, received {}",
            bytes.len()
        ));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    tilo_files::native::atomic::write(dest, &bytes)
        .map_err(|e| format!("write {}: {e}", dest.display()))?;
    Ok(bytes.len() as u64)
}

// ---- orchestration -------------------------------------------------

/// Execute a card action end-to-end. `staging_dir` is where a
/// downloaded `.tapp` is written before install; pass
/// [`tapp_staging_dir`] in production.
///
/// For `Open`/`Update` on an uncached Tilo app this is: download →
/// app-host verified install → exact prepared launch target. The Store
/// submits that target on its original authenticated collaboration channel
/// and waits for actual startup. Already-cached apps preserve their selected
/// signed package root through the same route.
pub fn execute(item: &StoreItem, action: ActionKind, staging_dir: &Path) -> ActionOutcome {
    execute_with_host(None, item, action, staging_dir)
}

/// Execute through the launcher-issued host capability. Production always
/// supplies `host`; the optional form keeps pure error-path tests independent
/// of a live session daemon.
pub fn execute_with_host(
    host: Option<&app_host::client::BlockingClient>,
    item: &StoreItem,
    action: ActionKind,
    staging_dir: &Path,
) -> ActionOutcome {
    if let Some((_, reason)) = item.unavailable_action(action) {
        return ActionOutcome::fail(reason);
    }
    match (action, &item.source) {
        (ActionKind::Open, StoreSource::Tilo { .. })
        | (ActionKind::Update, StoreSource::Tilo { .. })
        | (ActionKind::RuntimeInstall, StoreSource::Tilo { .. }) => {
            open_tilo(host, item, action, staging_dir)
        }
        (ActionKind::Remove, StoreSource::Tilo { .. }) => tilo_existing(host, item, |host, id| {
            host.remove(id).map(|()| "Removed app.".into())
        }),
        (ActionKind::MarkInstalled, StoreSource::Tilo { .. }) => {
            tilo_existing(host, item, |host, id| {
                host.pin(id, true).map(|()| "Marked installed.".into())
            })
        }
        (ActionKind::Unmark, StoreSource::Tilo { .. }) => tilo_existing(host, item, |host, id| {
            host.pin(id, false).map(|()| "Unmarked app.".into())
        }),
        (ActionKind::FlatpakInstall, StoreSource::Flathub | StoreSource::FlatpakRemote { .. }) => {
            let reference = match item.install_ref_path.as_deref() {
                Some(path) => match std::fs::read(path) {
                    Ok(bytes) if bytes.len() <= 64 * 1024 => Some(bytes),
                    Ok(_) => return ActionOutcome::fail("Flatpak reference exceeds 64 KiB"),
                    Err(error) => {
                        return ActionOutcome::fail(format!(
                            "Could not read Flatpak reference: {error}"
                        ));
                    }
                },
                None => None,
            };
            host_result(host, |host| {
                if let StoreSource::FlatpakRemote {
                    name,
                    branch,
                    user_managed,
                    ..
                } = &item.source
                {
                    if !*user_managed {
                        return Err("This repository belongs to the device. Ask its administrator to install the app.".into());
                    }
                    host.flatpak_install_remote(name, &item.id, branch)
                        .map(|_| ui::i18n!("store-app-installed"))
                } else {
                    host.flatpak(
                        app_host::client::FlatpakAction::Install,
                        &item.id,
                        reference.as_deref(),
                    )
                    .map(|_| ui::i18n!("store-app-installed"))
                }
            })
        }
        (ActionKind::FlatpakUpdate, StoreSource::Flathub | StoreSource::FlatpakRemote { .. }) => {
            host_result(host, |host| {
                host.flatpak(app_host::client::FlatpakAction::Update, &item.id, None)
                    .map(|_| ui::i18n!("store-app-updated"))
            })
        }
        (ActionKind::FlatpakOpen, StoreSource::Flathub | StoreSource::FlatpakRemote { .. }) => {
            ActionOutcome {
                ok: true,
                status_line: format!("Opening {}…", item.name),
                launch: Some(ApplicationLaunch {
                    app_id: format!("host:{}", item.id),
                    package_root: None,
                }),
            }
        }
        (ActionKind::FlatpakRemove, StoreSource::Flathub | StoreSource::FlatpakRemote { .. }) => {
            host_result(host, |host| {
                host.flatpak(app_host::client::FlatpakAction::Remove, &item.id, None)
                    .map(|_| ui::i18n!("store-app-removed"))
            })
        }
        (ActionKind::NeedsFlatpakHost, _) => {
            ActionOutcome::fail("Install flatpak on the host to use this action.")
        }
        // Nonsensical (source, action) pairings never issue a
        // subprocess — silent no-op rather than a wrong spawn.
        _ => ActionOutcome::fail("unsupported action for this item"),
    }
}

/// The Tilo `Open` / `Update` flow: fetch-if-needed, then launch.
fn open_tilo(
    host: Option<&app_host::client::BlockingClient>,
    item: &StoreItem,
    action: ActionKind,
    _staging_dir: &Path,
) -> ActionOutcome {
    let Some(host) = host else {
        return ActionOutcome::fail("App service is unavailable; no host command was attempted.");
    };
    let already_cached = item.installed.is_installed() && action == ActionKind::Open;

    let mut package_root = host.list().ok().and_then(|packages| {
        packages
            .into_iter()
            .find(|package| package.id == item.id)
            .and_then(|package| package.package_id())
    });

    if !already_cached {
        // Need a download URL for the newest version.
        let Some(version) = item.versions.first() else {
            return ActionOutcome::fail("registry entry has no version");
        };
        let Some(root) = version
            .root_hash_hex
            .as_deref()
            .filter(|root| root.len() == 64 && root.bytes().all(|byte| byte.is_ascii_hexdigit()))
        else {
            return ActionOutcome::fail("registry entry has an invalid package root");
        };
        let Some(publisher) = item.publisher_pubkey_hex.as_deref() else {
            return ActionOutcome::fail("registry entry has no publisher identity");
        };
        let expected = match (format!("tapp:{root}").parse(), hex::decode(publisher)) {
            (Ok(package_root), Ok(key)) if key.len() == 32 => {
                let mut publisher = [0u8; 32];
                publisher.copy_from_slice(&key);
                app_host::install::InstallExpectation {
                    package_root,
                    publisher,
                    app_id: item.id.clone(),
                    version: version.version.clone(),
                    size_bytes: version.size_bytes,
                }
            }
            _ => {
                return ActionOutcome::fail(
                    "registry entry has an invalid package or publisher identity",
                );
            }
        };
        let StoreSource::Tilo { registry_name } = &item.source else {
            return ActionOutcome::fail("This package has no Tilo repository.");
        };
        match host.install_offer(registry_name, &expected) {
            Ok(installed) => package_root = Some(installed),
            Err(error) => {
                return ActionOutcome::fail(format!("{}: install refused: {error}", item.name));
            }
        }
        // An update replaces what runs at the app's next launch; it does
        // not open the app.
        if action == ActionKind::RuntimeInstall {
            return ActionOutcome::ok(ui::i18n::I18nManager::format(
                "store-runtime-installed",
                &[
                    ("name", ui::i18n::ArgValue::Text(item.name.clone())),
                    ("version", ui::i18n::ArgValue::Text(version.version.clone())),
                ],
            ));
        }
        if action == ActionKind::Update {
            return ActionOutcome::ok(format!("Updated {} to {}", item.name, version.version));
        }
    }

    // Launch. A launch failure (e.g. no sandbox on this host) is
    // surfaced but doesn't undo the successful fetch/cache.
    let Some(package_root) = package_root else {
        return ActionOutcome::fail("installed package could not be resolved");
    };
    ActionOutcome {
        ok: true,
        status_line: format!("Opening {}…", item.name),
        launch: Some(ApplicationLaunch {
            app_id: item.id.clone(),
            package_root: Some(package_root.to_string()),
        }),
    }
}

fn host_result(
    host: Option<&app_host::client::BlockingClient>,
    action: impl FnOnce(&app_host::client::BlockingClient) -> Result<String, String>,
) -> ActionOutcome {
    let Some(host) = host else {
        return ActionOutcome::fail("App service is unavailable; no host command was attempted.");
    };
    match action(host) {
        Ok(status) => ActionOutcome::ok(status),
        Err(error) => ActionOutcome::fail(error),
    }
}

fn tilo_existing(
    host: Option<&app_host::client::BlockingClient>,
    item: &StoreItem,
    action: impl FnOnce(
        &app_host::client::BlockingClient,
        app_host::PackageId,
    ) -> Result<String, String>,
) -> ActionOutcome {
    let Some(host) = host else {
        return ActionOutcome::fail("App service is unavailable; no host command was attempted.");
    };
    let id = match host.list().and_then(|packages| {
        packages
            .into_iter()
            .find(|package| package.id == item.id)
            .and_then(|package| package.package_id())
            .ok_or_else(|| "installed package could not be resolved".into())
    }) {
        Ok(id) => id,
        Err(error) => return ActionOutcome::fail(error),
    };
    host_result(Some(host), |host| action(host, id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InstalledState, StoreItem, StoreItemVersion, StoreSource, StoreTrust};

    fn tilo_item(name: &str, installed: InstalledState) -> StoreItem {
        StoreItem {
            id: format!("so.tilo.{}", name.to_ascii_lowercase()),
            name: name.into(),
            description: String::new(),
            tagline: String::new(),
            supported_languages: Vec::new(),
            categories: vec![],
            icon_url: None,
            screenshot_urls: Vec::new(),
            homepage_url: None,
            source_url: None,
            install_ref_path: None,
            source: StoreSource::Tilo {
                registry_name: "Tilo".into(),
            },
            trust: StoreTrust::PublisherSigned,
            versions: vec![StoreItemVersion {
                version: "0.1.0".into(),
                released_at_unix: 0,
                download_url: Some("https://x/y.tapp".into()),
                root_hash_hex: Some("de".repeat(32)),
                size_bytes: 0,
                runtime: None,
            }],
            publisher_pubkey_hex: Some("aa".repeat(32)),
            installed,
        }
    }

    fn flatpak_item(name: &str, installed: InstalledState) -> StoreItem {
        StoreItem {
            id: format!("org.example.{name}"),
            name: name.into(),
            description: String::new(),
            tagline: String::new(),
            supported_languages: Vec::new(),
            categories: vec![],
            icon_url: None,
            screenshot_urls: Vec::new(),
            homepage_url: None,
            source_url: None,
            install_ref_path: None,
            source: StoreSource::Flathub,
            trust: StoreTrust::Flathub,
            versions: vec![StoreItemVersion {
                version: "1.0".into(),
                released_at_unix: 0,
                download_url: None,
                root_hash_hex: None,
                size_bytes: 0,
                runtime: None,
            }],
            publisher_pubkey_hex: None,
            installed,
        }
    }

    /// Without a Flatpak host, needs-host and open both fail without spawning anything.
    #[test]
    fn actions_without_a_flatpak_host_fail_without_spawning() {
        // Case `needs_flatpak_host_is_a_no_op_failure`.
        {
            let item = flatpak_item("Chess", InstalledState::Not);
            let out = execute(&item, ActionKind::NeedsFlatpakHost, Path::new("/tmp"));
            assert!(!out.ok);
        }

        // Case `open_without_host_capability_fails_without_spawning_anything`.
        {
            // A Tilo item whose registry entry lost its download URL.
            let mut item = tilo_item("Notes", InstalledState::Not);
            item.versions[0].download_url = None;
            let out = execute(&item, ActionKind::Open, Path::new("/tmp"));
            assert!(!out.ok);
            assert!(out.status_line.contains("service is unavailable"));
        }
    }
}
