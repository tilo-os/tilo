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

use serde::Deserialize;
use tilo_deployment_status::Deployments;
pub use tilo_system_resources::update::*;

/// What the live update applied of the staged image, and what waits.
pub const LIVE_STATE: &str = os_actions::LIVE_UPDATE_STATE;
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
/// The unit that boots the previous deployment from now on, which the person at the machine may
/// start (os/etc/polkit-1/rules.d/49-tilo-updates.rules). It reorders the boot loader's entries
/// and writes the next boot's default; the restart that follows is what makes it take effect.
pub const ROLLBACK_UNIT: &str = "tilo-deployment-rollback.service";
/// How long going back may take: a handful of renames on the ESP and one EFI variable write.
pub const ROLLBACK_TIMEOUT: Duration = Duration::from_secs(2 * 60);

/// The unit that finishes a live update: it merges what is left of the
/// update, restarts the session services it changed, and restarts every Tilo
/// desktop on this machine in place, which comes back with its windows
/// reopened (`tilo-extension live:session`).
pub const SESSION_UNIT: &str = "tilo-live-update-session.service";
/// How long finishing a live update may take, the desktop restart included.
pub const SESSION_TIMEOUT: Duration = Duration::from_secs(15 * 60);

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

/// What the live update says of the staged image
/// (`/run/tilo/live-update/state.json`, written by `tilo-extension live:*`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LiveState {
    /// The digest of the staged image it describes; empty when nothing is staged.
    pub image: String,
    /// The last part of the update applied live: `system`, then `session`.
    pub applied: Option<String>,
    /// Part of the update waits for the desktop to restart.
    pub desktop_pending: bool,
    /// Part of the update waits for a restart of the machine.
    pub reboot_required: bool,
    /// Applying it live did not finish and was taken back.
    pub failed: bool,
    /// Why it failed, in the live update's words.
    pub detail: String,
}

/// The reading of the deployments `deployments` lists and the live update's state `live`.
/// Where the deployments cannot be seen, `staged` says whether an update is staged at all.
///
/// The state speaks of one image, by its digest: a state an earlier update left says nothing
/// of another staged since, which then waits for the live update or a restart.
#[must_use]
pub fn reading(
    deployments: Option<&Deployments>,
    staged: bool,
    live: Option<&LiveState>,
) -> Update {
    let live = live.filter(|state| !state.image.is_empty());
    let staged = match deployments {
        Some(deployments) => deployments.staged.clone(),
        None => live
            .map(|state| Deployment {
                digest: Some(state.image.clone()),
                ..Deployment::default()
            })
            .or_else(|| staged.then(Deployment::default)),
    };
    let live = live.filter(|state| {
        deployments.is_none()
            || staged.as_ref().and_then(|staged| staged.digest.as_deref())
                == Some(state.image.as_str())
    });
    let failed = live
        .filter(|state| state.failed)
        .map(|state| state.detail.clone());
    let desktop_pending = live.is_some_and(|state| state.desktop_pending) && failed.is_none();
    let restart_needed = match live {
        Some(state) => state.reboot_required || state.failed,
        None => staged.is_some(),
    };
    Update {
        booted: deployments.and_then(|deployments| deployments.booted.clone()),
        rollback: deployments.and_then(|deployments| deployments.rollback.clone()),
        applied: live.is_some_and(|state| state.applied.is_some())
            && failed.is_none()
            && !desktop_pending
            && !restart_needed,
        desktop_pending,
        restart_needed,
        failed,
        staged,
        ..Update::default()
    }
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

/// The deployments on this machine, as the deployment status service lists them to its admins.
///
/// # Errors
/// No such service, or the account is not an admin's.
pub fn deployments() -> Result<Deployments, String> {
    let socket = tilo_deployment_status::socket()
        .ok_or_else(|| "this system lists its deployments to nobody but root".to_owned())?;
    tilo_deployment_status::read(&socket).map_err(|error| error.to_string())
}

/// Whether ostree marks an image staged for the next start: what a member who may not see the
/// deployments still learns.
#[must_use]
pub fn staged_marked() -> bool {
    Path::new(OSTREE_STAGED).exists()
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

/// Boot the previous deployment from now on: the unit reorders the boot loader's entries and
/// writes the next boot's default. The machine restarts into it afterward.
///
/// # Errors
/// The unit did not run, or the account may not start it: polkit allows only the person at this
/// machine's seat, in `wheel`, to roll it back.
pub fn rollback() -> Result<(), String> {
    let output = bounded_command::run(
        "systemctl",
        &["start", "--no-ask-password", ROLLBACK_UNIT],
        None,
        64 * 1024,
        ROLLBACK_TIMEOUT,
    )
    .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("the rollback ended with {}", output.status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deployment(digest: &str) -> Deployment {
        Deployment {
            image: Some("registry.tilo.so/tilo/tilo-vm:x86_64-ci".into()),
            digest: Some(digest.into()),
            version: Some(format!("44.{digest}")),
            built: None,
        }
    }

    fn on_disk(staged: Option<&str>) -> Deployments {
        Deployments {
            booted: Some(deployment("sha256:old")),
            staged: staged.map(deployment),
            rollback: Some(deployment("sha256:older")),
        }
    }

    fn state(json: &str) -> LiveState {
        live_state_in(json).unwrap()
    }

    tilo_test_env::scenario! {
        fn the_reading_says_what_the_staged_update_needs;

        phase nothing_staged_is_nothing_to_do;
        {
            let update = reading(Some(&on_disk(None)), false, None);
            assert_eq!(update.booted, Some(deployment("sha256:old")));
            assert_eq!(update.rollback, Some(deployment("sha256:older")), "what it can go back to");
            assert_eq!(update.staged, None);
            assert!(!update.restart_needed && !update.desktop_pending && !update.applied);
            assert_eq!(update.failed, None);
        }

        phase an_update_applied_live_waits_only_for_the_desktop;
        {
            let live = state(
                r#"{"schema":1,"image":"sha256:new","applied":"system","desktop_pending":true,
                    "reboot_required":false,"failed":false,"detail":""}"#,
            );
            let update = reading(Some(&on_disk(Some("sha256:new"))), false, Some(&live));
            assert_eq!(update.staged, Some(deployment("sha256:new")));
            assert!(update.desktop_pending && !update.restart_needed && !update.applied);
        }

        phase finishing_the_desktop_half_leaves_the_update_running;
        {
            let live = state(
                r#"{"schema":1,"image":"sha256:new","applied":"session","desktop_pending":false,
                    "reboot_required":false,"failed":false,"detail":""}"#,
            );
            let update = reading(Some(&on_disk(Some("sha256:new"))), false, Some(&live));
            assert!(update.applied, "nothing of it waits");
            assert!(!update.desktop_pending && !update.restart_needed);
        }

        phase a_kernel_change_waits_for_a_restart_of_the_machine;
        {
            let live = state(
                r#"{"schema":1,"image":"sha256:new","applied":"system","desktop_pending":true,
                    "reboot_required":true,"failed":false,"detail":""}"#,
            );
            let update = reading(Some(&on_disk(Some("sha256:new"))), false, Some(&live));
            assert!(update.restart_needed && !update.applied);
        }

        phase a_live_update_that_failed_says_why_and_needs_a_restart;
        {
            let live = state(
                r#"{"schema":1,"image":"sha256:new","applied":null,"desktop_pending":false,
                    "reboot_required":true,"failed":true,
                    "detail":"the desktop did not come back"}"#,
            );
            let update = reading(Some(&on_disk(Some("sha256:new"))), false, Some(&live));
            assert_eq!(update.failed.as_deref(), Some("the desktop did not come back"));
            assert!(update.restart_needed && !update.desktop_pending && !update.applied);
        }

        phase a_state_left_by_an_earlier_update_says_nothing_of_a_newer_one;
        {
            let live = state(
                r#"{"schema":1,"image":"sha256:new","applied":null,"failed":true,
                    "reboot_required":true,"detail":"x"}"#,
            );
            let update = reading(Some(&on_disk(Some("sha256:newer"))), false, Some(&live));
            assert_eq!(update.failed, None, "the failure was of another image");
            assert!(update.restart_needed, "a staged image the live update has not taken waits");
            let update = reading(Some(&on_disk(None)), false, Some(&live));
            assert_eq!(update.failed, None, "nothing staged any more: nothing failed");
            assert!(!update.restart_needed);
        }

        phase a_member_who_may_not_see_the_deployments_reads_the_live_update;
        {
            let live = state(
                r#"{"schema":1,"image":"sha256:new","applied":"system","desktop_pending":true}"#,
            );
            let update = reading(None, false, Some(&live));
            assert_eq!(update.booted, None);
            assert_eq!(
                update.staged.and_then(|staged| staged.digest).as_deref(),
                Some("sha256:new")
            );
            assert!(update.desktop_pending);
            let update = reading(None, true, None);
            assert!(update.staged.is_some() && update.restart_needed, "staged with no live update");
            assert!(!reading(None, false, None).restart_needed);
        }
    }

    #[test]
    fn states_read_from_their_json() {
        assert_eq!(
            live_state_in(r#"{"image":"x","desktop_pending":true}"#),
            Some(LiveState {
                image: "x".into(),
                desktop_pending: true,
                ..LiveState::default()
            })
        );
        assert!(live_state_in("not json").is_none());
    }

    /// What the machine asks for, and when: a staged update asks at most
    /// every two days, an always-on host restarts into it while nobody is at
    /// it, and a desktop restart waits until the person is away.
    #[test]
    #[allow(clippy::too_many_lines, reason = "one scenario, read top to bottom")]
    fn an_update_is_asked_for_and_restarted_into_on_a_schedule_it_keeps() {
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

        let pending = Update {
            staged: Some(Deployment {
                digest: Some("sha256:aa".into()),
                ..Deployment::default()
            }),
            desktop_pending: true,
            ..Update::default()
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
            serde_json::from_str::<Update>(r#"{"restart-needed":true}"#)
                .unwrap()
                .fell_back
                .is_empty(),
            "a reading from before this field says nothing fell back"
        );
    }
}
