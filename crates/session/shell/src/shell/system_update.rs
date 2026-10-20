//! The system on its newest version: applied live, with a restart asked for
//! only when part of it needs one, at most every two days.
//!
//! The update service fetches and stages the newest system image on its own
//! schedule (`bootc-fetch-apply-updates.timer`, at least daily), and apps and
//! the runtime they link update in the background without a restart
//! (`tilo-app-update.timer`). A staged image then applies live
//! (`tilo-live-update.service`, `tilo-extension live:system`): its changed
//! system services restart, its apps and runtime are new at their next
//! start, and what needs the desktop restarted (the compositor, the session
//! services, the interface versions apps speak) waits for this desktop to
//! finish it. Its state is [`LIVE_STATE`].
//!
//! This desktop finishes it when the person is away (the screen locked or
//! blanked) by starting `tilo-live-update-session.service`, which restarts
//! the desktop in place: the login stays and its windows reopen. Only a
//! kernel, bootloader or initramfs change, or an update that could not apply
//! live, needs a restart of the machine, and the person is asked for it. A
//! desktop restart and a request to restart share one clock: neither comes
//! more often than every [`ASK_INTERVAL`], so the updates of those days apply
//! together (docs/architecture/compatibility.md). An always-on host (a board,
//! [`tilo_power_mgmt::MachineDuty::Host`]) has nobody to ask and no work to
//! batch the restarts around: it finishes the update, restarting the desktop
//! or the machine, whenever it is away. An app or a collaborator
//! that needs a newer system than this one starts the update at once
//! ([`update_now_and_ask`]): what applies live applies before it returns, the
//! desktop finishes it at the next moment the person is away, and a restart
//! of the machine is asked for as soon as one is needed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use tilo_desktop::notifications::{Notification, NotificationSeverity};
use tilo_lock::MutexRecover as _;

use crate::shell::i18n::t;

/// The shortest time between two requests to restart, desktop restarts
/// included.
pub(crate) const ASK_INTERVAL: Duration = Duration::from_secs(2 * 24 * 60 * 60);
/// How long the person must have been away before the desktop restarts to
/// finish an update.
pub(crate) const AWAY_FOR: Duration = Duration::from_secs(2 * 60);
/// How often the shell looks for a staged image that needs a restart of the
/// machine. Staging happens at most a few times a day; this costs one file
/// check, or one `bootc status` where no live update runs.
const LOOK_EVERY: Duration = Duration::from_secs(60 * 60);
/// How often the shell looks whether the desktop can finish an update: one
/// small file read.
const LOOK_LIVE_EVERY: Duration = Duration::from_secs(60);
/// The first look, once the session has settled.
const FIRST_LOOK_AFTER: Duration = Duration::from_secs(10 * 60);
/// Where ostree records the deployment staged for the next boot.
const OSTREE_STAGED: &str = "/run/ostree/staged-deployment";
/// What the live update applied of the staged image, and what waits
/// (`tilo-extension live:status`).
pub(crate) const LIVE_STATE: &str = "/run/tilo/live-update/state.json";
/// The unit that fetches and stages the newest image now, which the person
/// at the machine may start (os/etc/polkit-1/rules.d/49-tilo-updates.rules).
const UPDATE_UNIT: &str = "tilo-deployment-update.service";
/// The unit that finishes a live update and restarts the desktop in place.
const SESSION_UNIT: &str = "tilo-live-update-session.service";
/// The notification action that restarts the machine.
pub(crate) const RESTART_ACTION: &str = "restart-to-update";

/// What the live update says of the staged image.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct LiveState {
    /// The staged image it describes; empty when nothing is staged.
    #[serde(default)]
    pub(crate) image: String,
    /// Part of the update waits for a desktop restart.
    #[serde(default)]
    pub(crate) desktop_pending: bool,
    /// Part of the update waits for a restart of the machine.
    #[serde(default)]
    pub(crate) reboot_required: bool,
}

/// Whether the person is at the machine, as the compositor last saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Presence {
    /// Time since the last input.
    pub(crate) idle: Duration,
    /// The screen is locked or blanked: nobody is looking.
    pub(crate) away: bool,
}

/// Whether to ask now: a restart is needed and the last request is at least
/// [`ASK_INTERVAL`] old, or there was none.
pub(crate) fn due(needed: bool, last_asked: Option<SystemTime>, now: SystemTime) -> bool {
    needed && interval_passed(last_asked, now)
}

fn interval_passed(last: Option<SystemTime>, now: SystemTime) -> bool {
    last.is_none_or(|asked| {
        now.duration_since(asked)
            .is_ok_and(|since| since >= ASK_INTERVAL)
    })
}

/// Whether the machine needs a restart for the staged update: what the live
/// update says, or, where none runs, that an image is staged at all.
pub(crate) fn restart_needed(staged: bool, live: Option<&LiveState>) -> bool {
    live.map_or(staged, |state| state.reboot_required)
}

/// Whether to restart the desktop now to finish the live update: part of it
/// waits for the desktop (and none for the machine, whose restart covers
/// it), the person is away, and the last restart or request is at least
/// [`ASK_INTERVAL`] old, unless something needs the update now (`urgent`)
/// or the machine is an always-on host (`host`).
pub(crate) fn desktop_restart_due(
    live: Option<&LiveState>,
    last: Option<SystemTime>,
    now: SystemTime,
    presence: Presence,
    urgent: bool,
    host: bool,
) -> bool {
    live.is_some_and(|state| state.desktop_pending && !state.reboot_required)
        && away(presence)
        && (urgent || host || interval_passed(last, now))
}

/// Whether an always-on host restarts itself now into the staged update:
/// the machine needs a restart and nobody is at it.
pub(crate) fn host_restart_due(needed: bool, presence: Option<Presence>) -> bool {
    needed && presence.is_some_and(away)
}

fn away(presence: Presence) -> bool {
    presence.away && presence.idle >= AWAY_FOR
}

fn host() -> bool {
    static HOST: OnceLock<bool> = OnceLock::new();
    *HOST.get_or_init(|| tilo_power_mgmt::this_machine_duty() == tilo_power_mgmt::MachineDuty::Host)
}

/// When the person was last asked, or the desktop last restarted, as
/// recorded at `path`.
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

/// The live update's state, where one runs.
fn live_state() -> Option<LiveState> {
    live_state_in(&std::fs::read_to_string(LIVE_STATE).ok()?)
}

fn live_state_in(json: &str) -> Option<LiveState> {
    serde_json::from_str(json).ok()
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
/// Whether the person is at the machine, as the compositor last saw.
static PRESENCE: Mutex<Option<(Presence, Instant)>> = Mutex::new(None);
/// Something needs the update now: the desktop finishes it at the next
/// moment the person is away, whatever the last restart.
static URGENT: AtomicBool = AtomicBool::new(false);

/// Record whether the person is at the machine; the compositor calls this
/// every tick.
pub(crate) fn note_presence(idle: Duration, away: bool) {
    *PRESENCE.lock_recover() = Some((Presence { idle, away }, Instant::now()));
}

/// The person's presence now, from the compositor's last report.
fn presence() -> Option<Presence> {
    let (presence, at) = (*PRESENCE.lock_recover())?;
    Some(Presence {
        idle: presence.idle.saturating_add(at.elapsed()),
        ..presence
    })
}

/// Start looking for staged images, once per process.
pub(crate) fn start() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let spawned = std::thread::Builder::new()
            .name("system-update".to_string())
            .spawn(|| {
                std::thread::sleep(FIRST_LOOK_AFTER);
                let mut looked: Option<Instant> = None;
                loop {
                    if looked.is_none_or(|at| at.elapsed() >= LOOK_EVERY) {
                        look();
                        looked = Some(Instant::now());
                    }
                    finish_live_update_if_due();
                    std::thread::sleep(LOOK_LIVE_EVERY);
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
    let live = live_state();
    let needed = match &live {
        Some(state) => restart_needed(false, Some(state)),
        None => restart_needed(staged(), None),
    };
    if host() {
        if host_restart_due(needed, presence()) {
            tracing::info!("finishing the system update: restarting this always-on host");
            if let Err(error) =
                os_actions::session::perform_session_action(os_actions::SessionAction::Restart)
            {
                tracing::warn!(%error, "the host could not restart into the system update");
            }
        }
        return;
    }
    if !due(needed, last_asked_at(&path), now) {
        return;
    }
    record_asked_at(&path, now);
    queue_ask();
}

/// Restart the desktop to finish the live update, when that is due.
fn finish_live_update_if_due() {
    let (Some(path), Some(presence)) = (asked_path(), presence()) else {
        return;
    };
    let now = SystemTime::now();
    let urgent = URGENT.load(Ordering::Acquire);
    if !desktop_restart_due(
        live_state().as_ref(),
        last_asked_at(&path),
        now,
        presence,
        urgent,
        host(),
    ) {
        return;
    }
    record_asked_at(&path, now);
    URGENT.store(false, Ordering::Release);
    tracing::info!("finishing the system update: restarting the desktop in place");
    // The service restarts this desktop; this call ends with it. A failure
    // is rolled back by the service and leaves the update to the next
    // restart of the machine, which [`look`] then asks for.
    match bounded_command::run(
        "systemctl",
        &["start", "--no-ask-password", SESSION_UNIT],
        None,
        64 * 1024,
        SESSION_TIMEOUT,
    ) {
        Ok(output) if output.status.success() => {}
        Ok(output) => tracing::warn!(
            status = %output.status,
            "the live system update could not be finished"
        ),
        Err(error) => tracing::warn!(%error, "the live system update could not be finished"),
    }
}

/// How long finishing a live update may take, the desktop restart included.
const SESSION_TIMEOUT: Duration = Duration::from_secs(15 * 60);

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

/// Fetch, stage and apply the newest system now, on a worker, whatever the
/// last request: something (an app, a collaborator's device) needs a newer
/// version than this device runs. What applies live has applied when the
/// update unit returns; the desktop finishes the rest at the next moment the
/// person is away, and a restart of the machine is asked for at once when
/// one is needed.
pub(crate) fn update_now_and_ask() {
    // One at a time: asking again while it runs changes nothing.
    static RUNNING: AtomicBool = AtomicBool::new(false);
    if RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("system-update-now".to_string())
        .spawn(|| {
            let _done = Done(&RUNNING);
            // A oneshot unit: `start` returns once the image is staged and
            // applied live, or once there was nothing newer to stage.
            match bounded_command::run(
                "systemctl",
                &["start", "--no-ask-password", UPDATE_UNIT],
                None,
                64 * 1024,
                UPDATE_TIMEOUT,
            ) {
                Ok(output) if output.status.success() => after_urgent_update(),
                Ok(output) => tracing::warn!(
                    status = %output.status,
                    "the system update could not be started"
                ),
                Err(error) => tracing::warn!(%error, "the system update could not be started"),
            }
        });
    if let Err(error) = spawned {
        RUNNING.store(false, Ordering::Release);
        tracing::warn!(%error, "could not start the system update");
    }
}

/// Clears the flag it holds when the update it guards ends, however it ends.
struct Done(&'static AtomicBool);

impl Drop for Done {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// How long fetching, staging and applying an image may take.
const UPDATE_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// After an update something needs now: ask to restart at once when the
/// machine needs it, else let the desktop finish at its next chance.
fn after_urgent_update() {
    let live = live_state();
    let needed = match &live {
        Some(state) => restart_needed(false, Some(state)),
        None => restart_needed(staged(), None),
    };
    if needed {
        if let Some(path) = asked_path() {
            record_asked_at(&path, SystemTime::now());
        }
        queue_ask();
    } else if live.is_some_and(|state| state.desktop_pending) {
        URGENT.store(true, Ordering::Release);
    }
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

    // USER REQUIREMENT (2026-10-18): "it's fine if we break compat with old versions every few days, people just need to update"
    #[test]
    fn only_what_a_live_update_cannot_apply_asks_for_a_restart_of_the_machine() {
        let applied = LiveState {
            image: "sha256:aa".into(),
            desktop_pending: true,
            reboot_required: false,
        };
        assert!(
            !restart_needed(true, Some(&applied)),
            "a staged image that applied live needs no restart"
        );
        let kernel = LiveState {
            reboot_required: true,
            ..applied.clone()
        };
        assert!(restart_needed(true, Some(&kernel)));
        assert!(
            restart_needed(true, None),
            "without a live update a staged image applies at the next restart"
        );
        assert!(!restart_needed(false, None));
    }

    // USER REQUIREMENT (2026-10-18): "it's fine if we break compat with old versions every few days, people just need to update"
    #[test]
    fn the_desktop_restarts_to_finish_an_update_only_while_the_person_is_away_and_at_most_every_two_days()
     {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let hours = |h: u64| start + Duration::from_secs(h * 60 * 60);
        let pending = LiveState {
            image: "sha256:aa".into(),
            desktop_pending: true,
            reboot_required: false,
        };
        let away = Presence {
            idle: Duration::from_secs(10 * 60),
            away: true,
        };
        let working = Presence {
            idle: Duration::from_secs(1),
            away: false,
        };
        assert!(desktop_restart_due(
            Some(&pending),
            None,
            start,
            away,
            false,
            false
        ));
        assert!(
            !desktop_restart_due(Some(&pending), None, start, working, false, false),
            "never while the person works"
        );
        assert!(
            !desktop_restart_due(
                Some(&pending),
                None,
                start,
                Presence {
                    idle: Duration::from_secs(30 * 60),
                    away: false
                },
                false,
                false
            ),
            "idle with the screen on (a video) is not away"
        );
        assert!(
            !desktop_restart_due(
                Some(&pending),
                None,
                start,
                Presence {
                    idle: Duration::from_secs(10),
                    away: true
                },
                false,
                false
            ),
            "a lock the person just set is not yet away"
        );
        assert!(
            !desktop_restart_due(Some(&pending), Some(start), hours(47), away, false, false),
            "one restart or request every two days at most"
        );
        assert!(desktop_restart_due(
            Some(&pending),
            Some(start),
            hours(48),
            away,
            false,
            false
        ));
        assert!(
            desktop_restart_due(Some(&pending), Some(start), hours(1), away, true, false),
            "an update something needs now finishes at the next chance"
        );
        assert!(
            desktop_restart_due(Some(&pending), Some(start), hours(1), away, false, true),
            "an always-on host finishes whenever it is away"
        );
        assert!(
            !desktop_restart_due(Some(&pending), Some(start), hours(1), working, false, true),
            "and never while someone works at it"
        );
        let done = LiveState {
            desktop_pending: false,
            ..pending.clone()
        };
        assert!(!desktop_restart_due(
            Some(&done),
            None,
            start,
            away,
            false,
            false
        ));
        let kernel = LiveState {
            reboot_required: true,
            ..pending
        };
        assert!(
            !desktop_restart_due(Some(&kernel), None, start, away, false, false),
            "the restart of the machine covers it"
        );
        assert!(!desktop_restart_due(None, None, start, away, false, false));
    }

    #[test]
    fn the_live_state_reads_what_the_helper_writes() {
        let state = live_state_in(
            r#"{"schema":1,"image":"sha256:aa","applied":"system","desktop_pending":true,
                "reboot_required":false,"failed":false,"deferred":["greetd.service"],"detail":""}"#,
        )
        .unwrap();
        assert_eq!(
            state,
            LiveState {
                image: "sha256:aa".into(),
                desktop_pending: true,
                reboot_required: false,
            }
        );
        assert!(live_state_in("not json").is_none());
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

    #[test]
    fn an_always_on_host_restarts_into_an_update_only_while_nobody_is_at_it() {
        let away = Presence {
            idle: Duration::from_secs(10 * 60),
            away: true,
        };
        let working = Presence {
            idle: Duration::from_secs(1),
            away: false,
        };
        assert!(host_restart_due(true, Some(away)));
        assert!(!host_restart_due(true, Some(working)));
        assert!(!host_restart_due(true, None), "presence not yet known");
        assert!(!host_restart_due(false, Some(away)), "nothing staged");
    }
}
