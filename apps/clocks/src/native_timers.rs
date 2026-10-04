//! Reconcile durable timer state with the platform's notification
//! scheduler, where it keeps notifications for an app that is not running
//! (`ui::notify::Feature::Schedule`).
use crate::Clocks;
use crate::schedule::Delivery;
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

pub(crate) fn timer_id(id: &str) -> String {
    format!("t{}", id.chars().take(14).collect::<String>())
}

/// Bring the platform's scheduled notifications in line with the running
/// timers of `clocks`, recording in `delivery` what this device owns and
/// what the platform accepted. `persist` stores `delivery`; it runs before
/// the platform is touched, so deleting a timer or crashing after a
/// submission never loses the ability to cancel its notification.
pub(crate) fn reconcile(
    clocks: &Clocks,
    delivery: &mut Delivery,
    host: &(impl Scheduler + ?Sized),
    now: DateTime<Utc>,
    mut persist: impl FnMut(&Delivery) -> Result<(), String>,
) -> Result<(), String> {
    delivery
        .native_owners
        .extend(clocks.timers.ids().map(|id| id.as_str().to_owned()));
    if delivery.native_owners.is_empty() {
        return Ok(());
    }
    persist(delivery)?;
    let owned = delivery
        .native_owners
        .iter()
        .map(|id| timer_id(id))
        .collect::<HashSet<_>>();
    let wanted = clocks
        .timers
        .iter()
        .filter(|(_, timer)| timer.ends_at_ms.is_some())
        .map(|(id, _)| timer_id(id.as_str()))
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
    for (id, timer) in clocks.timers.iter() {
        let Some(end) = timer.ends_at() else {
            delivery.native_scheduled.remove(id.as_str());
            continue;
        };
        if end <= now {
            continue;
        }
        match host.schedule(&timer_id(id.as_str()), &timer.label, system_time(end)) {
            Ok(()) => {
                delivery
                    .native_scheduled
                    .insert(id.as_str().to_owned(), end.timestamp_millis());
                retained.insert(timer_id(id.as_str()));
            }
            Err(error) => {
                delivery.native_scheduled.remove(id.as_str());
                result = Err(error);
            }
        }
    }
    delivery.native_owners.retain(|id| {
        let id = timer_id(id);
        wanted.contains(&id) || retained.contains(&id)
    });
    persist(delivery)?;
    result
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        collections::BTreeMap,
        time::Duration,
    };
    use ui::prelude::{Collection, Id};

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

    fn running(label: &str) -> Clocks {
        let mut clocks = Clocks::default();
        clocks.timers.push(
            Id::from_raw(uuid::Uuid::new_v4().simple().to_string()),
            crate::Timer {
                label: label.into(),
                duration_ms: 3_600_000,
                ends_at_ms: Some(Utc::now().timestamp_millis() + 3_600_000),
                paused_remaining_ms: None,
            },
        );
        clocks
    }

    fn first_id(clocks: &Clocks) -> String {
        clocks.timers.ids().next().unwrap().as_str().to_owned()
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn reconcile_touches_only_owned_timers_and_persists_ownership_before_the_host() {
        // Case: opening an empty profile preserves another profile's timer.
        {
            let host = TestScheduler::default();
            let foreign_id = timer_id("0123456789abcdef");
            host.schedule(
                &foreign_id,
                "Other profile",
                ui::time::wall() + Duration::from_secs(3600),
            )
            .unwrap();
            reconcile(
                &Clocks::default(),
                &mut Delivery::default(),
                &host,
                Utc::now(),
                |_| Ok(()),
            )
            .unwrap();
            assert!(
                host.pending.borrow().contains_key(&foreign_id),
                "another profile's timer was cancelled"
            );
        }

        // Case: a deleted timer's cancellation survives failure and restart
        // without touching another profile.
        {
            let host = TestScheduler::default();
            let stored = RefCell::new(Delivery::default());
            let store = |delivery: &Delivery| {
                *stored.borrow_mut() = delivery.clone();
                Ok(())
            };
            let mut first = running("Owned timer");
            let owned_id = first_id(&first);
            let mut delivery = Delivery::default();
            reconcile(&first, &mut delivery, &host, Utc::now(), store).unwrap();
            assert_eq!(
                stored.borrow().native_scheduled.len(),
                1,
                "case deleted-timer: the handoff is recorded"
            );
            let second = running("Other profile");
            let second_id = timer_id(&first_id(&second));
            reconcile(&second, &mut Delivery::default(), &host, Utc::now(), |_| {
                Ok(())
            })
            .unwrap();
            assert_eq!(
                host.pending.borrow().len(),
                2,
                "case deleted-timer: both profiles scheduled"
            );

            first.timers = Collection::default();
            host.fail_cancel.set(true);
            assert!(
                reconcile(&first, &mut delivery, &host, Utc::now(), store).is_err(),
                "case deleted-timer: failed cancellation is reported"
            );
            let mut restarted = stored.borrow().clone();
            assert!(
                restarted.native_owners.contains(&owned_id),
                "case deleted-timer: ownership retained for retry"
            );
            host.fail_cancel.set(false);
            reconcile(&first, &mut restarted, &host, Utc::now(), store).unwrap();
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
                stored.borrow().native_owners.is_empty(),
                "case deleted-timer: ownership released"
            );
        }

        // Case: a storage failure prevents native submission.
        {
            let host = TestScheduler::default();
            let clocks = running("Owned timer");
            let mut delivery = Delivery::default();
            assert!(
                reconcile(&clocks, &mut delivery, &host, Utc::now(), |_| Err(
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
                delivery.native_scheduled.is_empty(),
                "case storage-failure: no handoff recorded"
            );
        }

        // Case: an elapsed notification is retained until Windows delivers it.
        {
            let host = TestScheduler::default();
            let id = "fedcba9876543210".to_owned();
            let mut delivery = Delivery::default();
            delivery.native_owners.insert(id.clone());
            host.pending
                .borrow_mut()
                .insert(timer_id(&id), ui::time::wall() - Duration::from_secs(60));
            reconcile(&Clocks::default(), &mut delivery, &host, Utc::now(), |_| {
                Ok(())
            })
            .unwrap();
            assert!(
                host.pending.borrow().contains_key(&timer_id(&id)),
                "case elapsed: still pending on the host"
            );
            assert!(
                delivery.native_owners.contains(&id),
                "case elapsed: ownership retained"
            );
            host.pending.borrow_mut().clear();
            reconcile(&Clocks::default(), &mut delivery, &host, Utc::now(), |_| {
                Ok(())
            })
            .unwrap();
            assert!(
                delivery.native_owners.is_empty(),
                "case elapsed: ownership released after delivery"
            );
        }
    }
}
