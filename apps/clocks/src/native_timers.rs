//! Reconcile durable timer state with the platform's notification
//! scheduler, where it keeps notifications for an app that is not running
//! (`ui::notify::Feature::Schedule`).
use crate::ClockState;
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use ui::notify::{Notification, Notifier, Scheduled};
use ui::time::SystemTime;

/// What a click on a timer's notification opens.
pub(crate) const TIMERS_URI: &str = "tilo-clocks:timers";
/// What a click on an alarm's notification opens.
pub(crate) const ALARMS_URI: &str = "tilo-clocks:alarms";

pub(crate) trait Scheduler {
    fn pending(&self) -> Result<Vec<Scheduled>, String>;
    fn cancel(&self, id: &str) -> Result<(), String>;
    fn schedule(&self, id: &str, label: &str, end: SystemTime) -> Result<(), String>;
}

impl Scheduler for dyn Notifier {
    fn pending(&self) -> Result<Vec<Scheduled>, String> {
        self.scheduled().map_err(|error| error.to_string())
    }
    fn cancel(&self, id: &str) -> Result<(), String> {
        self.cancel_scheduled(id).map_err(|error| error.to_string())
    }
    fn schedule(&self, id: &str, label: &str, end: SystemTime) -> Result<(), String> {
        let notification =
            Notification::new(id, "Timer finished", label).with_activation(TIMERS_URI);
        Notifier::schedule(self, &notification, end).map_err(|error| error.to_string())
    }
}

/// `at` on the platform clock (which has no chrono conversion on the web).
fn system_time(at: DateTime<Utc>) -> SystemTime {
    let millis = u64::try_from(at.timestamp_millis()).unwrap_or(0);
    SystemTime::UNIX_EPOCH
        .checked_add(std::time::Duration::from_millis(millis))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

pub(crate) fn timer_id(id: uuid::Uuid) -> String {
    format!(
        "t{}",
        id.simple().to_string().chars().take(14).collect::<String>()
    )
}

pub(crate) fn reconcile(
    state: &mut ClockState,
    host: &(impl Scheduler + ?Sized),
    now: DateTime<Utc>,
    mut persist: impl FnMut(&ClockState) -> Result<(), String>,
) -> Result<(), String> {
    state
        .native_timer_owners
        .extend(state.timers.iter().map(|timer| timer.id));
    if state.native_timer_owners.is_empty() {
        return Ok(());
    }
    // Record ownership before touching the platform. Deleting a timer or crashing
    // after submission must not lose the ability to cancel its native alert.
    persist(state)?;
    let owned = state
        .native_timer_owners
        .iter()
        .copied()
        .map(timer_id)
        .collect::<HashSet<_>>();
    let wanted = state
        .timers
        .iter()
        .filter(|timer| timer.ends_at_utc.is_some())
        .map(|timer| timer_id(timer.id))
        .collect::<HashSet<_>>();
    let pending = host.pending()?;
    let mut retained = pending
        .iter()
        .map(|notice| notice.tag.clone())
        .collect::<HashSet<_>>();
    for pending in pending {
        if owned.contains(&pending.tag)
            && !wanted.contains(&pending.tag)
            && pending.at > ui::time::wall()
        {
            host.cancel(&pending.tag)?;
            retained.remove(&pending.tag);
        }
    }
    let mut result = Ok(());
    for timer in &mut state.timers {
        let Some(end) = timer.ends_at_utc else {
            timer.native_scheduled_end = None;
            continue;
        };
        if end <= now {
            continue;
        }
        match host.schedule(&timer_id(timer.id), &timer.label, system_time(end)) {
            Ok(()) => {
                timer.native_scheduled_end = Some(end);
                retained.insert(timer_id(timer.id));
            }
            Err(error) => {
                timer.native_scheduled_end = None;
                result = Err(error);
            }
        }
    }
    state.native_timer_owners.retain(|id| {
        let id = timer_id(*id);
        wanted.contains(&id) || retained.contains(&id)
    });
    persist(state)?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        collections::BTreeMap,
        time::Duration,
    };

    #[derive(Default)]
    struct TestScheduler {
        pending: RefCell<BTreeMap<String, SystemTime>>,
        fail_cancel: Cell<bool>,
    }
    impl Scheduler for TestScheduler {
        fn pending(&self) -> Result<Vec<Scheduled>, String> {
            Ok(self
                .pending
                .borrow()
                .iter()
                .map(|(id, end)| Scheduled {
                    tag: id.clone(),
                    at: *end,
                })
                .collect())
        }
        fn cancel(&self, id: &str) -> Result<(), String> {
            if self.fail_cancel.get() {
                return Err("cancellation unavailable".into());
            }
            self.pending.borrow_mut().remove(id);
            Ok(())
        }
        fn schedule(&self, id: &str, _: &str, end: SystemTime) -> Result<(), String> {
            self.pending.borrow_mut().insert(id.to_owned(), end);
            Ok(())
        }
    }

    fn running_state() -> ClockState {
        let mut timer = crate::Timer::new("Owned timer", Duration::from_secs(3600));
        timer.start(Utc::now());
        ClockState {
            timers: vec![timer],
            ..ClockState::default()
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn reconcile_touches_only_owned_timers_and_persists_ownership_before_the_host() {
        // Case: opening an empty profile preserves another profile's timer.
        {
            let host = TestScheduler::default();
            let foreign_id = timer_id(uuid::Uuid::new_v4());
            host.schedule(
                &foreign_id,
                "Other profile",
                ui::time::wall() + Duration::from_secs(3600),
            )
            .unwrap();
            reconcile(&mut ClockState::default(), &host, Utc::now(), |_| Ok(())).unwrap();
            assert!(
                host.pending.borrow().contains_key(&foreign_id),
                "another profile's timer was cancelled"
            );
        }

        // Case: a deleted timer's cancellation survives failure and restart
        // without touching another profile.
        {
            let host = TestScheduler::default();
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("Grün profile/clocks.json");
            let mut first = running_state();
            let first_id = first.timers[0].id;
            reconcile(&mut first, &host, Utc::now(), |s| {
                s.save_at(&path).map_err(|e| e.to_string())
            })
            .unwrap();
            let mut second = running_state();
            let second_id = timer_id(second.timers[0].id);
            reconcile(&mut second, &host, Utc::now(), |_| Ok(())).unwrap();
            assert_eq!(
                host.pending.borrow().len(),
                2,
                "case deleted-timer: both profiles scheduled"
            );

            first.timers.clear();
            host.fail_cancel.set(true);
            assert!(
                reconcile(&mut first, &host, Utc::now(), |s| s
                    .save_at(&path)
                    .map_err(|e| e.to_string()))
                .is_err(),
                "case deleted-timer: failed cancellation is reported"
            );
            let mut restarted = ClockState::load_at(&path);
            assert!(
                restarted.timers.is_empty(),
                "case deleted-timer: deletion persisted"
            );
            assert!(
                restarted.native_timer_owners.contains(&first_id),
                "case deleted-timer: ownership retained for retry"
            );
            host.fail_cancel.set(false);
            reconcile(&mut restarted, &host, Utc::now(), |s| {
                s.save_at(&path).map_err(|e| e.to_string())
            })
            .unwrap();
            assert_eq!(
                host.pending.borrow().len(),
                1,
                "case deleted-timer: retry cancels"
            );
            assert!(
                host.pending.borrow().contains_key(&second_id),
                "case deleted-timer: other profile untouched"
            );
            assert!(
                ClockState::load_at(&path).native_timer_owners.is_empty(),
                "case deleted-timer: ownership released"
            );
        }

        // Case: a storage failure prevents native submission.
        {
            let host = TestScheduler::default();
            let mut state = running_state();
            assert!(
                reconcile(&mut state, &host, Utc::now(), |_| Err(
                    "storage unavailable".into()
                ))
                .is_err(),
                "case storage-failure: reported"
            );
            assert!(
                host.pending.borrow().is_empty(),
                "case storage-failure: nothing submitted"
            );
            assert!(
                state.timers[0].native_scheduled_end.is_none(),
                "case storage-failure: no handoff recorded"
            );
        }

        // Case: an elapsed notification is retained until Windows delivers it.
        {
            let host = TestScheduler::default();
            let id = uuid::Uuid::new_v4();
            let mut state = ClockState::default();
            state.native_timer_owners.insert(id);
            host.pending
                .borrow_mut()
                .insert(timer_id(id), ui::time::wall() - Duration::from_secs(60));
            reconcile(&mut state, &host, Utc::now(), |_| Ok(())).unwrap();
            assert!(
                host.pending.borrow().contains_key(&timer_id(id)),
                "case elapsed: still pending on the host"
            );
            assert!(
                state.native_timer_owners.contains(&id),
                "case elapsed: ownership retained"
            );
            host.pending.borrow_mut().clear();
            reconcile(&mut state, &host, Utc::now(), |_| Ok(())).unwrap();
            assert!(
                state.native_timer_owners.is_empty(),
                "case elapsed: ownership released after delivery"
            );
        }
    }
}
