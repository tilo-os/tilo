//! Finishing the staged system update: applied live, with the desktop
//! restarted onto the new interface once the person has stepped away, and a
//! restart of the machine asked for at most every two days.
//!
//! The update service stages the newest image on its own schedule and the
//! live update applies what it can in place: the system services restart,
//! the apps and the runtime are new at their next start. What waits for the
//! desktop (the compositor, the session services, the interface versions apps
//! speak) is this module's to finish, through the `system-update` resource's
//! own actions ([`tilo_system_service::update::RESTART_DESKTOP`]): the
//! session invokes one exactly as an app or the person would, and the
//! service's worker starts the unit that restarts this desktop in place. The
//! login stays, and the session comes back on the new interface with the
//! windows it had open.
//!
//! Nothing is finished while someone is at the machine: they are away when
//! the screen is locked or blanked and no input has come since
//! ([`tilo_system_service::update::AWAY_FOR`]). A desktop restart and a
//! request to restart the machine share one clock
//! ([`tilo_system_service::update::ASK_INTERVAL`]), so the updates of those
//! two days apply together. An always-on host has nobody to ask and no work
//! to batch the restarts around: it finishes the update whenever it is away.
//!
//! Something that needs a newer system now (a collaborator's device at a
//! newer protocol version) has the session fetch the update
//! ([`tilo_system_service::update::UPDATE_NOW`]); what applied live is
//! finished at the next moment the person is away, whatever the last restart.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant, SystemTime};

use tilo_notifications_service::{Notifications, Taken};
use tilo_system_service::update::{self, Presence, Update};

use crate::Session;
use crate::notices::RESTART_ACTION;

/// When the session first looks for an update to finish: once the desk has
/// settled.
const FIRST_LOOK_AFTER: Duration = Duration::from_secs(10 * 60);
/// How often it looks again. Both looks are one file read and one reading.
const LOOK_EVERY: Duration = Duration::from_secs(60);

/// What finishing the update asks of the session now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Due {
    /// Fetch and apply the newest system now: something needs it.
    UpdateNow,
    /// Restart the desktop onto the new interface.
    RestartDesktop,
    /// Restart the machine into the staged update.
    RestartMachine,
    /// Tell the person an update waits for a restart of the machine.
    Ask,
}

/// The clock on finishing the update, and what the person asked of its
/// notice.
pub(crate) struct Finish {
    /// When it next looks.
    next: Instant,
    /// Something needed the update now, and what applied live waits for the
    /// desktop: the next moment the person is away finishes it.
    urgent: bool,
    /// Whether this session saw an update run, so the reading that says it
    /// ended is the one that says whether to finish it now.
    was_updating: bool,
    /// When the person was last asked to restart, or the desktop or machine
    /// last restarted, as recorded at `asked`.
    asked: Option<PathBuf>,
    /// What the person asked of the update's notice, from the notifications
    /// service's taps, which run wherever it publishes.
    taken: Receiver<()>,
}

impl Finish {
    /// The clock this session finishes its updates on, recording what it
    /// asks where a restart of the desktop keeps it.
    pub(crate) fn for_session(options: &crate::Options) -> Self {
        Self::new(
            Instant::now(),
            asked_path(),
            options
                .desktop
                .as_ref()
                .map(|desktop| desktop.notifications()),
        )
    }

    /// A clock that first looks when the desk has settled, recording what it
    /// asks at `asked`, which survives a restart of the desktop; the
    /// person's answers to the update's notice come back through
    /// `notifications`, when this session tells the person through one.
    pub(crate) fn new(
        now: Instant,
        asked: Option<PathBuf>,
        notifications: Option<&Notifications>,
    ) -> Self {
        let (taken, answers) = mpsc::channel();
        if let Some(notifications) = notifications {
            notifications.on_taken(move |_id, taken_action| {
                if let Taken::Action { key, .. } = taken_action
                    && key == RESTART_ACTION
                {
                    let _ = taken.send(());
                }
            });
        }
        Self {
            next: now.checked_add(FIRST_LOOK_AFTER).unwrap_or(now),
            urgent: false,
            was_updating: false,
            asked,
            taken: answers,
        }
    }

    /// When the session next looks with no event in between, once it has
    /// settled.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        Some(self.next)
    }

    /// One look: what finishing the update asks now, if anything. `machine`
    /// says whether this device is an always-on host, and `wanted` that
    /// something asked for the newest system now.
    pub(crate) fn step(
        &mut self,
        at: Instant,
        clock: SystemTime,
        presence: Presence,
        update: &Update,
        host: bool,
        wanted: bool,
    ) -> Option<Due> {
        // The person pressed the notice's restart: they asked for it, and
        // nothing waits for a better moment.
        if self.taken.try_iter().next().is_some() {
            self.record(clock);
            return Some(Due::RestartMachine);
        }
        // An update that ended waiting for the desktop is finished at the next
        // moment they are away; one that needs a restart of the machine is
        // asked for at once, however lately it was asked.
        let was_updating = std::mem::replace(&mut self.was_updating, update.updating);
        if was_updating && !update.updating {
            if update.desktop_pending && !update.restart_needed {
                self.urgent = true;
            } else if update.restart_needed && !host {
                self.record(clock);
                return Some(Due::Ask);
            }
        }
        if wanted {
            return Some(Due::UpdateNow);
        }
        if at < self.next {
            return None;
        }
        self.next = at.checked_add(LOOK_EVERY).unwrap_or(at);
        // A restart of the machine covers what waits for the desktop, so two
        // of them never follow one another.
        if update::desktop_restart_due(
            update,
            self.last_asked(),
            clock,
            presence,
            self.urgent,
            host,
        ) {
            self.record(clock);
            self.urgent = false;
            return Some(Due::RestartDesktop);
        }
        if host && update::host_restart_due(update.restart_needed, Some(presence)) {
            self.record(clock);
            return Some(Due::RestartMachine);
        }
        if !host && update::ask_due(update.restart_needed, self.last_asked(), clock) {
            self.record(clock);
            return Some(Due::Ask);
        }
        None
    }

    /// When the person was last asked, or the desktop or machine last
    /// restarted.
    fn last_asked(&self) -> Option<SystemTime> {
        self.asked.as_deref().and_then(last_asked_at)
    }

    /// Record that the session asked, or restarted, now: the next one waits
    /// [`update::ASK_INTERVAL`].
    fn record(&mut self, clock: SystemTime) {
        if let Some(path) = &self.asked {
            record_asked_at(path, clock);
        }
    }
}

/// When the person was last asked, or the desktop or machine last restarted,
/// as recorded at `path`.
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

/// Where the session records when it last asked for a restart, or restarted
/// the desktop or the machine.
#[must_use]
pub(crate) fn asked_path() -> Option<PathBuf> {
    tilo_paths::state_dir().map(|root| root.join("tilo/update-restart-asked"))
}

/// Whether this machine is an always-on host: a board or a host image with
/// nobody at it to ask.
fn host() -> bool {
    static HOST: OnceLock<bool> = OnceLock::new();
    *HOST.get_or_init(|| tilo_power_mgmt::this_machine_duty() == tilo_power_mgmt::MachineDuty::Host)
}

impl Session {
    /// Finish the staged update on the session's own clock: fetch it now when
    /// something needs it, restart the desktop onto it once nobody has been
    /// at the machine for a while, restart an always-on host into it, and ask
    /// the person for a restart of the machine. Called once per pass of the
    /// loop.
    pub(super) fn follow_update(&mut self) {
        let wanted = self
            .shared
            .sync
            .as_ref()
            .is_some_and(|sync| sync.take_update_request());
        let now = Instant::now();
        let presence = Presence {
            idle: self.compositor.input_idle(),
            away: self.compositor.locked() || self.compositor.blanked(),
        };
        // What the services last published: the update service keeps it
        // current, and it is absent where this session hosts none.
        let update = self
            .read(tilo_system_service::SYSTEM_UPDATE)
            .and_then(|reading| serde_json::from_value::<Update>(reading).ok())
            .unwrap_or_default();
        let Some(due) =
            self.update_finish
                .step(now, SystemTime::now(), presence, &update, host(), wanted)
        else {
            return;
        };
        match due {
            Due::Ask => self
                .shared
                .notifications
                .borrow()
                .tell(crate::notices::update_notice()),
            Due::UpdateNow => self.finish_update(update::UPDATE_NOW),
            Due::RestartDesktop => self.finish_update(update::RESTART_DESKTOP),
            Due::RestartMachine => self.finish_update(update::RESTART_TO_UPDATE),
        }
    }

    /// Invoke one of the update resource's actions, as an app or the person
    /// would: the service's worker makes the change, so the loop keeps
    /// drawing until the desktop restarts. The session is the person's own
    /// here, so what waits for their confirmation is confirmed in their name.
    fn finish_update(&self, action: &str) {
        let mut services = self.shared.services.borrow_mut();
        match services.invoke_confirmed(
            tilo_system_service::SYSTEM_UPDATE,
            action,
            &serde_json::Map::new(),
        ) {
            Some(Ok(_)) => tracing::info!(action, "finishing the system update"),
            Some(Err(error)) => tracing::warn!(%error, action, "the system update"),
            None => tracing::warn!(action, "no system update service answered"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn a_finish(now: Instant, asked: Option<PathBuf>) -> Finish {
        Finish::new(now, asked, None)
    }

    fn away() -> Presence {
        Presence {
            idle: Duration::from_secs(10 * 60),
            away: true,
        }
    }

    fn working() -> Presence {
        Presence {
            idle: Duration::from_secs(1),
            away: false,
        }
    }

    fn pending_desktop() -> Update {
        Update {
            staged: Some("sha256:aa".into()),
            desktop_pending: true,
            ..Update::default()
        }
    }

    #[test]
    fn the_desktop_restarts_onto_the_update_once_the_person_is_away() {
        let now = Instant::now();
        let dir = tempdir();
        let path = dir.path().join("tilo/update-restart-asked");
        let clock = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let mut finish = a_finish(now, Some(path.clone()));

        assert_eq!(
            finish.step(now, clock, away(), &pending_desktop(), false, false),
            None,
            "the desk settles first"
        );
        let look = now + FIRST_LOOK_AFTER;
        assert_eq!(
            finish.step(look, clock, working(), &pending_desktop(), false, false),
            None,
            "never while the person works"
        );
        assert_eq!(
            finish.step(
                look + LOOK_EVERY,
                clock,
                away(),
                &pending_desktop(),
                false,
                false
            ),
            Some(Due::RestartDesktop)
        );
        assert!(
            path.exists(),
            "when it restarted is recorded, so the session that comes back knows"
        );
        assert_eq!(
            finish.step(
                look + 2 * LOOK_EVERY,
                clock,
                away(),
                &pending_desktop(),
                false,
                false
            ),
            None,
            "one restart every two days at most"
        );
    }

    #[test]
    fn an_update_something_needed_now_is_finished_at_the_next_chance() {
        let now = Instant::now();
        let clock = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let mut finish = a_finish(now, None);
        // The person's app or a collaborator's device needs a newer system,
        // so this session fetches it: the reading says it runs, and then
        // that it ended waiting for the desktop.
        assert_eq!(
            finish.step(
                now,
                clock,
                working(),
                &Update {
                    updating: true,
                    ..Update::default()
                },
                false,
                true
            ),
            Some(Due::UpdateNow)
        );
        assert_eq!(
            finish.step(
                now,
                clock,
                working(),
                &Update {
                    updating: true,
                    ..Update::default()
                },
                false,
                false
            ),
            None
        );
        // It ended, and the desktop is what waits: the next moment the person
        // is away finishes it, whatever the last restart.
        assert_eq!(
            finish.step(now, clock, working(), &pending_desktop(), false, false),
            None
        );
        assert_eq!(
            finish.step(now, clock, away(), &pending_desktop(), false, false),
            None,
            "the desk still settles"
        );
        let look = now + FIRST_LOOK_AFTER;
        assert_eq!(
            finish.step(
                look + LOOK_EVERY,
                clock,
                away(),
                &pending_desktop(),
                false,
                false
            ),
            Some(Due::RestartDesktop)
        );
    }

    #[test]
    fn an_update_that_needs_a_restart_of_the_machine_asks_as_soon_as_it_ends() {
        let now = Instant::now();
        let dir = tempdir();
        let path = dir.path().join("tilo/update-restart-asked");
        let clock = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let mut finish = a_finish(now, Some(path));
        let running = Update {
            updating: true,
            ..Update::default()
        };
        assert_eq!(
            finish.step(now, clock, working(), &running, false, true),
            Some(Due::UpdateNow)
        );
        // It ended with part of it waiting for the machine: the person hears
        // so at once, not at the next two-day turn of the clock.
        let kernel = Update {
            staged: Some("sha256:aa".into()),
            restart_needed: true,
            ..Update::default()
        };
        assert_eq!(
            finish.step(now, clock, working(), &kernel, false, false),
            Some(Due::Ask)
        );
        assert_eq!(
            finish.step(now, clock, working(), &kernel, false, false),
            None,
            "and then at the usual pace"
        );
    }

    #[test]
    fn a_staged_update_asks_for_a_restart_of_the_machine_and_a_host_restarts_itself() {
        let now = Instant::now();
        let dir = tempdir();
        let path = dir.path().join("tilo/update-restart-asked");
        let clock = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let staged = Update {
            staged: Some("sha256:aa".into()),
            restart_needed: true,
            ..Update::default()
        };
        let mut finish = a_finish(now, Some(path.clone()));
        let look = now + FIRST_LOOK_AFTER;
        assert_eq!(
            finish.step(look, clock, away(), &staged, false, false),
            Some(Due::Ask),
            "the person is told, once the desk has settled"
        );
        assert_eq!(
            finish.step(
                look + LOOK_EVERY,
                clock + LOOK_EVERY,
                away(),
                &staged,
                false,
                false
            ),
            None,
            "one request every two days"
        );
        // An always-on host has nobody to ask: it restarts into the update
        // itself once nobody is at it.
        let mut always_on = a_finish(now, None);
        assert_eq!(
            always_on.step(look, clock, working(), &staged, true, false),
            None,
            "never while someone works at it"
        );
        assert_eq!(
            always_on.step(look + LOOK_EVERY, clock, away(), &staged, true, false),
            Some(Due::RestartMachine)
        );
    }

    #[test]
    fn the_notice_s_restart_action_reaches_the_session_wherever_it_publishes() {
        let now = Instant::now();
        let clock = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let notifications = Notifications::new();
        let mut finish = Finish::new(now, None, Some(&notifications));
        let id = notifications.alloc_id();
        notifications.push(tilo_notifications_service::Notification {
            actions: vec![(RESTART_ACTION.to_owned(), "Restart".to_owned())],
            ..tilo_notifications_service::Notification::new(id)
        });
        notifications.action_invoked(id, RESTART_ACTION, None);
        assert_eq!(
            finish.step(now, clock, working(), &Update::default(), false, false),
            Some(Due::RestartMachine),
            "the person pressed it: nothing waits for a better moment"
        );
    }

    #[test]
    fn the_last_restart_survives_a_restart_of_the_session() {
        let dir = tempdir();
        let path = dir.path().join("tilo/update-restart-asked");
        assert_eq!(last_asked_at(&path), None);
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        record_asked_at(&path, at);
        assert_eq!(last_asked_at(&path), Some(at));
    }
}
