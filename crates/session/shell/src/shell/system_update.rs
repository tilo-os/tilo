//! The system on its newest version, with a restart asked for at most every
//! two days.
//!
//! The update service fetches and stages the newest system image on its own
//! schedule (`bootc-fetch-apply-updates.timer`, at least daily); apps and the
//! runtime they link update in the background without a restart
//! (`tilo-app-update.timer`). A staged image applies at the next restart, so
//! the person is asked to restart, but never more often than every
//! [`ASK_INTERVAL`]: the updates of those days apply together, with one
//! restart (docs/architecture/compatibility.md). An app or a collaborator
//! that needs a newer system than this one starts the update at once
//! and asks to restart as soon as it is ready ([`update_now_and_ask`]).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use tilo_desktop::notifications::{Notification, NotificationSeverity};
use tilo_lock::MutexRecover as _;

use crate::shell::i18n::t;

/// The shortest time between two requests to restart.
pub(crate) const ASK_INTERVAL: Duration = Duration::from_secs(2 * 24 * 60 * 60);
/// How often the shell looks for a staged image. Staging happens at most a
/// few times a day; this costs one file check.
const LOOK_EVERY: Duration = Duration::from_secs(60 * 60);
/// The first look, once the session has settled.
const FIRST_LOOK_AFTER: Duration = Duration::from_secs(10 * 60);
/// Where ostree records the deployment staged for the next boot.
const OSTREE_STAGED: &str = "/run/ostree/staged-deployment";
/// The unit that fetches and stages the newest image now, which the person
/// at the machine may start (os/etc/polkit-1/rules.d/49-tilo-updates.rules).
const UPDATE_UNIT: &str = "tilo-deployment-update.service";
/// The notification action that restarts the machine.
pub(crate) const RESTART_ACTION: &str = "restart-to-update";

/// Whether to ask now: an image is staged and the last request is at least
/// [`ASK_INTERVAL`] old, or there was none.
pub(crate) fn due(staged: bool, last_asked: Option<SystemTime>, now: SystemTime) -> bool {
    staged
        && last_asked.is_none_or(|asked| {
            now.duration_since(asked)
                .is_ok_and(|since| since >= ASK_INTERVAL)
        })
}

/// When the person was last asked, as recorded at `path`.
fn last_asked_at(path: &Path) -> Option<SystemTime> {
    let seconds = std::fs::read_to_string(path).ok()?.trim().parse().ok()?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

fn record_asked_at(path: &Path, now: SystemTime) {
    let seconds = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    if let Err(error) =
        tilo_files::native::atomic::write_creating_parent(path, seconds.to_string().as_bytes())
    {
        tracing::warn!(%error, "could not record when a restart was last asked for");
    }
}

fn asked_path() -> Option<PathBuf> {
    tilo_paths::state_dir().map(|root| root.join("tilo/update-restart-asked"))
}

/// Whether an image waits for the next restart.
fn staged() -> bool {
    if Path::new(OSTREE_STAGED).exists() {
        return true;
    }
    bounded_command::run(
        "bootc",
        &["status", "--format", "json"],
        None,
        1024 * 1024,
        Duration::from_secs(10),
    )
    .ok()
    .filter(|output| output.status.success())
    .is_some_and(|output| staged_in(&String::from_utf8_lossy(&output.stdout)))
}

/// Whether `bootc status --format json` reports a staged deployment.
fn staged_in(json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|status| status.get("status")?.get("staged").cloned())
        .is_some_and(|staged| !staged.is_null())
}

/// Requests to restart waiting for the compositor to show them.
static ASKS: Mutex<Vec<Notification>> = Mutex::new(Vec::new());

/// Start looking for staged images, once per process.
pub(crate) fn start() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let spawned = std::thread::Builder::new()
            .name("system-update".to_string())
            .spawn(|| {
                std::thread::sleep(FIRST_LOOK_AFTER);
                loop {
                    look();
                    std::thread::sleep(LOOK_EVERY);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not start looking for system updates");
        }
    });
}

fn look() {
    let Some(path) = asked_path() else {
        return;
    };
    let now = SystemTime::now();
    if !due(staged(), last_asked_at(&path), now) {
        return;
    }
    record_asked_at(&path, now);
    queue_ask();
}

fn queue_ask() {
    ASKS.lock_recover().push(
        Notification::new(
            t("system-update-restart-title"),
            t("system-update-restart-body"),
        )
        .with_severity(NotificationSeverity::Info)
        .with_app_name(t("system-update-app-name"))
        .with_dismiss_after(Duration::from_secs(30)),
    );
}

/// Requests to restart since the last call, each with its action.
pub(crate) fn drain() -> Vec<Notification> {
    std::mem::take(&mut *ASKS.lock_recover())
        .into_iter()
        .map(|mut notification| {
            notification.actions = vec![(RESTART_ACTION.to_owned(), t("power-restart"))];
            notification
        })
        .collect()
}

/// Fetch and stage the newest system now, on a worker, and ask to restart
/// as soon as it is staged, whatever the last request: something (an app, a
/// collaborator's device) needs a newer version than this device runs.
pub(crate) fn update_now_and_ask() {
    // One at a time: asking again while it runs changes nothing.
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RUNNING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("system-update-now".to_string())
        .spawn(|| {
            let _done = Done(&RUNNING);
            // A oneshot unit: `start` returns once the image is staged, or
            // once there was nothing newer to stage.
            match bounded_command::run(
                "systemctl",
                &["start", "--no-ask-password", UPDATE_UNIT],
                None,
                64 * 1024,
                UPDATE_TIMEOUT,
            ) {
                Ok(output) if output.status.success() => ask_if_staged(),
                Ok(output) => tracing::warn!(
                    status = %output.status,
                    "the system update could not be started"
                ),
                Err(error) => tracing::warn!(%error, "the system update could not be started"),
            }
        });
    if let Err(error) = spawned {
        RUNNING.store(false, std::sync::atomic::Ordering::Release);
        tracing::warn!(%error, "could not start the system update");
    }
}

/// Clears the flag it holds when the update it guards ends, however it ends.
struct Done(&'static std::sync::atomic::AtomicBool);

impl Drop for Done {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// How long fetching and staging an image may take.
const UPDATE_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Ask to restart now when an image is staged: it is needed sooner than the
/// next regular request.
fn ask_if_staged() {
    if !staged() {
        return;
    }
    if let Some(path) = asked_path() {
        record_asked_at(&path, SystemTime::now());
    }
    queue_ask();
}

#[cfg(test)]
mod tests {
    use super::*;

    // USER REQUIREMENT (2026-10-18): "it's fine if we break compat with old versions every few days, people just need to update"
    #[test]
    fn a_staged_update_asks_for_a_restart_at_most_every_two_days() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let hours = |h: u64| start + Duration::from_secs(h * 60 * 60);
        assert!(!due(false, None, start), "nothing staged, nothing asked");
        assert!(due(true, None, start), "the first staged update asks");
        assert!(!due(true, Some(start), hours(1)));
        assert!(
            !due(true, Some(start), hours(47)),
            "updates batch for two days"
        );
        assert!(due(true, Some(start), hours(48)));
        assert!(!due(false, Some(start), hours(100)));
    }

    #[test]
    fn the_last_request_survives_a_restart_of_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tilo/update-restart-asked");
        assert_eq!(last_asked_at(&path), None);
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        record_asked_at(&path, at);
        assert_eq!(last_asked_at(&path), Some(at));
    }

    #[test]
    fn bootc_reports_a_staged_deployment() {
        assert!(staged_in(
            r#"{"status":{"staged":{"image":{}},"booted":{}}}"#
        ));
        assert!(!staged_in(r#"{"status":{"staged":null,"booted":{}}}"#));
        assert!(!staged_in("not json"));
    }
}
