//! Finishing the staged system update: applied live, with the desktop restarted onto the new
//! interface once the person has stepped away, and a restart of the machine asked for at most every
//! two days. What waits for the desktop (the compositor, the session services, the interface
//! versions apps speak) is finished here through the `system-update` resource's own actions
//! ([`tilo_system_service::update::RESTART_DESKTOP`]), invoked exactly as an app or the person
//! would; the login stays and the session comes back with the windows it had open. Nothing is
//! finished while someone is at the machine: they are away when the screen is locked or blanked and
//! no input has come since. A desktop restart and a machine restart share one clock, so those days'
//! updates apply together; an always-on host has nobody to ask and finishes whenever it is away.
//! How an update ended is told here too, once each: a deployment the machine went back from
//! ([`Finish::tell_went_back`]) and a live update that did not finish ([`Finish::tell_live_failed`]).

use std::sync::mpsc::Receiver;
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant, SystemTime};

use tilo_notifications_service::{Notifications, Taken};
use tilo_system_service::update::{self, Fallback, Presence, Update};

use crate::Session;
use tilo_session_client::device_records::{DeviceRecord, DeviceRecords};

/// The device record that holds when a restart for an update was last asked for, or done.
const ASKED: &str = "so.tilo.session-update-asked";
/// The device record that holds how updates ended that the person was told about: the deployment
/// fallbacks and the live updates that did not finish.
const TOLD: &str = "so.tilo.session-fallbacks-told";
use crate::notices::RESTART_ACTION;

/// When the session first looks for an update to finish: once the shell has settled.
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

/// The clock on finishing the update, and what the person asked of its notice.
pub(crate) struct Finish {
    /// When it next looks.
    next: Instant,
    /// Something needed the update now, and what applied live waits for the desktop: the next
    /// moment the person is away finishes it.
    urgent: bool,
    /// Whether this session saw an update run, so the reading that says it ended is the one that
    /// says whether to finish it now.
    was_updating: bool,
    /// When the person was last asked to restart, or the desktop or machine last restarted, as
    /// recorded in `asked` .
    asked: Option<DeviceRecord>,
    /// Where what the person was told of how updates ended is recorded: the ids of the deployment
    /// fallbacks, and `live:` and the digest of each live update that did not finish. `None` where
    /// there is nowhere to keep them.
    heard: Option<DeviceRecord>,
    /// What the person was told of, as the record names it; `None` until the record in `heard`
    /// is read.
    told: Option<Vec<String>>,
    /// What the person asked of the update's notice, from the notifications service's taps, which
    /// run wherever it publishes.
    taken: Receiver<()>,
    /// Whether the person was already told that the published update state does not read back: it
    /// is said once, not on every look.
    unreadable_told: bool,
}

impl Finish {
    /// The clock this session finishes its updates on, recording what it asks, and which fallbacks
    /// the person heard of, where a restart of the desktop keeps them.
    pub(crate) fn for_session(
        records: Option<&DeviceRecords>,
        desktop: Option<&crate::desktop_services::DesktopServices>,
    ) -> Self {
        Self::new(
            Instant::now(),
            records.map(|records| followed(records, ASKED)),
            records.map(|records| followed(records, TOLD)),
            desktop.map(crate::desktop_services::DesktopServices::notifications),
        )
    }

    /// A clock that first looks when the shell has settled, recording what it asks in `asked` ,
    /// which survives a restart of the desktop; the person's answers to the update's notice come
    /// back through `notifications` , when this session tells the person through one, and which
    /// fallbacks they already heard of is read from and written to `heard` .
    pub(crate) fn new(
        now: Instant,
        asked: Option<DeviceRecord>,
        heard: Option<DeviceRecord>,
        notifications: Option<&Notifications>,
    ) -> Self {
        let (taken, answers) = mpsc::channel();
        if let Some(notifications) = notifications {
            notifications.on_taken(move |_id, taken_action| {
                if let Taken::Action { key, .. } = taken_action
                    && key == RESTART_ACTION
                {
                    // The reader is gone once the session that owns it has shut down.
                    let _ = taken.send(());
                }
            });
        }
        Self {
            next: now.checked_add(FIRST_LOOK_AFTER).unwrap_or(now),
            urgent: false,
            was_updating: false,
            asked,
            heard,
            told: None,
            taken: answers,
            unreadable_told: false,
        }
    }

    /// Tell the person, once, that the published update state does not read back: `true` the first
    /// time a look meets it, `false` after.
    pub(crate) fn note_unreadable(&mut self) -> bool {
        !std::mem::replace(&mut self.unreadable_told, true)
    }

    /// When the session next looks with no event in between, once it has settled.
    pub(crate) fn next_deadline(&self) -> Instant {
        self.next
    }

    /// One look: what finishing the update asks now, if anything. `machine` says whether this
    /// device is an always-on host, and `wanted` that something asked for the newest system now.
    pub(crate) fn step(
        &mut self,
        at: Instant,
        clock: SystemTime,
        presence: Presence,
        update: &Update,
        host: bool,
        wanted: bool,
    ) -> Option<Due> {
        // The person pressed the notice's restart: they asked for it, and nothing waits for a
        // better moment.
        if self.taken.try_iter().next().is_some() {
            self.record(clock);
            return Some(Due::RestartMachine);
        }
        // An update that ended waiting for the desktop is finished at the next moment they are
        // away; one that needs a restart of the machine is asked for at once, however lately it was
        // asked.
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
        // A restart of the machine covers what waits for the desktop, so two of them never follow
        // one another.
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

    /// Tell the person, once, of each deployment this machine went back from that they have not
    /// heard of yet: a report the boot-time settling left
    /// ([`tilo_system_service::update::Fallback`]) reaches the session in the update's reading, and
    /// the ids it told are recorded beside the restart record, so a later session says nothing of
    /// the same fallback. Called with every reading, so it is cheap when there is nothing to say
    /// and says the fallback of this boot as soon as the reading carries it.
    pub(crate) fn tell_went_back(
        &mut self,
        fell_back: &[Fallback],
        mut tell: impl FnMut(&crate::platform::notice::Notice),
    ) {
        for fallback in fell_back {
            self.tell_once(
                &fallback.id,
                || crate::notices::went_back_notice(fallback),
                &mut tell,
            );
        }
    }

    /// Tell the person, once per update, that the staged update `update` could not be applied
    /// while the computer ran and was taken back, offering the restart that completes it. The
    /// desktop that comes back after a failed desktop half reads the failure at its start, and a
    /// failed system half reaches the reading as the live update writes it; either way it is said
    /// once, and recorded beside the fallbacks so a later session says nothing of it.
    pub(crate) fn tell_live_failed(
        &mut self,
        update: &Update,
        mut tell: impl FnMut(&crate::platform::notice::Notice),
    ) {
        if update.failed.is_none() {
            return;
        }
        let digest = update
            .staged
            .as_ref()
            .and_then(|staged| staged.digest.as_deref())
            .unwrap_or_default();
        self.tell_once(
            &format!("live:{digest}"),
            crate::notices::live_failed_notice,
            &mut tell,
        );
    }

    /// Tell the notice `notice` makes unless what `id` names was told already, and record it.
    fn tell_once(
        &mut self,
        id: &str,
        notice: impl FnOnce() -> crate::platform::notice::Notice,
        tell: &mut impl FnMut(&crate::platform::notice::Notice),
    ) {
        // Nothing is told that could not be recorded: it would be said at every sign-in instead of
        // once.
        let Some(heard) = &self.heard else {
            return;
        };
        let told = self.told.get_or_insert_with(|| heard_ids(heard));
        if told.iter().any(|told| told == id) {
            return;
        }
        tell(&notice());
        told.push(id.to_owned());
        record_heard(heard, told);
    }

    /// When the person was last asked, or the desktop or machine last restarted.
    fn last_asked(&self) -> Option<SystemTime> {
        self.asked.as_ref().and_then(last_asked_at)
    }

    /// Record that the session asked, or restarted, now: the next one waits
    /// [`update::ASK_INTERVAL`].
    fn record(&self, clock: SystemTime) {
        if let Some(asked) = &self.asked {
            record_asked_at(asked, clock);
        }
    }
}

/// When the person was last asked, or the desktop or machine last restarted, as recorded in
/// `asked` .
/// The record `name`, followed: when it last asked and what the person heard of are read as the
/// person's other tools last wrote them (an update Settings finished, a fallback heard of there),
/// never asked of the daemon on the clock's own turn.
fn followed(records: &DeviceRecords, name: &str) -> DeviceRecord {
    let record = records.record(name);
    record
        .follow(|_| {})
        .inspect_err(|error| tracing::warn!(%error, record = name, "an update's record is not followed"))
        .unwrap_or(record)
}

fn last_asked_at(asked: &DeviceRecord) -> Option<SystemTime> {
    let record = match asked.read() {
        Ok(record) => record?,
        Err(error) => {
            tracing::warn!(%error, "when a restart was last asked for does not read back");
            return None;
        }
    };
    let seconds = record.as_u64()?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

fn record_asked_at(asked: &DeviceRecord, now: SystemTime) {
    let seconds = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    asked.set(serde_json::Value::from(seconds));
}

/// What the person was told of, as recorded in `heard` : the ids.
fn heard_ids(heard: &DeviceRecord) -> Vec<String> {
    match heard.read() {
        Ok(record) => record
            .and_then(|record| serde_json::from_value(record).ok())
            .unwrap_or_default(),
        Err(error) => {
            tracing::warn!(%error, "the fallbacks the person was told about do not read back");
            Vec::new()
        }
    }
}

/// Record what the person was told of in `heard` .
fn record_heard(heard: &DeviceRecord, ids: &[String]) {
    heard.set(serde_json::Value::from(ids.to_vec()));
}

/// Whether this machine is an always-on host: a board or a host image with nobody at it to ask.
fn host() -> bool {
    static HOST: OnceLock<bool> = OnceLock::new();
    *HOST.get_or_init(|| tilo_power_mgmt::this_machine_duty() == tilo_power_mgmt::MachineDuty::Host)
}

impl Session {
    /// Finish the staged update on the session's own clock: fetch it now when something needs it,
    /// restart the desktop onto it once nobody has been at the machine for a while, restart an
    /// always-on host into it, and ask the person for a restart of the machine. Called once per
    /// pass of the loop.
    pub(super) fn follow_update(&mut self) {
        // A member's device at another protocol version is the person's to know: this device
        // updates when theirs is the newer one, and otherwise theirs is the one that must.
        let mismatched = self
            .shared
            .sync
            .as_ref()
            .map(|sync| sync.take_version_mismatches())
            .unwrap_or_default();
        let mut wanted = false;
        for mismatch in mismatched {
            wanted |= mismatch.theirs_is_newer();
            let device = crate::network::device_at(self.shared.fabric.as_ref(), &mismatch.endpoint);
            self.shared
                .notifications
                .borrow()
                .tell(&crate::notices::version_mismatch_notice(
                    &mismatch,
                    device.as_deref(),
                ));
        }
        let now = Instant::now();
        let presence = Presence {
            idle: self.compositor.input_idle(),
            away: self.compositor.locked() || self.compositor.blanked(),
        };
        // What the services last published: the update service keeps it current, and it is absent
        // where this session hosts none.
        let update = match self
            .read(tilo_system_service::SYSTEM_UPDATE)
            .map(serde_json::from_value::<Update>)
        {
            Some(Ok(update)) => update,
            Some(Err(error)) => {
                tracing::warn!(
                    %error,
                    "the published update state does not read back; this pass reads it as no update"
                );
                // A state the person can see nothing of is not acted on, so it is theirs to know
                // about: told once, and the warning above stays for operators.
                if self.update_finish.note_unreadable() {
                    self.shared
                        .notifications
                        .borrow()
                        .tell(&crate::notices::update_state_notice());
                }
                Update::default()
            }
            None => Update::default(),
        };
        // How the update this computer tried ended is the person's to know before anything else
        // waits: a deployment that did not start was settled before this session ran, and its
        // report is in the reading.
        let shared = &self.shared;
        self.update_finish
            .tell_went_back(&update.fell_back, |notice| {
                shared.notifications.borrow().tell(notice);
            });
        self.update_finish.tell_live_failed(&update, |notice| {
            shared.notifications.borrow().tell(notice);
        });
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
                .tell(&crate::notices::update_notice()),
            Due::UpdateNow => self.finish_update(update::UPDATE_NOW),
            Due::RestartDesktop => self.finish_update(update::RESTART_DESKTOP),
            Due::RestartMachine => self.finish_update(update::RESTART_TO_UPDATE),
        }
    }

    /// Invoke one of the update resource's actions, as an app or the person would: the service's
    /// worker makes the change, so the loop keeps drawing until the desktop restarts. The session
    /// is the person's own here, so what waits for their confirmation is confirmed in their name.
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

    fn a_finish(now: Instant, asked: Option<DeviceRecord>) -> Finish {
        Finish::new(now, asked, None, None)
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
            staged: Some(update::Deployment {
                digest: Some("sha256:aa".into()),
                ..update::Deployment::default()
            }),
            desktop_pending: true,
            ..Update::default()
        }
    }

    tilo_test_env::scenario! {
        fn what_went_wrong_with_an_update_is_told_once;

        phase an_unreadable_update_state_is_told_once;
        // An update state that does not read back is nothing the clock acts on,
        // so it is the person's to know: once, not on every look.
        {
            let mut finish = a_finish(Instant::now(), None);
            assert!(finish.note_unreadable(), "the person is told");
            assert!(!finish.note_unreadable(), "and only once");
        }

        phase a_deployment_the_machine_went_back_from_is_told_once_and_recorded_once;
        // A deployment the machine went back from is what the person hears after the reboot: the
        // session tells them once, records that it did, and a session that starts later says nothing
        // of it.
        {
            let records = DeviceRecords::held();
            let heard = || records.record(TOLD);
            let notifications = Notifications::new();
            let id = "20261025T101010Z-0a1b2c3d4e5f6a7b";
            let fallback = update::Fallback {
                id: id.into(),
                image: Some("registry.tilo.so/tilo/tilo-vm:main-1a2b3c4".into()),
                version: Some("44.20261023.0".into()),
                tries: Some(3),
                running: Some("44.20261022.0".into()),
                summary: "/var/lib/tilo/boot-fallback/20261025T101010Z-0a1b2c3d4e5f6a7b/summary.txt"
                    .into(),
            };
            let mut finish = Finish::new(Instant::now(), None, Some(heard()), None);
            assert!(
                notifications.live().is_empty(),
                "nothing fell back before the reading says so"
            );
            finish.tell_went_back(std::slice::from_ref(&fallback), |notice| {
                crate::notifications::tell_daemon(notice, &notifications);
            });

            let live = notifications.live();
            assert_eq!(live.len(), 1, "one fallback, one notice");
            let notice = &live[0];
            assert_eq!(notice.summary, "Went back to the version before");
            assert_eq!(notice.app_name, "Updates");
            assert_eq!(notice.urgency, 2, "an update that failed is not a shrug");
            assert_eq!(
                notice.expire_timeout, -1,
                "it stays until the person dismisses it"
            );
            assert!(notice.machine_wide, "the whole machine went back");
            assert!(notice.body.contains("44.20261023.0"), "{}", notice.body);
            assert!(notice.body.contains("after 3 tries"), "{}", notice.body);
            assert!(
                notice.body.contains("back to 44.20261022.0"),
                "{}",
                notice.body
            );
            assert!(notice.body.contains("summary.txt"), "{}", notice.body);
            assert_eq!(
                heard_ids(&heard()),
                [id.to_owned()],
                "told once is recorded once"
            );

            // The reading keeps carrying the report of every earlier fallback: this session says
            // nothing more of it.
            finish.tell_went_back(std::slice::from_ref(&fallback), |notice| {
                crate::notifications::tell_daemon(notice, &notifications);
            });
            assert_eq!(notifications.live().len(), 1, "said once per session");

            // A later session reads the record and says nothing at all: the person hears of one
            // fallback once, however many sign-ins follow.
            let mut later = Finish::new(Instant::now(), None, Some(heard()), None);
            later.tell_went_back(std::slice::from_ref(&fallback), |notice| {
                crate::notifications::tell_daemon(notice, &notifications);
            });
            assert_eq!(notifications.live().len(), 1, "said once per person");
            assert_eq!(heard_ids(&heard()), [id.to_owned()], "and recorded once");
        }

        phase a_later_fallback_is_told_and_nothing_is_told_that_cannot_be_recorded;
        // A second deployment that did not start is news of its own, and a machine with nowhere to
        // record what was told says nothing rather than saying it at every sign-in.
        {
            let records = DeviceRecords::held();
            let heard = || records.record(TOLD);
            let notifications = Notifications::new();
            let fallback = |id: &str| update::Fallback {
                id: id.into(),
                summary: format!("/var/lib/tilo/boot-fallback/{id}/summary.txt"),
                ..update::Fallback::default()
            };
            let mut finish = Finish::new(Instant::now(), None, Some(heard()), None);
            finish.tell_went_back(&[fallback("20261025T101010Z-aaaa")], |notice| {
                crate::notifications::tell_daemon(notice, &notifications);
            });
            finish.tell_went_back(
                &[
                    fallback("20261025T101010Z-aaaa"),
                    fallback("20261102T090000Z-bbbb"),
                ],
                |notice| {
                    crate::notifications::tell_daemon(notice, &notifications);
                },
            );
            assert_eq!(
                notifications.live().len(),
                2,
                "each fallback is its own news"
            );
            assert_eq!(
                heard_ids(&heard()),
                ["20261025T101010Z-aaaa", "20261102T090000Z-bbbb"]
            );

            let mut nowhere = Finish::new(Instant::now(), None, None, None);
            nowhere.tell_went_back(&[fallback("20261103T090000Z-cccc")], |notice| {
                crate::notifications::tell_daemon(notice, &notifications);
            });
            assert_eq!(notifications.live().len(), 2, "nothing repeats");
        }

        phase a_live_update_that_did_not_finish_is_told_once_with_the_restart_that_completes_it;
        // The desktop half failed and was taken back: the desktop that comes back reads the
        // failure and the person hears it once, offered the restart; a later session, and the
        // same failure read again, say nothing.
        {
            let records = DeviceRecords::held();
            let heard = || records.record(TOLD);
            let notifications = Notifications::new();
            let staged = |digest: &str| update::Deployment {
                digest: Some(digest.into()),
                ..update::Deployment::default()
            };
            let failed = |digest: &str| Update {
                staged: Some(staged(digest)),
                restart_needed: true,
                failed: Some("the desktop did not come back".into()),
                ..Update::default()
            };
            let tell = |finish: &mut Finish, update: &Update| {
                finish.tell_live_failed(update, |notice| {
                    crate::notifications::tell_daemon(notice, &notifications);
                });
            };
            let mut finish = Finish::new(Instant::now(), None, Some(heard()), None);
            tell(&mut finish, &pending_desktop());
            assert!(notifications.live().is_empty(), "an update that waits failed nowhere");

            tell(&mut finish, &failed("sha256:new"));
            let live = notifications.live();
            assert_eq!(live.len(), 1);
            let notice = &live[0];
            assert_eq!(notice.summary, "The update needs a restart");
            assert!(
                notice.body.contains("Restart the computer to complete it"),
                "{}",
                notice.body
            );
            assert!(
                notice.actions.iter().any(|(key, _)| key == RESTART_ACTION),
                "the notice offers the restart"
            );

            tell(&mut finish, &failed("sha256:new"));
            // The session ends; the next one holds the record.
            drop(finish);
            let mut later = Finish::new(Instant::now(), None, Some(heard()), None);
            tell(&mut later, &failed("sha256:new"));
            assert_eq!(notifications.live().len(), 1, "one failure, one notice");

            tell(&mut later, &failed("sha256:newer"));
            assert_eq!(notifications.live().len(), 2, "another update's failure is news");
            assert_eq!(heard_ids(&heard()), ["live:sha256:new", "live:sha256:newer"]);
        }
    }

    tilo_test_env::scenario! {
        fn a_staged_update_is_finished_when_the_person_or_the_machine_allows;

        phase the_desktop_restarts_onto_the_update_once_the_person_is_away;
        {
            let now = Instant::now();
            let records = DeviceRecords::held();
            let asked = || records.record(ASKED);
            let clock = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
            let mut finish = a_finish(now, Some(asked()));

            assert_eq!(
                finish.step(now, clock, away(), &pending_desktop(), false, false),
                None,
                "the shell settles first"
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
            assert_eq!(
                last_asked_at(&asked()),
                Some(clock),
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

        phase an_update_something_needed_now_is_finished_at_the_next_chance;
        {
            let now = Instant::now();
            let clock = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
            let mut finish = a_finish(now, None);
            // The person's app or a member's device needs a newer system,
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
            // It ended, and the desktop is what waits: the next moment the person is away finishes it,
            // whatever the last restart.
            assert_eq!(
                finish.step(now, clock, working(), &pending_desktop(), false, false),
                None
            );
            assert_eq!(
                finish.step(now, clock, away(), &pending_desktop(), false, false),
                None,
                "the shell still settles"
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

        phase an_update_that_needs_a_restart_of_the_machine_asks_as_soon_as_it_ends;
        {
            let now = Instant::now();
            let records = DeviceRecords::held();
            let asked = || records.record(ASKED);
            let clock = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
            let mut finish = a_finish(now, Some(asked()));
            let running = Update {
                updating: true,
                ..Update::default()
            };
            assert_eq!(
                finish.step(now, clock, working(), &running, false, true),
                Some(Due::UpdateNow)
            );
            // It ended with part of it waiting for the machine: the person hears so at once, not at the
            // next two-day turn of the clock.
            let kernel = Update {
                staged: Some(update::Deployment {
                digest: Some("sha256:aa".into()),
                ..update::Deployment::default()
            }),
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

        phase a_staged_update_asks_for_a_restart_of_the_machine_and_a_host_restarts_itself;
        {
            let now = Instant::now();
            let records = DeviceRecords::held();
            let asked = || records.record(ASKED);
            let clock = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
            let staged = Update {
                staged: Some(update::Deployment {
                digest: Some("sha256:aa".into()),
                ..update::Deployment::default()
            }),
                restart_needed: true,
                ..Update::default()
            };
            let mut finish = a_finish(now, Some(asked()));
            let look = now + FIRST_LOOK_AFTER;
            assert_eq!(
                finish.step(look, clock, away(), &staged, false, false),
                Some(Due::Ask),
                "the person is told, once the shell has settled"
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
            // An always-on host has nobody to ask: it restarts into the update itself once nobody is at
            // it.
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

        phase the_notice_s_restart_action_reaches_the_session_wherever_it_publishes;
        {
            let now = Instant::now();
            let clock = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
            let notifications = Notifications::new();
            let mut finish = Finish::new(now, None, None, Some(&notifications));
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

        phase the_last_restart_survives_a_restart_of_the_session;
        {
            let records = DeviceRecords::held();
            let asked = records.record(ASKED);
            assert_eq!(last_asked_at(&asked), None);
            let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
            record_asked_at(&asked, at);
            drop(asked);
            let next = records.record(ASKED);
            assert_eq!(last_asked_at(&next), Some(at));
        }
    }
}
