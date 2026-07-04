//! Action dispatch: the four buttons a store card can offer all end
//! up here.
//!
//! Every action is a **subprocess spawn** (with `output()` so we can
//! capture stdout/stderr into the status bar) executed on the worker
//! thread — never on the UI thread. The dispatcher is a small pure
//! function that returns the command it would run; the actual
//! spawning + streaming lives on the worker so it can be exercised
//! separately without a real `flatpak` binary.

use std::path::PathBuf;
use std::process::Command;

use crate::model::{ActionKind, StoreItem, StoreSource};

/// A single spawn plan — the exact process invocation the worker
/// would run. Extracting this as data makes the dispatch layer
/// unit-testable without touching a real subprocess.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnPlan {
    pub program: String,
    pub args: Vec<String>,
    /// One-line status message rendered while the process is
    /// running. The worker overwrites this with the process' own
    /// stderr line stream once it starts producing output.
    pub status_line: String,
}

/// Resolve which subprocess a given action → item pair should run.
///
/// The resolver is pure — it decides "for this item + this action,
/// which command line". Downloading the `.qpkg` and hash-checking
/// it against the entry's `qpkg_root_hash_hex` happens on the worker
/// **before** `qos-install` is invoked; the plan returned here
/// assumes the download has already been staged to `staged_qpkg`
/// (a `None` staged path means "the worker still needs to
/// download").
pub fn plan_for(
    item: &StoreItem,
    action: ActionKind,
    staged_qpkg: Option<&std::path::Path>,
) -> Option<SpawnPlan> {
    match (action, &item.source) {
        (ActionKind::Add, StoreSource::Qos { .. })
        | (ActionKind::Update, StoreSource::Qos { .. }) => {
            let staged = staged_qpkg?;
            Some(SpawnPlan {
                program: qos_install_bin(),
                args: vec![
                    "install".into(),
                    staged.to_string_lossy().into_owned(),
                    "--allow-self-signed".into(),
                ],
                status_line: format!(
                    "Installing {} {} from {}…",
                    item.name,
                    item.latest_version(),
                    registry_label(item),
                ),
            })
        }
        (ActionKind::Remove, StoreSource::Qos { .. }) => Some(SpawnPlan {
            program: qos_install_bin(),
            args: vec!["remove".into(), item.id.clone()],
            status_line: format!("Removing {}…", item.name),
        }),
        (ActionKind::FlatpakInstall, StoreSource::Flathub) => Some(SpawnPlan {
            program: "flatpak".into(),
            args: vec![
                "install".into(),
                "--user".into(),
                "--noninteractive".into(),
                "flathub".into(),
                item.id.clone(),
            ],
            status_line: format!("Installing Flatpak {}…", item.name),
        }),
        (ActionKind::FlatpakRemove, StoreSource::Flathub) => Some(SpawnPlan {
            program: "flatpak".into(),
            args: vec![
                "uninstall".into(),
                "--user".into(),
                "--noninteractive".into(),
                item.id.clone(),
            ],
            status_line: format!("Removing Flatpak {}…", item.name),
        }),
        (ActionKind::NeedsFlatpakHost, _) => None,
        // Unreachable pairings — enforce with `None` so an
        // accidentally-driven "Remove a Flatpak with an Add-qOS
        // path" is a silent no-op rather than an incorrect spawn.
        _ => None,
    }
}

fn qos_install_bin() -> String {
    std::env::var("QOS_INSTALL_BIN").unwrap_or_else(|_| "qos-install".to_string())
}

fn registry_label(item: &StoreItem) -> String {
    match &item.source {
        StoreSource::Qos { registry_name } => registry_name.clone(),
        StoreSource::Flathub => "Flathub".to_string(),
    }
}

/// The staging directory for downloaded `.qpkg`s. Lives under the
/// store's cache dir so a container-mounted read-only home is not
/// a problem. Best-effort creation.
pub fn qpkg_staging_dir() -> Option<PathBuf> {
    let base = crate::config::cache_dir()?;
    let staged = base.join("staged-qpkg");
    let _ = std::fs::create_dir_all(&staged);
    Some(staged)
}

/// Spawn `plan`'s command and block until it exits. Returns
/// combined stdout+stderr as one string, plus the exit code (128
/// when the binary was missing or unspawnable).
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InstalledState, StoreItem, StoreItemVersion, StoreSource};
    use std::path::PathBuf;

    fn qos_item(name: &str, installed: InstalledState) -> StoreItem {
        StoreItem {
            id: format!("dev.qos.{}", name.to_ascii_lowercase()),
            name: name.into(),
            description: "".into(),
            categories: vec![],
            icon_url: None,
            homepage_url: None,
            source_url: None,
            source: StoreSource::Qos { registry_name: "qOS".into() },
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
            id: format!("org.example.{}", name),
            name: name.into(),
            description: "".into(),
            categories: vec![],
            icon_url: None,
            homepage_url: None,
            source_url: None,
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
    fn qos_add_uses_qos_install_install() {
        let item = qos_item("Notes", InstalledState::Not);
        let staged = PathBuf::from("/tmp/notes.qpkg");
        let plan = plan_for(&item, ActionKind::Add, Some(&staged)).expect("plan");
        assert!(plan.program.ends_with("qos-install"));
        assert_eq!(plan.args[0], "install");
        assert_eq!(plan.args[1], "/tmp/notes.qpkg");
        assert!(plan.args.contains(&"--allow-self-signed".to_string()));
    }

    #[test]
    fn qos_add_without_staged_returns_none() {
        let item = qos_item("Notes", InstalledState::Not);
        assert!(plan_for(&item, ActionKind::Add, None).is_none());
    }

    #[test]
    fn qos_remove_uses_qos_install_remove() {
        let item = qos_item(
            "Notes",
            InstalledState::Installed { version: "1".into(), available: "1".into() },
        );
        let plan = plan_for(&item, ActionKind::Remove, None).expect("plan");
        assert!(plan.program.ends_with("qos-install"));
        assert_eq!(plan.args, vec!["remove".to_string(), "dev.qos.notes".to_string()]);
    }

    #[test]
    fn flatpak_install_uses_flatpak_install() {
        let item = flatpak_item("Chess", InstalledState::Not);
        let plan = plan_for(&item, ActionKind::FlatpakInstall, None).expect("plan");
        assert_eq!(plan.program, "flatpak");
        assert_eq!(plan.args[0], "install");
        assert!(plan.args.contains(&"flathub".to_string()));
        assert!(plan.args.contains(&"org.example.Chess".to_string()));
    }

    #[test]
    fn flatpak_remove_uses_flatpak_uninstall() {
        let item = flatpak_item(
            "Chess",
            InstalledState::Installed { version: "1".into(), available: "1".into() },
        );
        let plan = plan_for(&item, ActionKind::FlatpakRemove, None).expect("plan");
        assert_eq!(plan.program, "flatpak");
        assert_eq!(plan.args[0], "uninstall");
    }

    #[test]
    fn needs_flatpak_host_yields_no_plan() {
        let item = flatpak_item("Chess", InstalledState::Not);
        assert!(plan_for(&item, ActionKind::NeedsFlatpakHost, None).is_none());
    }

    #[test]
    fn cross_source_pairing_is_rejected() {
        // A Flatpak item + qOS Add path is nonsense — return None
        // so the worker never issues a wrong subprocess.
        let item = flatpak_item("Chess", InstalledState::Not);
        assert!(plan_for(&item, ActionKind::Add, None).is_none());
    }
}
