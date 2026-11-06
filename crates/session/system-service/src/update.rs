//! The system update: what the update service staged, and what finishing
//! it needs.
//!
//! The update service fetches and stages the newest system image on its
//! own schedule, and a staged image applies live; what waits for the
//! desktop or a restart of the machine is in the live update's state
//! ([`LIVE_STATE`]). Where no live update runs, an image staged at all
//! needs a restart.
//!
//! The desktop is finished in place ([`restart_desktop`]): the login stays
//! and its windows reopen on the new interface. That is done when nobody is
//! at the machine ([`Presence`]), at most every [`ASK_INTERVAL`], and the
//! person is asked for a restart of the machine at the same pace. Nothing
//! here decides when: [`desktop_restart_due`], [`ask_due`] and
//! [`host_restart_due`] say whether, and the session's loop asks them.
//!
//! A deployment that did not start leaves the machine on the previous one
//! before any session runs (`tilo-deployment fallback`), and writes what it
//! found under [`BOOT_FALLBACK_DIR`]. Those reports are facts of this
//! machine, read here ([`fallbacks`]) and carried in the resource's reading
//! ([`Update::fell_back`]) for the session to tell the person, once each.

use std::path::Path;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// What the live update applied of the staged image, and what waits.
pub const LIVE_STATE: &str = "/run/tilo/live-update/state.json";
/// Where ostree records the deployment staged for the next boot.
const OSTREE_STAGED: &str = "/run/ostree/staged-deployment";
/// Where a deployment that did not start leaves its report: one directory per
/// fallback, `<when>-<composefs>`, holding `report`, `summary.txt` and what
/// the failed tries left behind (os/usr/libexec/tilo-deployment).
pub const BOOT_FALLBACK_DIR: &str = "/var/lib/tilo/boot-fallback";
/// The unit that fetches and stages the newest image now, which the person
/// at the machine may start (os/etc/polkit-1/rules.d/49-tilo-updates.rules).
pub const UPDATE_UNIT: &str = "tilo-deployment-update.service";
/// How long fetching, staging and applying an image may take.
pub const UPDATE_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// The unit that finishes a live update: it merges what is left of the
/// update, restarts the session services it changed, and restarts every Tilo
/// desktop on this machine in place, which comes back with its windows
/// reopened (`tilo-extension live:session`).
pub const SESSION_UNIT: &str = "tilo-live-update-session.service";
/// How long finishing a live update may take, the desktop restart included.
pub const SESSION_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// The `system-update` action that fetches and applies the newest system now.
pub const UPDATE_NOW: &str = "update-now";
/// The `system-update` action that restarts the desktop onto the new
/// interface to finish the update.
pub const RESTART_DESKTOP: &str = "restart-desktop";
/// The `system-update` action that restarts the machine into the staged
/// update.
pub const RESTART_TO_UPDATE: &str = "restart-to-update";

/// The shortest time between two requests to restart the machine and two
/// desktop restarts alike: the updates of those days apply together, and
/// neither interrupts the person more often than every two days.
pub const ASK_INTERVAL: Duration = Duration::from_secs(2 * 24 * 60 * 60);
/// How long the person must have been away before the desktop restarts to
/// finish an update.
pub const AWAY_FOR: Duration = Duration::from_secs(2 * 60);

/// Whether the person is at the machine, as the compositor last saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Presence {
    /// Time since the last input.
    pub idle: Duration,
    /// The screen is locked or blanked: nobody is looking.
    pub away: bool,
}

impl Presence {
    /// Nobody has been at the machine for [`AWAY_FOR`]: locked or blanked,
    /// and no input since.
    #[must_use]
    pub fn no_one_there(&self) -> bool {
        self.away && self.idle >= AWAY_FOR
    }
}

/// Whether the last request or restart is at least [`ASK_INTERVAL`] old, or
/// there was none.
#[must_use]
pub fn interval_passed(last: Option<SystemTime>, now: SystemTime) -> bool {
    last.is_none_or(|last| {
        now.duration_since(last)
            .is_ok_and(|since| since >= ASK_INTERVAL)
    })
}

/// Whether to ask the person now: the machine needs a restart for the staged
/// update, and the last request or restart is at least [`ASK_INTERVAL`] old.
#[must_use]
pub fn ask_due(needed: bool, last: Option<SystemTime>, now: SystemTime) -> bool {
    needed && interval_passed(last, now)
}

/// Whether to restart the desktop now to finish the update `update`: part of
/// it waits for the desktop (and none for the machine, whose restart covers
/// it), nobody is at the machine, and the last restart or request is at
/// least [`ASK_INTERVAL`] old, unless something needs the update now
/// (`urgent`) or the machine is an always-on host (`host`).
#[must_use]
pub fn desktop_restart_due(
    update: &Update,
    last: Option<SystemTime>,
    now: SystemTime,
    presence: Presence,
    urgent: bool,
    host: bool,
) -> bool {
    update.desktop_pending
        && !update.restart_needed
        && presence.no_one_there()
        && (urgent || host || interval_passed(last, now))
}

/// Whether an always-on host restarts itself now into the staged update: the
/// machine needs a restart and nobody is at it.
#[must_use]
pub fn host_restart_due(needed: bool, presence: Option<Presence>) -> bool {
    needed && presence.is_some_and(|presence| presence.no_one_there())
}

/// What the live update says of the staged image.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct LiveState {
    /// The staged image it describes; empty when nothing is staged.
    #[serde(default)]
    pub image: String,
    /// Part of the update waits for the desktop to restart.
    #[serde(default)]
    pub desktop_pending: bool,
    /// Part of the update waits for a restart of the machine.
    #[serde(default)]
    pub reboot_required: bool,
}

/// A deployment this machine went back from: it did not start within its
/// tries, so the boot loader started the previous one and the machine settled
/// on it. What the boot-time settling recorded of it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Fallback {
    /// The report's directory name, `<when>-<composefs>`, which names the
    /// fallback once per machine: the session tells each of them once.
    pub id: String,
    /// The image that did not start.
    pub image: Option<String>,
    /// Its version, where the image has one.
    pub version: Option<String>,
    /// How many times it was tried.
    pub tries: Option<usize>,
    /// The version the machine runs instead.
    pub running: Option<String>,
    /// The report a person reads of what the failed tries left behind.
    pub summary: String,
}

/// The `system-update` resource.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Update {
    /// The staged image, when the live update names it.
    pub staged: Option<String>,
    /// Part of it waits for the desktop to restart.
    pub desktop_pending: bool,
    /// The machine must restart to finish it.
    pub restart_needed: bool,
    /// An update asked for here is being fetched and applied.
    pub updating: bool,
    pub pending: Vec<String>,
    pub problem: Option<String>,
    /// Firmware updates `fwupd` offers; `None` where it is not installed.
    pub firmware: Option<usize>,
    /// The deployments this machine went back from, oldest first.
    pub fell_back: Vec<Fallback>,
}

impl Update {
    /// The reading of the live update's state `live`, else of whether an
    /// image is `staged` at all.
    #[must_use]
    pub fn of(staged: bool, live: Option<&LiveState>) -> Self {
        Self {
            staged: live
                .map(|state| state.image.clone())
                .filter(|image| !image.is_empty()),
            desktop_pending: live.is_some_and(|state| state.desktop_pending),
            restart_needed: restart_needed(staged, live),
            ..Self::default()
        }
    }
}

/// Whether the machine needs a restart for the staged update: what the live
/// update says, or, where none runs, that an image is staged at all.
#[must_use]
pub fn restart_needed(staged: bool, live: Option<&LiveState>) -> bool {
    live.map_or(staged, |state| state.reboot_required)
}

/// The live update's state, where one runs.
#[must_use]
pub fn live_state() -> Option<LiveState> {
    live_state_in(&std::fs::read_to_string(LIVE_STATE).ok()?)
}

fn live_state_in(json: &str) -> Option<LiveState> {
    serde_json::from_str(json).ok()
}

/// The deployments this machine went back from, oldest first, as the
/// boot-time settling recorded them.
#[must_use]
pub fn fallbacks() -> Vec<Fallback> {
    fallbacks_in(Path::new(BOOT_FALLBACK_DIR))
}

/// The reports under `dir`, oldest first; none where the machine never went
/// back or they cannot be read.
#[must_use]
pub fn fallbacks_in(dir: &Path) -> Vec<Fallback> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut reports: Vec<Fallback> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let id = entry.file_name().into_string().ok()?;
            fallback_at(&entry.path(), id)
        })
        .collect();
    reports.sort_by(|left, right| left.id.cmp(&right.id));
    reports
}

/// The fallback `dir` recorded under the name `id`, where it has a report.
fn fallback_at(dir: &Path, id: String) -> Option<Fallback> {
    let report = std::fs::read_to_string(dir.join("report")).ok()?;
    let field = |key: &str| {
        report
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(str::to_owned)
            .filter(|value| !value.is_empty())
    };
    Some(Fallback {
        image: field("image"),
        version: field("version"),
        tries: field("tries").and_then(|tries| tries.parse().ok()),
        running: field("running_version"),
        summary: dir.join("summary.txt").display().to_string(),
        id,
    })
}

/// Whether an image waits for the next restart.
#[must_use]
pub fn staged() -> bool {
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

/// Fetch, stage and apply the newest system now: the update unit returns
/// once the image is staged and applied live, or there was nothing newer.
///
/// # Errors
/// The unit did not run, or failed.
pub fn update_now() -> Result<(), String> {
    let output = bounded_command::run(
        "systemctl",
        &["start", "--no-ask-password", UPDATE_UNIT],
        None,
        64 * 1024,
        UPDATE_TIMEOUT,
    )
    .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("the system update ended with {}", output.status))
    }
}

/// Finish the update that waits for the desktop: the unit merges what is
/// left, restarts the session services it changed, and restarts every Tilo
/// desktop on this machine in place. This desktop comes back on the new
/// interface with the windows it had open, and the login stays.
///
/// # Errors
/// The unit did not run, or failed; the live update rolls a failed update
/// back and leaves it to the next restart of the machine.
pub fn restart_desktop() -> Result<(), String> {
    let output = bounded_command::run(
        "systemctl",
        &["start", "--no-ask-password", SESSION_UNIT],
        None,
        64 * 1024,
        SESSION_TIMEOUT,
    )
    .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "finishing the system update ended with {}",
            output.status
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_update_decides_whether_the_machine_restarts() {
        let live = |desktop_pending, reboot_required| LiveState {
            image: "ghcr.io/tilo/os:42".into(),
            desktop_pending,
            reboot_required,
        };
        assert!(!restart_needed(false, None));
        assert!(restart_needed(true, None), "staged with no live update");
        assert!(
            !restart_needed(true, Some(&live(true, false))),
            "applied live: the desktop finishes it"
        );
        assert!(restart_needed(false, Some(&live(false, true))));
        let update = Update::of(true, Some(&live(true, false)));
        assert_eq!(update.staged.as_deref(), Some("ghcr.io/tilo/os:42"));
        assert!(update.desktop_pending && !update.restart_needed);
    }

    #[test]
    fn states_read_from_their_json() {
        assert_eq!(
            live_state_in(r#"{"image":"x","desktop_pending":true}"#),
            Some(LiveState {
                image: "x".into(),
                desktop_pending: true,
                reboot_required: false
            })
        );
        assert!(live_state_in("not json").is_none());
        assert!(staged_in(r#"{"status":{"staged":{"image":"x"}}}"#));
        assert!(!staged_in(r#"{"status":{"staged":null}}"#));
        assert!(!staged_in("{}"));
    }

    #[test]
    fn a_staged_update_asks_for_a_restart_at_most_every_two_days() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let hours = |h: u64| start + Duration::from_secs(h * 60 * 60);
        assert!(
            !ask_due(false, None, start),
            "nothing staged, nothing asked"
        );
        assert!(ask_due(true, None, start), "the first staged update asks");
        assert!(!ask_due(true, Some(start), hours(1)));
        assert!(
            !ask_due(true, Some(start), hours(47)),
            "updates batch for two days"
        );
        assert!(ask_due(true, Some(start), hours(48)));
        assert!(!ask_due(false, Some(start), hours(100)));
    }

    #[test]
    fn the_desktop_restarts_to_finish_an_update_only_while_the_person_is_away_and_at_most_every_two_days()
     {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let hours = |h: u64| start + Duration::from_secs(h * 60 * 60);
        let pending = Update {
            staged: Some("sha256:aa".into()),
            desktop_pending: true,
            ..Update::default()
        };
        let away = Presence {
            idle: Duration::from_secs(10 * 60),
            away: true,
        };
        let working = Presence {
            idle: Duration::from_secs(1),
            away: false,
        };
        let due = |update: &Update, last, at, presence, urgent, host| {
            desktop_restart_due(update, last, at, presence, urgent, host)
        };
        assert!(due(&pending, None, start, away, false, false));
        assert!(
            !due(&pending, None, start, working, false, false),
            "never while the person works"
        );
        assert!(
            !due(
                &pending,
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
            !due(
                &pending,
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
            !due(&pending, Some(start), hours(47), away, false, false),
            "one restart or request every two days at most"
        );
        assert!(due(&pending, Some(start), hours(48), away, false, false));
        assert!(
            due(&pending, Some(start), hours(1), away, true, false),
            "an update something needs now finishes at the next chance"
        );
        assert!(
            due(&pending, Some(start), hours(1), away, false, true),
            "an always-on host finishes whenever it is away"
        );
        assert!(
            !due(&pending, Some(start), hours(1), working, false, true),
            "and never while someone works at it"
        );
        let done = Update {
            desktop_pending: false,
            ..pending.clone()
        };
        assert!(!due(&done, None, start, away, false, false));
        let kernel = Update {
            restart_needed: true,
            ..pending
        };
        assert!(
            !due(&kernel, None, start, away, false, false),
            "the restart of the machine covers it"
        );
        assert!(!due(&Update::default(), None, start, away, false, false));
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

    /// A deployment the machine went back from left a report: what did not
    /// start, how many tries it spent, and what runs instead, read from the
    /// files the boot-time settling wrote.
    #[test]
    fn a_deployment_the_machine_went_back_from_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let reports = dir.path();
        std::fs::create_dir(reports.join("20261025T101010Z-0a1b2c3d4e5f6a7b")).unwrap();
        std::fs::write(
            reports.join("20261025T101010Z-0a1b2c3d4e5f6a7b/report"),
            "image=registry.tilo.so/tilo/tilo-vm:main-1a2b3c4\n\
             version=44.20261023.0\n\
             built=2026-10-23T21:40:00Z\n\
             composefs=0a1b2c3d4e5f6a7b\n\
             tries=3\n\
             logged=1\n\
             running_image=registry.tilo.so/tilo/tilo-vm:main-0f0e0d0\n\
             running_version=44.20261022.0\n\
             settled=yes\n",
        )
        .unwrap();
        std::fs::write(
            reports.join("20261025T101010Z-0a1b2c3d4e5f6a7b/summary.txt"),
            "Tilo went back to the version that worked before.\n",
        )
        .unwrap();
        // A directory without a report is nothing the machine went back from.
        std::fs::create_dir(reports.join("not-a-fallback")).unwrap();

        let fallbacks = fallbacks_in(reports);
        assert_eq!(fallbacks.len(), 1);
        let fallback = &fallbacks[0];
        assert_eq!(fallback.id, "20261025T101010Z-0a1b2c3d4e5f6a7b");
        assert_eq!(
            fallback.image.as_deref(),
            Some("registry.tilo.so/tilo/tilo-vm:main-1a2b3c4")
        );
        assert_eq!(fallback.version.as_deref(), Some("44.20261023.0"));
        assert_eq!(fallback.tries, Some(3));
        assert_eq!(fallback.running.as_deref(), Some("44.20261022.0"));
        assert_eq!(
            fallback.summary,
            reports
                .join("20261025T101010Z-0a1b2c3d4e5f6a7b/summary.txt")
                .display()
                .to_string()
        );
        assert!(
            fallbacks_in(&reports.join("absent")).is_empty(),
            "no reports is nothing to say"
        );
    }

    /// The resource's reading carries the reports, and one published by a
    /// machine that never went back carries none.
    #[test]
    fn the_reading_carries_the_fallbacks() {
        let update = Update {
            fell_back: vec![Fallback {
                id: "20261025T101010Z-0a1b2c3d4e5f6a7b".into(),
                ..Fallback::default()
            }],
            ..Update::default()
        };
        let json = serde_json::to_string(&update).unwrap();
        assert!(json.contains("\"fell-back\""), "{json}");
        assert_eq!(serde_json::from_str::<Update>(&json).unwrap(), update);
        assert!(Update::default().fell_back.is_empty());
        assert!(
            serde_json::from_str::<Update>(r#"{"staged":"x"}"#)
                .unwrap()
                .fell_back
                .is_empty(),
            "a reading from before this field says nothing fell back"
        );
    }
}
