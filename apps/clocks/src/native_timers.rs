//! Reconcile durable timer state with the Windows notification scheduler.
use crate::ClockState;
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::time::SystemTime;
use ui::host_notifications::{HostNotifications, ScheduledNotice};

pub(crate) trait Scheduler {
    fn pending(&self) -> Result<Vec<ScheduledNotice>, String>;
    fn cancel(&self, id: &str) -> Result<(), String>;
    fn schedule(&self, id: &str, label: &str, end: SystemTime) -> Result<(), String>;
}

impl Scheduler for HostNotifications {
    fn pending(&self) -> Result<Vec<ScheduledNotice>, String> {
        self.scheduled().map_err(|error| error.to_string())
    }
    fn cancel(&self, id: &str) -> Result<(), String> {
        self.cancel_schedule(id).map_err(|error| error.to_string())
    }
    fn schedule(&self, id: &str, label: &str, end: SystemTime) -> Result<(), String> {
        HostNotifications::schedule(self, id, "Timer finished", label, end)
            .map_err(|error| error.to_string())
    }
}

pub(crate) fn timer_id(id: uuid::Uuid) -> String {
    format!(
        "t{}",
        id.simple().to_string().chars().take(14).collect::<String>()
    )
}

pub(crate) fn reconcile(
    state: &mut ClockState,
    host: &impl Scheduler,
    now: DateTime<Utc>,
    mut persist: impl FnMut(&ClockState) -> Result<(), String>,
) -> Result<(), String> {
    state
        .native_timer_owners
        .extend(state.timers.iter().map(|timer| timer.id));
    if state.native_timer_owners.is_empty() {
        return Ok(());
    }
    // Record ownership before touching Windows. Deleting a timer or crashing
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
        .map(|notice| notice.id.clone())
        .collect::<HashSet<_>>();
    for pending in pending {
        if owned.contains(&pending.id)
            && !wanted.contains(&pending.id)
            && pending.delivery_time > SystemTime::now()
        {
            host.cancel(&pending.id)?;
            retained.remove(&pending.id);
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
        match host.schedule(&timer_id(timer.id), &timer.label, end.into()) {
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
        fn pending(&self) -> Result<Vec<ScheduledNotice>, String> {
            Ok(self
                .pending
                .borrow()
                .iter()
                .map(|(id, end)| ScheduledNotice {
                    id: id.clone(),
                    delivery_time: *end,
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
                SystemTime::now() + Duration::from_secs(3600),
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
                .insert(timer_id(id), SystemTime::now() - Duration::from_secs(60));
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
