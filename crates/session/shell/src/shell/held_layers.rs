//! Deployed layers held on the booted image, told to the person.
//!
//! On a development machine tilo-vmd keeps every layer the person deployed
//! across an image change, and holds those that fit neither the new base nor
//! its interface versions: kept, but not merged, so the image's own
//! component runs instead (crates/dev/tilo-vmd/src/image.rs). It leaves a
//! notice at [`NOTICE`]; this desktop tells the person once per notice which
//! layers are held and why. A machine without tilo-vmd has no notice.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tilo_desktop::notifications::{Notification, NotificationSeverity};
use tilo_lock::MutexRecover as _;

use crate::shell::i18n::{t, t_args};

/// Where tilo-vmd leaves the notice (its `NOTICE_DIR` and `HELD_NOTICE`).
pub(crate) const NOTICE: &str = "/run/tilo-vm/held-layers.json";
/// The first look, once the session has settled; the notice is written at
/// boot, before the person logs in.
const FIRST_LOOK_AFTER: Duration = Duration::from_secs(20);
/// Later looks: a notice only changes at a boot, or when a swap brings a
/// held layer back.
const LOOK_EVERY: Duration = Duration::from_secs(10 * 60);

/// One held layer, as the notice names it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct Held {
    pub(crate) component: String,
    #[serde(default)]
    pub(crate) reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct HeldNotice {
    #[serde(default)]
    pub(crate) image: Option<String>,
    #[serde(default)]
    pub(crate) layers: Vec<Held>,
}

pub(crate) fn parse(json: &str) -> Option<HeldNotice> {
    serde_json::from_str::<HeldNotice>(json)
        .ok()
        .filter(|notice| !notice.layers.is_empty())
}

/// The notification for `notice`: which components are held, and why.
pub(crate) fn notification(notice: &HeldNotice) -> Notification {
    let lines: Vec<String> = notice
        .layers
        .iter()
        .map(|held| format!("{}: {}", held.component, held.reason))
        .collect();
    Notification::new(
        t_args(
            "held-layers-title",
            &[("count", notice.layers.len().into())],
        ),
        t_args("held-layers-body", &[("layers", lines.join("\n").into())]),
    )
    .with_severity(NotificationSeverity::Warning)
    .with_app_name(t("held-layers-app-name"))
}

/// Whether `json` is a notice not told yet: the last one told is remembered
/// at `told`, so a restart of the desktop does not tell it again.
fn untold(json: &str, told: &Path) -> bool {
    std::fs::read_to_string(told).ok().as_deref() != Some(json)
}

fn told_path() -> Option<PathBuf> {
    tilo_paths::state_dir().map(|root| root.join("tilo/held-layers-told.json"))
}

static PENDING: Mutex<Vec<Notification>> = Mutex::new(Vec::new());

fn look() {
    let Ok(json) = std::fs::read_to_string(NOTICE) else {
        return;
    };
    let Some(notice) = parse(&json) else {
        return;
    };
    let Some(told) = told_path() else {
        return;
    };
    if !untold(&json, &told) {
        return;
    }
    if let Err(error) = tilo_files::native::atomic::write_creating_parent(&told, json.as_bytes()) {
        tracing::warn!(%error, "could not remember the held-layers notice");
    }
    PENDING.lock_recover().push(notification(&notice));
}

pub(crate) fn start() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let spawned = std::thread::Builder::new()
            .name("held-layers".to_string())
            .spawn(|| {
                std::thread::sleep(FIRST_LOOK_AFTER);
                loop {
                    look();
                    std::thread::sleep(LOOK_EVERY);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not start looking for held layers");
        }
    });
}

pub(crate) fn drain() -> Vec<Notification> {
    std::mem::take(&mut *PENDING.lock_recover())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The notice tilo-vmd writes names each held layer and why; the person
    /// reads both, once.
    #[test]
    fn a_held_layers_notice_is_told_once_with_each_reason() {
        let json = r#"{"image": "composefs:two", "layers": [
            {"component": "shell", "layer": "ab12", "reason": "speaks shell-ui 9, the image serves 10"}]}"#;
        let notice = parse(json).expect("a notice");
        assert_eq!(notice.image.as_deref(), Some("composefs:two"));
        let shown = notification(&notice);
        assert!(
            shown
                .body
                .contains("shell: speaks shell-ui 9, the image serves 10"),
            "{}",
            shown.body
        );
        assert_eq!(
            parse(r#"{"layers": []}"#),
            None,
            "nothing held, nothing told"
        );
        assert_eq!(parse("not json"), None);

        let dir = tempfile::tempdir().unwrap();
        let told = dir.path().join("told.json");
        assert!(untold(json, &told));
        std::fs::write(&told, json).unwrap();
        assert!(!untold(json, &told), "told once");
        assert!(untold(r#"{"layers": [{"component": "launcher"}]}"#, &told));
    }
}
