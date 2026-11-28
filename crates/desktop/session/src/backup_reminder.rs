//! The reminder of an identity backup the person left for later. Setup offers the backup and
//! never forces it; once the person chose Later, a system notice offers it again every few days,
//! until a backup is verified or they turn the reminders off from the notice. Devices keeps
//! offering the backup whatever the reminder does. What the person chose is kept in their
//! settings (`identity.backed_up`, `identity.backup_reminders`, `identity.backup_reminded`), so
//! the reminder follows them across their devices and restarts.

use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Map, Value};
use settings_store::identity::{BACKED_UP, BACKUP_REMINDED, BACKUP_REMINDERS};
use tilo_notifications_service::{Notifications, Taken};

use crate::Session;

/// The notice's action that raises the backup.
pub(crate) const BACK_UP_ACTION: &str = "identity-backup-now";
/// The notice's action that turns the reminders off.
pub(crate) const STOP_ACTION: &str = "identity-backup-reminders-off";

/// How long after the last reminder the next one comes.
const EVERY: Duration = Duration::from_secs(3 * 24 * 60 * 60);
/// When the session first looks: once the desktop has settled, not over the person's first
/// minutes on it.
const FIRST_LOOK_AFTER: Duration = Duration::from_secs(10 * 60);
/// How often it looks again: one read of the person's settings.
const LOOK_EVERY: Duration = Duration::from_secs(60 * 60);

/// What the reminder asks of the session now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Due {
    /// Tell the person their identity is not backed up yet.
    Remind,
    /// The person pressed the notice's back up: raise the backup.
    BackUp,
    /// The person turned the reminders off from the notice.
    Stop,
}

/// The reminder's clock, and the answers to its notice.
pub(crate) struct Reminder {
    next: Instant,
    taken: Receiver<Due>,
}

impl Reminder {
    /// A reminder that first looks once the desktop has settled; the person's answers to its
    /// notice come back through `notifications`, where the session tells them through one.
    pub(crate) fn new(now: Instant, notifications: Option<&Notifications>) -> Self {
        let (answer, taken) = mpsc::channel();
        if let Some(notifications) = notifications {
            notifications.on_taken(move |_id, taken| {
                let Taken::Action { key, .. } = taken else {
                    return;
                };
                let due = match key {
                    BACK_UP_ACTION => Due::BackUp,
                    STOP_ACTION => Due::Stop,
                    _ => return,
                };
                // The reader is gone once the session that owns it has shut down.
                let _ = answer.send(due);
            });
        }
        Self {
            next: now.checked_add(FIRST_LOOK_AFTER).unwrap_or(now),
            taken,
        }
    }

    /// The reminder of this session, told through its desktop's notifications.
    pub(crate) fn for_session(desktop: Option<&crate::desktop_services::DesktopServices>) -> Self {
        Self::new(
            Instant::now(),
            desktop.map(crate::desktop_services::DesktopServices::notifications),
        )
    }

    /// When the session next looks with no event in between.
    pub(crate) fn next_deadline(&self) -> Instant {
        self.next
    }

    /// One look at `at`, the wall clock reading `clock`, with the person's identity settings
    /// `identity`: what is due, if anything. An answer to the notice comes first.
    pub(crate) fn step(
        &mut self,
        at: Instant,
        clock: SystemTime,
        identity: impl FnOnce() -> Map<String, Value>,
    ) -> Option<Due> {
        if let Some(answer) = self.taken.try_iter().next() {
            return Some(answer);
        }
        if at < self.next {
            return None;
        }
        self.next = at.checked_add(LOOK_EVERY).unwrap_or(at);
        due(&identity(), clock).then_some(Due::Remind)
    }
}

/// Whether the person's identity settings `identity` call for a reminder at `clock`: they left
/// the backup for later, kept the reminders on, and were last reminded [`EVERY`] ago or never.
fn due(identity: &Map<String, Value>, clock: SystemTime) -> bool {
    let now = seconds(clock);
    BACKED_UP.from_section(identity) == Some(false)
        && BACKUP_REMINDERS.from_section(identity)
        && BACKUP_REMINDED
            .from_section(identity)
            .is_none_or(|last| now.saturating_sub(last) >= EVERY.as_secs())
}

fn seconds(clock: SystemTime) -> u64 {
    clock
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

impl Session {
    /// Remind the person of the backup they left for later, on the reminder's own clock, and
    /// carry out what they answered its notice. Called once per pass of the loop.
    pub(super) fn follow_backup_reminder(&mut self) {
        let clock = SystemTime::now();
        let Some(due) = self.backup_reminder.step(Instant::now(), clock, || {
            settings_store::load_section(settings_store::identity::SECTION)
        }) else {
            return;
        };
        match due {
            Due::Remind => {
                if let Err(error) = BACKUP_REMINDED.set(Some(seconds(clock))) {
                    // Unrecorded, it would remind at every look: said nothing until it records.
                    tracing::warn!(%error, "the backup reminder is not recorded; not reminding");
                    return;
                }
                self.shared
                    .notifications
                    .borrow()
                    .tell(&crate::notices::backup_notice());
            }
            Due::BackUp => {
                if let Err(why) = self.back_up_identity() {
                    self.shared
                        .notifications
                        .borrow()
                        .tell(&crate::notices::backup_failed_notice(&why));
                }
            }
            Due::Stop => {
                if let Err(error) = BACKUP_REMINDERS.set(false) {
                    tracing::warn!(%error, "the backup reminders could not be turned off");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    #[test]
    fn a_backup_left_for_later_is_offered_again_every_few_days_until_it_is_made_or_turned_off() {
        let start = Instant::now();
        let clock = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let notifications = Notifications::new();
        let mut reminder = Reminder::new(start, Some(&notifications));
        let later = identity(&[("backed_up", Value::Bool(false))]);

        // Nothing over the person's first minutes on the desktop.
        assert_eq!(reminder.step(start, clock, || later.clone()), None);
        let look = start + FIRST_LOOK_AFTER;
        assert_eq!(
            reminder.step(look, clock, || later.clone()),
            Some(Due::Remind),
            "left for later and never reminded: reminded once the desktop settled"
        );
        let was_reminded = identity(&[
            ("backed_up", Value::Bool(false)),
            ("backup_reminded", Value::from(seconds(clock))),
        ]);
        let next = look + LOOK_EVERY;
        assert_eq!(
            reminder.step(next, clock + LOOK_EVERY, || was_reminded.clone()),
            None,
            "not again within the interval"
        );
        let days_on = next + LOOK_EVERY;
        assert_eq!(
            reminder.step(days_on, clock + EVERY, || was_reminded.clone()),
            Some(Due::Remind),
            "again once the interval has passed"
        );

        // A verified backup, a person who turned the reminders off, and one setup never asked
        // are not reminded.
        let mut at = days_on;
        for settled in [
            identity(&[("backed_up", Value::Bool(true))]),
            identity(&[
                ("backed_up", Value::Bool(false)),
                ("backup_reminders", Value::Bool(false)),
            ]),
            Map::new(),
        ] {
            at += LOOK_EVERY;
            assert_eq!(
                reminder.step(at, clock + EVERY, || settled.clone()),
                None,
                "{settled:?}"
            );
        }

        // The notice's actions reach the session wherever the notifications publish.
        for (action, answer) in [(BACK_UP_ACTION, Due::BackUp), (STOP_ACTION, Due::Stop)] {
            let id = notifications.alloc_id();
            notifications.push(tilo_notifications_service::Notification {
                actions: vec![(action.to_owned(), action.to_owned())],
                ..tilo_notifications_service::Notification::new(id)
            });
            notifications.action_invoked(id, action, None);
            assert_eq!(
                reminder.step(at, clock, Map::new),
                Some(answer),
                "{action} is the person's answer, taken at once"
            );
        }
    }
}
