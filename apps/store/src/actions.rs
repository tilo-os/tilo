//! Action dispatch + execution — what happens when a card button is
//! clicked.
//!
//! Two layers:
//!
//!   * **Planning** ([`SpawnPlan`] + the `*_plan` builders): pure
//!     functions that describe the exact subprocess a step would run.
//!     Extracting the invocation as data keeps dispatch unit-testable
//!     without touching a real `qos-install` / `flatpak` binary.
//!   * **Execution** ([`execute`]): orchestrates a whole action —
//!     downloading a `.qpkg` on demand, handing it to `qos-install`
//!     (which verifies the publisher signature + content hash before
//!     it extracts), and launching it. Runs on the worker thread, never
//!     on the UI thread.
//!
//! The qOS side deliberately has **no "Install" button**. `Open`
//! fetches the package the first time and launches it every time;
//! `Mark installed` (pin) is the separate, optional step that keeps an
//! app and ranks it first. Cryptographic verification is delegated to
//! `qos-install`, which recomputes the content-addressed package id and
//! checks the publisher signature fail-closed before extracting a
//! single byte.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::model::{ActionKind, StoreItem, StoreSource};

/// A single subprocess invocation — the exact command a step runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnPlan {
    pub program: String,
    pub args: Vec<String>,
    /// One-line status message rendered while the process runs.
    pub status_line: String,
}

/// The outcome of a fully-executed [`execute`] call: whether the last
/// meaningful step succeeded and the human-readable status the status
/// bar should show.
#[derive(Clone, Debug)]
pub struct ActionOutcome {
    pub ok: bool,
    pub status_line: String,
}

impl ActionOutcome {
    fn ok(msg: impl Into<String>) -> Self {
        Self {
            ok: true,
            status_line: msg.into(),
        }
    }
    fn fail(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            status_line: msg.into(),
        }
    }
}

fn qos_install_bin() -> String {
    std::env::var("QOS_INSTALL_BIN").unwrap_or_else(|_| "qos-install".to_string())
}

fn qos_launcher_bin() -> String {
    std::env::var("QOS_LAUNCHER_BIN").unwrap_or_else(|_| "qos-launcher".to_string())
}

fn registry_label(item: &StoreItem) -> String {
    match &item.source {
        StoreSource::Qos { registry_name } => registry_name.clone(),
        StoreSource::Flathub => "Flathub".to_string(),
    }
}

// ---- plan builders (pure) ------------------------------------------

/// `qos-install install <staged.qpkg> --allow-self-signed --force`.
/// This is the fetch-on-demand *cache* step: `qos-install` verifies
/// and extracts the package into the local content-addressed store.
pub fn install_plan(item: &StoreItem, staged_qpkg: &Path) -> SpawnPlan {
    SpawnPlan {
        program: qos_install_bin(),
        args: vec![
            "install".into(),
            staged_qpkg.to_string_lossy().into_owned(),
            "--allow-self-signed".into(),
            "--force".into(),
        ],
        status_line: format!(
            "Fetching {} {} from {}…",
            item.name,
            item.latest_version(),
            registry_label(item),
        ),
    }
}

/// `qos-install run <id> --no-services --allow-missing-runtime`.
pub fn run_plan(item: &StoreItem) -> SpawnPlan {
    SpawnPlan {
        program: qos_install_bin(),
        args: vec![
            "run".into(),
            item.id.clone(),
            "--no-services".into(),
            "--allow-missing-runtime".into(),
        ],
        status_line: format!("Opening {}…", item.name),
    }
}

/// `qos-install remove <id>`.
pub fn remove_plan(item: &StoreItem) -> SpawnPlan {
    SpawnPlan {
        program: qos_install_bin(),
        args: vec!["remove".into(), item.id.clone()],
        status_line: format!("Removing {}…", item.name),
    }
}

/// `qos-install pin|unpin <id>` — the "mark installed" toggle.
pub fn pin_plan(item: &StoreItem, pin: bool) -> SpawnPlan {
    SpawnPlan {
        program: qos_install_bin(),
        args: vec![if pin { "pin" } else { "unpin" }.into(), item.id.clone()],
        status_line: if pin {
            format!("Marking {} installed…", item.name)
        } else {
            format!("Unmarking {}…", item.name)
        },
    }
}

/// Open an installed Flatpak.
///
/// Goes through `qos-launcher run-host` rather than `flatpak run`
/// directly, for the same reason every other launch in qOS goes through
/// the launcher: one place decides what environment an application
/// starts in. The launcher resolves the app's exported `.desktop` entry
/// — a Flathub app id *is* its desktop-file id, because that is how
/// Flatpak names its exports — expands the entry's own `Exec` line, and
/// hands over the compositor's Wayland socket with qOS's internal
/// variables stripped.
///
/// It is deliberately not wrapped in qOS's sandbox. A Flatpak builds
/// its own, which needs syscalls the baseline seccomp filter denies, so
/// nesting the two yields an app that fails to start rather than one
/// confined twice. `run-host` prints an UNCONFINED notice naming the
/// app; see `launcher::host`.
pub fn flatpak_open_plan(item: &StoreItem) -> SpawnPlan {
    SpawnPlan {
        program: qos_launcher_bin(),
        args: vec!["run-host".into(), "--desktop-id".into(), item.id.clone()],
        status_line: format!("Opening {}…", item.name),
    }
}

/// Flatpak install/remove via the host `flatpak` CLI.
pub fn flatpak_plan(item: &StoreItem, install: bool) -> SpawnPlan {
    if install {
        let mut args = vec!["install".into(), "--user".into(), "--noninteractive".into()];
        if let Some(path) = item.install_ref_path.as_ref() {
            args.push(path.clone());
        } else {
            args.extend(["flathub".into(), item.id.clone()]);
        }
        SpawnPlan {
            program: "flatpak".into(),
            args,
            status_line: format!("Installing Flatpak {}…", item.name),
        }
    } else {
        SpawnPlan {
            program: "flatpak".into(),
            args: vec![
                "uninstall".into(),
                "--user".into(),
                "--noninteractive".into(),
                item.id.clone(),
            ],
            status_line: format!("Removing Flatpak {}…", item.name),
        }
    }
}

/// `flatpak update --user --noninteractive <app-id>`.
pub fn flatpak_update_plan(item: &StoreItem) -> SpawnPlan {
    SpawnPlan {
        program: "flatpak".into(),
        args: vec![
            "update".into(),
            "--user".into(),
            "--noninteractive".into(),
            item.id.clone(),
        ],
        status_line: format!("Updating Flatpak {}…", item.name),
    }
}

pub fn flatpak_remote_add_plan(name: &str, url: &str) -> SpawnPlan {
    SpawnPlan {
        program: "flatpak".into(),
        args: vec![
            "remote-add".into(),
            "--user".into(),
            "--if-not-exists".into(),
            name.into(),
            url.into(),
        ],
        status_line: format!("Adding Flatpak remote {name}…"),
    }
}

pub fn flatpak_remote_remove_plan(name: &str) -> SpawnPlan {
    SpawnPlan {
        program: "flatpak".into(),
        args: vec![
            "remote-delete".into(),
            "--user".into(),
            "--force".into(),
            name.into(),
        ],
        status_line: format!("Removing Flatpak remote {name}…"),
    }
}

/// The staging directory for downloaded `.qpkg`s. Lives under the
/// store's cache dir so a container-mounted read-only home isn't a
/// problem. Best-effort creation.
pub fn qpkg_staging_dir() -> Option<PathBuf> {
    let base = crate::config::cache_dir()?;
    let staged = base.join("staged-qpkg");
    let _ = std::fs::create_dir_all(&staged);
    Some(staged)
}

/// Spawn `plan`'s command and block until it exits. Returns combined
/// stdout+stderr plus the exit code (128 when unspawnable).
pub fn spawn_plan(plan: &SpawnPlan) -> (i32, String) {
    match Command::new(&plan.program).args(&plan.args).output() {
        Ok(out) => {
            let code = out.status.code().unwrap_or(1);
            let mut buf = String::new();
            buf.push_str(&String::from_utf8_lossy(&out.stdout));
            buf.push_str(&String::from_utf8_lossy(&out.stderr));
            (code, buf)
        }
        Err(e) => (128, format!("failed to run {}: {}", plan.program, e)),
    }
}

// ---- download ------------------------------------------------------

/// HTTP-download a `.qpkg` to `dest`. Blocking — runs on the worker
/// thread. The bytes are *not* trusted here: `qos-install install`
/// re-verifies the content hash + publisher signature before it
/// extracts anything, so a truncated or tampered download fails
/// closed at the install step.
///
/// # Errors
///
/// Returns an error message if the HTTP client can't be built, the request
/// fails or comes back with a non-success status or an empty body, or `dest`
/// (or its parent directory) can't be written.
pub fn download_qpkg(url: &str, dest: &Path) -> Result<u64, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("qos-store/0.1")
        .timeout(Duration::from_mins(2))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let bytes = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::bytes)
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

// ---- orchestration -------------------------------------------------

/// Execute a card action end-to-end. `staging_dir` is where a
/// downloaded `.qpkg` is written before install; pass
/// [`qpkg_staging_dir`] in production.
///
/// For `Open`/`Update` on an uncached qOS app this is: download →
/// `qos-install install` (verify + cache) → `qos-install run`
/// (launch). For an already-cached app `Open` skips straight to the
/// launch. Every subprocess result is folded into the returned
/// [`ActionOutcome`] so the status bubble reflects reality.
pub fn execute(item: &StoreItem, action: ActionKind, staging_dir: &Path) -> ActionOutcome {
    match (action, &item.source) {
        (ActionKind::Open, StoreSource::Qos { .. })
        | (ActionKind::Update, StoreSource::Qos { .. }) => open_qos(item, action, staging_dir),
        (ActionKind::Remove, StoreSource::Qos { .. }) => run_step(&remove_plan(item)),
        (ActionKind::MarkInstalled, StoreSource::Qos { .. }) => run_step(&pin_plan(item, true)),
        (ActionKind::Unmark, StoreSource::Qos { .. }) => run_step(&pin_plan(item, false)),
        (ActionKind::FlatpakInstall, StoreSource::Flathub) => run_step(&flatpak_plan(item, true)),
        (ActionKind::FlatpakUpdate, StoreSource::Flathub) => run_step(&flatpak_update_plan(item)),
        (ActionKind::FlatpakOpen, StoreSource::Flathub) => run_step(&flatpak_open_plan(item)),
        (ActionKind::FlatpakRemove, StoreSource::Flathub) => run_step(&flatpak_plan(item, false)),
        (ActionKind::NeedsFlatpakHost, _) => {
            ActionOutcome::fail("Install flatpak on the host to use this action.")
        }
        // Nonsensical (source, action) pairings never issue a
        // subprocess — silent no-op rather than a wrong spawn.
        _ => ActionOutcome::fail("unsupported action for this item"),
    }
}

/// The qOS `Open` / `Update` flow: fetch-if-needed, then launch.
fn open_qos(item: &StoreItem, action: ActionKind, staging_dir: &Path) -> ActionOutcome {
    let already_cached = item.installed.is_installed() && action == ActionKind::Open;

    if !already_cached {
        // Need a download URL for the newest version.
        let Some(url) = item.versions.first().and_then(|v| v.download_url.clone()) else {
            return ActionOutcome::fail(format!(
                "{}: no download URL in registry entry",
                item.name
            ));
        };
        let dest = staging_dir.join(format!("{}-{}.qpkg", item.id, item.latest_version()));
        if let Err(e) = download_qpkg(&url, &dest) {
            return ActionOutcome::fail(format!("{}: {e}", item.name));
        }
        let (code, out) = spawn_plan(&install_plan(item, &dest));
        // Clean the staged file regardless — the bytes now live in
        // the content-addressed store.
        let _ = std::fs::remove_file(&dest);
        if code != 0 {
            return ActionOutcome::fail(format!(
                "{}: install failed ({}): {}",
                item.name,
                code,
                last_line(&out)
            ));
        }
    }

    // Launch. A launch failure (e.g. no sandbox on this host) is
    // surfaced but doesn't undo the successful fetch/cache.
    let (code, out) = spawn_plan(&run_plan(item));
    if code == 0 {
        ActionOutcome::ok(format!("Opened {}.", item.name))
    } else {
        ActionOutcome::fail(format!(
            "{}: fetched, but launch failed ({}): {}",
            item.name,
            code,
            last_line(&out)
        ))
    }
}

/// Run a single plan and fold its result into an outcome.
fn run_step(plan: &SpawnPlan) -> ActionOutcome {
    let (code, out) = spawn_plan(plan);
    if code == 0 {
        ActionOutcome::ok(first_line(&out).unwrap_or_else(|| plan.status_line.clone()))
    } else {
        ActionOutcome::fail(format!("failed ({code}): {}", last_line(&out)))
    }
}

fn first_line(s: &str) -> Option<String> {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
}

fn last_line(s: &str) -> String {
    s.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InstalledState, StoreItem, StoreItemVersion, StoreSource};

    fn qos_item(name: &str, installed: InstalledState) -> StoreItem {
        StoreItem {
            id: format!("dev.qos.{}", name.to_ascii_lowercase()),
            name: name.into(),
            description: String::new(),
            categories: vec![],
            icon_url: None,
            homepage_url: None,
            source_url: None,
            install_ref_path: None,
            source: StoreSource::Qos {
                registry_name: "qOS".into(),
            },
            versions: vec![StoreItemVersion {
                version: "0.1.0".into(),
                released_at_unix: 0,
                download_url: Some("https://x/y.qpkg".into()),
                root_hash_hex: Some("de".repeat(32)),
                size_bytes: 0,
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
            categories: vec![],
            icon_url: None,
            homepage_url: None,
            source_url: None,
            install_ref_path: None,
            source: StoreSource::Flathub,
            versions: vec![StoreItemVersion {
                version: "1.0".into(),
                released_at_unix: 0,
                download_url: None,
                root_hash_hex: None,
                size_bytes: 0,
            }],
            publisher_pubkey_hex: None,
            installed,
        }
    }

    #[test]
    fn install_plan_uses_qos_install_install_force_self_signed() {
        let item = qos_item("Notes", InstalledState::Not);
        let plan = install_plan(&item, Path::new("/tmp/notes.qpkg"));
        assert!(plan.program.ends_with("qos-install"));
        assert_eq!(plan.args[0], "install");
        assert_eq!(plan.args[1], "/tmp/notes.qpkg");
        assert!(plan.args.contains(&"--allow-self-signed".to_string()));
        assert!(plan.args.contains(&"--force".to_string()));
    }

    #[test]
    fn run_plan_launches_by_id() {
        let item = qos_item("Notes", InstalledState::Not);
        let plan = run_plan(&item);
        assert_eq!(
            plan.args,
            vec![
                "run".to_string(),
                "dev.qos.notes".to_string(),
                "--no-services".to_string(),
                "--allow-missing-runtime".to_string(),
            ]
        );
    }

    #[test]
    fn pin_plan_maps_to_pin_and_unpin() {
        let item = qos_item(
            "Notes",
            InstalledState::Installed {
                version: "1".into(),
                available: "1".into(),
                pinned: false,
            },
        );
        assert_eq!(
            pin_plan(&item, true).args,
            vec!["pin".to_string(), "dev.qos.notes".to_string()]
        );
        assert_eq!(
            pin_plan(&item, false).args,
            vec!["unpin".to_string(), "dev.qos.notes".to_string()]
        );
    }

    #[test]
    fn remove_plan_uses_qos_install_remove() {
        let item = qos_item(
            "Notes",
            InstalledState::Installed {
                version: "1".into(),
                available: "1".into(),
                pinned: false,
            },
        );
        let plan = remove_plan(&item);
        assert_eq!(
            plan.args,
            vec!["remove".to_string(), "dev.qos.notes".to_string()]
        );
    }

    #[test]
    fn flatpak_plan_install_and_remove() {
        let item = flatpak_item("Chess", InstalledState::Not);
        let ins = flatpak_plan(&item, true);
        assert_eq!(ins.program, "flatpak");
        assert_eq!(ins.args[0], "install");
        assert!(ins.args.contains(&"flathub".to_string()));
        assert!(ins.args.contains(&"org.example.Chess".to_string()));
        let rm = flatpak_plan(&item, false);
        assert_eq!(rm.args[0], "uninstall");
        let update = flatpak_update_plan(&item);
        assert_eq!(update.args[0], "update");
        assert!(update.args.contains(&"org.example.Chess".to_string()));
    }

    #[test]
    fn flatpak_remote_plans_are_user_scoped_and_noninteractive() {
        let add = flatpak_remote_add_plan("flathub", "https://example.test/repo");
        assert_eq!(add.args[0], "remote-add");
        assert!(add.args.contains(&"--user".to_string()));
        assert!(add.args.contains(&"--if-not-exists".to_string()));
        let remove = flatpak_remote_remove_plan("flathub");
        assert_eq!(remove.args[0], "remote-delete");
        assert!(remove.args.contains(&"--force".to_string()));
    }

    #[test]
    fn flatpak_ref_install_uses_the_selected_file() {
        let mut item = flatpak_item("Chess", InstalledState::Not);
        item.install_ref_path = Some("/tmp/chess.flatpakref".into());
        let plan = flatpak_plan(&item, true);
        assert!(plan.args.contains(&"/tmp/chess.flatpakref".to_string()));
        assert!(!plan.args.contains(&"flathub".to_string()));
    }

    /// The gap this closes: an installed Flatpak had no action that
    /// started it. Opening one goes through the launcher, unconfined,
    /// by the desktop-file id Flatpak exported it under.
    #[test]
    fn flatpak_open_goes_through_the_launchers_host_path() {
        let item = flatpak_item("Chess", InstalledState::Not);
        let plan = flatpak_open_plan(&item);
        assert_eq!(plan.program, "qos-launcher");
        assert_eq!(
            plan.args,
            vec![
                "run-host".to_string(),
                "--desktop-id".to_string(),
                "org.example.Chess".to_string(),
            ]
        );
        assert!(plan.status_line.contains("Opening Chess"));
    }

    #[test]
    fn needs_flatpak_host_is_a_no_op_failure() {
        let item = flatpak_item("Chess", InstalledState::Not);
        let out = execute(&item, ActionKind::NeedsFlatpakHost, Path::new("/tmp"));
        assert!(!out.ok);
    }

    #[test]
    fn open_without_download_url_fails_cleanly() {
        // A qOS item whose registry entry lost its download URL.
        let mut item = qos_item("Notes", InstalledState::Not);
        item.versions[0].download_url = None;
        let out = execute(&item, ActionKind::Open, Path::new("/tmp"));
        assert!(!out.ok);
        assert!(out.status_line.contains("no download URL"));
    }
}
