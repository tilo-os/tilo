//! Reconcile durable timer and alarm state with the platform's notification
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
    fn schedule_alarm(
        &self,
        id: &str,
        activation: &str,
        alarm: &crate::Alarm,
        end: SystemTime,
        last_rung: Option<chrono::NaiveDate>,
    ) -> Result<(), String>;
}

impl Scheduler for dyn Notifier {
    fn schedule_alarm(
        &self,
        id: &str,
        activation: &str,
        alarm: &crate::Alarm,
        end: SystemTime,
        last_rung: Option<chrono::NaiveDate>,
    ) -> Result<(), String> {
        let notification = Notification::new(id, "Alarm", &alarm.label).with_activation(activation);
        Notifier::schedule_alarm(
            self,
            &notification,
            end,
            ui::notify::AlarmSchedule {
                alarm: ui::notify::CalendarAlarm {
                    hour: alarm.hour,
                    minute: alarm.minute,
                    weekdays: alarm.repeat,
                },
                enabled_at_ms: alarm.enabled_at_ms,
                last_rung,
            },
        )
        .map_err(|error| error.to_string())
    }
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

pub(crate) fn alarm_activation(record: Option<&ui::record::RecordId>, alarm: &str) -> String {
    record.map_or_else(
        || format!("tilo-clocks:alarm/{alarm}"),
        |record| format!("tilo-clocks:record/{record}/alarm/{alarm}"),
    )
}

pub(crate) fn activation_record(uri: &str) -> Option<ui::record::RecordId> {
    let path = uri.strip_prefix("tilo-clocks:record/")?;
    let record = path
        .split_once("/alarm/")
        .map_or(path, |(record, _)| record);
    ui::record::RecordId::parse(record)
}

pub(crate) fn activation_alarm(uri: &str) -> Option<&str> {
    uri.strip_prefix("tilo-clocks:alarm/").or_else(|| {
        let (record, alarm) = uri
            .strip_prefix("tilo-clocks:record/")?
            .split_once("/alarm/")?;
        ui::record::RecordId::parse(record)?;
        Some(alarm)
    })
}

pub(crate) fn alarm_id(id: &str) -> String {
    format!("a{}", id.chars().take(14).collect::<String>())
}

/// Bring the platform's scheduled notifications in line with the running
/// timers and alarms of `clocks`, recording in `delivery` what this device owns and
/// what the platform accepted. `persist` stores `delivery`; it runs before
/// the platform is touched, so deleting a timer or crashing after a
/// submission never loses the ability to cancel its notification.
pub(crate) fn reconcile(
    record: Option<&ui::record::RecordId>,
    clocks: &Clocks,
    delivery: &mut Delivery,
    host: &(impl Scheduler + ?Sized),
    now: DateTime<Utc>,
    mut persist: impl FnMut(&Delivery) -> Result<(), String>,
) -> Result<(), String> {
    delivery
        .native_owners
        .extend(clocks.timers.ids().map(|id| id.as_str().to_owned()));
    delivery
        .native_alarm_owners
        .extend(clocks.alarms.ids().map(|id| id.as_str().to_owned()));
    if delivery.native_owners.is_empty() && delivery.native_alarm_owners.is_empty() {
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
    if let Err(error) = reconcile_alarms(record, clocks, delivery, host, now) {
        result = Err(error);
    }
    persist(delivery)?;
    result
}

/// Restore service-adjusted deadlines before the app ticks: the calendar
/// may have followed a timezone change or rung while this process was closed.
pub(crate) fn restore_alarms(
    clocks: &Clocks,
    delivery: &mut Delivery,
    states: &[ui::notify::AlarmState],
) {
    for (id, alarm) in clocks.alarms.iter().filter(|(_, alarm)| alarm.enabled) {
        let Some(state) = states.iter().find(|state| {
            state.tag == alarm_id(id.as_str())
                && state.enabled_at_ms == alarm.enabled_at_ms
                && state.alarm
                    == ui::notify::CalendarAlarm {
                        hour: alarm.hour,
                        minute: alarm.minute,
                        weekdays: alarm.repeat,
                    }
        }) else {
            continue;
        };
        let millis = |at: SystemTime| {
            at.duration_since(SystemTime::UNIX_EPOCH)
                .ok()
                .and_then(|d| i64::try_from(d.as_millis()).ok())
        };
        let next = state.next_at.and_then(millis).or_else(|| {
            if alarm.repeat == 0 {
                state.last_at.and_then(millis)
            } else {
                None
            }
        });
        delivery.alarms.insert(
            id.as_str().to_owned(),
            crate::schedule::ArmedAlarm {
                schedule: crate::schedule::schedule_of(alarm),
                next_due_ms: next,
                last_rung: state.last_rung,
            },
        );
        if let Some(next) = next {
            delivery
                .native_alarms_scheduled
                .insert(id.as_str().to_owned(), next);
        }
    }
}

fn reconcile_alarms(
    record: Option<&ui::record::RecordId>,
    clocks: &Clocks,
    delivery: &mut Delivery,
    host: &(impl Scheduler + ?Sized),
    now: DateTime<Utc>,
) -> Result<(), String> {
    let wanted = clocks
        .alarms
        .iter()
        .filter(|(_, alarm)| alarm.enabled)
        .filter_map(|(id, _)| {
            Some((
                id.as_str().to_owned(),
                delivery.alarms.get(id.as_str())?.next_due_ms?,
            ))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    delivery.native_completed_alarms.retain(|id, generation| {
        clocks
            .alarms
            .get(&ui::prelude::Id::from_raw(id.as_str()))
            .is_some_and(|alarm| {
                alarm.repeat == 0 && *generation == crate::schedule::schedule_of(alarm)
            })
    });
    let mut result = Ok(());
    for id in delivery.native_alarm_owners.clone() {
        if !wanted.contains_key(&id) && !delivery.native_completed_alarms.contains_key(&id) {
            host.cancel(&alarm_id(&id))?;
            delivery.native_alarm_owners.remove(&id);
            delivery.native_alarms_scheduled.remove(&id);
        }
    }
    for (id, alarm) in clocks.alarms.iter() {
        let Some(&deadline) = wanted.get(id.as_str()) else {
            continue;
        };
        if deadline <= now.timestamp_millis() {
            continue;
        }
        let Some(at) = DateTime::<Utc>::from_timestamp_millis(deadline) else {
            continue;
        };
        let last_rung = delivery
            .alarms
            .get(id.as_str())
            .and_then(|armed| armed.last_rung);
        match host.schedule_alarm(
            &alarm_id(id.as_str()),
            &alarm_activation(record, id.as_str()),
            alarm,
            system_time(at),
            last_rung,
        ) {
            Ok(()) => {
                delivery
                    .native_alarms_scheduled
                    .insert(id.as_str().to_owned(), deadline);
            }
            Err(error) => {
                result = Err(error);
            }
        }
    }
    result
}

#[cfg(test)]
#[allow(
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
    struct CapturedAlarm(std::sync::Mutex<Option<Notification>>);
    impl Notifier for CapturedAlarm {
        fn show(&self, _: &Notification) -> Result<(), ui::notify::NotifyError> {
            Err(ui::notify::NotifyError::Unavailable)
        }
        fn withdraw(&self, _: &str) -> Result<(), ui::notify::NotifyError> {
            Ok(())
        }
        fn scheduled(&self) -> Result<Vec<Scheduled>, ui::notify::NotifyError> {
            Ok(Vec::new())
        }
        fn schedule(
            &self,
            notification: &Notification,
            _: SystemTime,
        ) -> Result<(), ui::notify::NotifyError> {
            *self.0.lock().unwrap() = Some(notification.clone());
            Ok(())
        }
    }

    #[test]
    fn real_alarm_handoff_transmits_the_full_resource_identity() {
        let now = chrono::Local::now();
        let due = now + chrono::Duration::minutes(5);
        let mut clocks = Clocks::default();
        clocks.alarms.insert(
            0,
            Id::from_raw("complete-alarm-resource-identity"),
            crate::Alarm {
                label: "Requested".into(),
                hour: u8::try_from(chrono::Timelike::hour(&due)).unwrap(),
                minute: u8::try_from(chrono::Timelike::minute(&due)).unwrap(),
                enabled: true,
                enabled_at_ms: now.timestamp_millis(),
                ..crate::Alarm::default()
            },
        );
        let mut delivery = Delivery::default();
        crate::schedule::tick(&clocks, &mut delivery, now.with_timezone(&Utc), now);
        let captured = CapturedAlarm::default();
        let host: &dyn Notifier = &captured;
        let record = ui::record::RecordId::minted();
        reconcile(
            Some(&record),
            &clocks,
            &mut delivery,
            host,
            now.with_timezone(&Utc),
            |_| Ok(()),
        )
        .unwrap();
        let notification = captured.0.lock().unwrap().clone().unwrap();
        assert_eq!(
            notification.activation.as_deref(),
            Some(
                format!("tilo-clocks:record/{record}/alarm/complete-alarm-resource-identity")
                    .as_str()
            )
        );
    }

    #[derive(Default)]
    struct TestScheduler {
        pending: RefCell<BTreeMap<String, SystemTime>>,
        fail_cancel: Cell<bool>,
        alarm_last_rung: RefCell<BTreeMap<String, Option<chrono::NaiveDate>>>,
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
        fn schedule_alarm(
            &self,
            id: &str,
            _: &str,
            _: &crate::Alarm,
            end: SystemTime,
            last_rung: Option<chrono::NaiveDate>,
        ) -> Result<(), String> {
            self.alarm_last_rung
                .borrow_mut()
                .insert(id.to_owned(), last_rung);
            self.pending.borrow_mut().insert(id.to_owned(), end);
            Ok(())
        }
        fn schedule(&self, id: &str, _: &str, end: SystemTime) -> Result<(), String> {
            self.pending.borrow_mut().insert(id.to_owned(), end);
            Ok(())
        }
    }

    #[test]
    fn enabled_alarm_is_handed_to_scheduler_before_app_closes() {
        let now = chrono::Local::now();
        let due = now + chrono::Duration::minutes(2);
        let mut clocks = Clocks::default();
        clocks.alarms.push(
            Id::from_raw("closed-app-alarm"),
            crate::Alarm {
                label: "Wake up".into(),
                hour: u8::try_from(chrono::Timelike::hour(&due)).unwrap(),
                minute: u8::try_from(chrono::Timelike::minute(&due)).unwrap(),
                repeat: 0,
                enabled: true,
                enabled_at_ms: now.timestamp_millis(),
            },
        );
        let mut delivery = Delivery::default();
        assert!(
            crate::schedule::tick(&clocks, &mut delivery, now.with_timezone(&Utc), now)
                .due
                .is_empty()
        );
        let stored = RefCell::new(Delivery::default());
        let host = TestScheduler::default();
        reconcile(
            None,
            &clocks,
            &mut delivery,
            &host,
            now.with_timezone(&Utc),
            |d| {
                *stored.borrow_mut() = d.clone();
                Ok(())
            },
        )
        .unwrap();
        // Closing Clocks means no more ticks. Only the platform can deliver
        // the armed deadline, so it must have been submitted before closing.
        drop(delivery);
        assert_eq!(
            host.pending().unwrap().len(),
            1,
            "an enabled alarm must survive the app closing in the scheduler"
        );
        assert!(
            !stored.borrow().alarms.is_empty(),
            "the armed occurrence must survive restart"
        );
    }

    #[test]
    fn disabling_alarm_retries_failed_cancellation_after_restart() {
        let now = chrono::Local::now();
        let due = now + chrono::Duration::minutes(2);
        let id = Id::from_raw("cancel-alarm");
        let mut clocks = Clocks::default();
        clocks.alarms.push(
            id.clone(),
            crate::Alarm {
                label: "Wake up".into(),
                hour: u8::try_from(chrono::Timelike::hour(&due)).unwrap(),
                minute: u8::try_from(chrono::Timelike::minute(&due)).unwrap(),
                repeat: 0,
                enabled: true,
                enabled_at_ms: now.timestamp_millis(),
            },
        );
        let mut delivery = Delivery::default();
        crate::schedule::tick(&clocks, &mut delivery, now.with_timezone(&Utc), now);
        let stored = RefCell::new(Delivery::default());
        let persist = |d: &Delivery| {
            *stored.borrow_mut() = d.clone();
            Ok(())
        };
        let host = TestScheduler::default();
        reconcile(
            None,
            &clocks,
            &mut delivery,
            &host,
            now.with_timezone(&Utc),
            persist,
        )
        .unwrap();
        clocks.alarms.get_mut(&id).unwrap().enabled = false;
        host.fail_cancel.set(true);
        assert!(
            reconcile(
                None,
                &clocks,
                &mut delivery,
                &host,
                now.with_timezone(&Utc),
                persist
            )
            .is_err()
        );
        let mut restarted = stored.borrow().clone();
        assert!(restarted.native_alarm_owners.contains(id.as_str()));
        assert_eq!(host.pending().unwrap().len(), 1);
        host.fail_cancel.set(false);
        reconcile(
            None,
            &clocks,
            &mut restarted,
            &host,
            now.with_timezone(&Utc),
            persist,
        )
        .unwrap();
        assert!(host.pending().unwrap().is_empty());
        assert!(stored.borrow().native_alarm_owners.is_empty());
        assert!(stored.borrow().native_alarms_scheduled.is_empty());
    }

    #[test]
    fn reopening_completed_one_shot_preserves_host_snooze_until_edit() {
        let now = chrono::Local::now();
        let due = now + chrono::Duration::minutes(2);
        let id = Id::from_raw("completed-one-shot");
        let mut clocks = Clocks::default();
        clocks.alarms.push(
            id.clone(),
            crate::Alarm {
                label: "Fixture".into(),
                hour: u8::try_from(chrono::Timelike::hour(&due)).unwrap(),
                minute: u8::try_from(chrono::Timelike::minute(&due)).unwrap(),
                repeat: 0,
                enabled: true,
                enabled_at_ms: now.timestamp_millis(),
            },
        );
        let mut delivery = Delivery::default();
        let host = TestScheduler::default();
        crate::schedule::tick(&clocks, &mut delivery, now.with_timezone(&Utc), now);
        reconcile(
            None,
            &clocks,
            &mut delivery,
            &host,
            now.with_timezone(&Utc),
            |_| Ok(()),
        )
        .unwrap();
        let deadline = delivery.alarms[id.as_str()].next_due_ms.unwrap();
        let ring = DateTime::<Utc>::from_timestamp_millis(deadline).unwrap();
        let tick = crate::schedule::tick(
            &clocks,
            &mut delivery,
            ring,
            ring.with_timezone(&chrono::Local),
        );
        assert_eq!(tick.finished_alarms, vec![id.clone()]);
        assert!(matches!(
            tick.due.as_slice(),
            [crate::schedule::DueEvent::Alarm {
                native_scheduled: true,
                ..
            }]
        ));
        // The app's existing automatic completion edit follows the tick.
        clocks.alarms.get_mut(&id).unwrap().enabled = false;
        let encoded = serde_json::to_vec(&delivery).unwrap();
        let mut restarted: Delivery = serde_json::from_slice(&encoded).unwrap();
        crate::schedule::tick(
            &clocks,
            &mut restarted,
            ring,
            ring.with_timezone(&chrono::Local),
        );
        reconcile(None, &clocks, &mut restarted, &host, ring, |_| Ok(())).unwrap();
        assert!(
            host.pending.borrow().contains_key(&alarm_id(id.as_str())),
            "automatic completion canceled the retained host occurrence"
        );
        assert!(restarted.native_alarm_owners.contains(id.as_str()));
        clocks.alarms.get_mut(&id).unwrap().minute =
            (clocks.alarms.get(&id).unwrap().minute + 1) % 60;
        reconcile(None, &clocks, &mut restarted, &host, ring, |_| Ok(())).unwrap();
        assert!(
            host.pending.borrow().is_empty(),
            "editing a completed generation must cancel it"
        );
        assert!(restarted.native_completed_alarms.is_empty());
        assert!(restarted.native_alarm_owners.is_empty());
    }

    #[test]
    fn reenabled_alarm_hands_off_the_previously_delivered_date() {
        let now = chrono::Local::now();
        let id = Id::from_raw("rearmed-alarm");
        let mut clocks = Clocks::default();
        clocks.alarms.push(
            id.clone(),
            crate::Alarm {
                label: "Wake up".into(),
                hour: 12,
                minute: 0,
                repeat: 0,
                enabled: true,
                enabled_at_ms: now.timestamp_millis(),
            },
        );
        let mut delivery = Delivery::default();
        delivery.alarms.insert(
            id.as_str().to_owned(),
            crate::schedule::ArmedAlarm {
                last_rung: Some(now.date_naive()),
                ..Default::default()
            },
        );
        crate::schedule::tick(&clocks, &mut delivery, now.with_timezone(&Utc), now);
        let host = TestScheduler::default();
        reconcile(
            None,
            &clocks,
            &mut delivery,
            &host,
            now.with_timezone(&Utc),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(
            host.alarm_last_rung.borrow().get(&alarm_id(id.as_str())),
            Some(&Some(now.date_naive())),
            "the host must exclude the app's consumed local date"
        );
    }

    #[test]
    fn service_history_restores_occurrences_without_rearming_delivered_alarms() {
        let now = chrono::Local::now();
        let yesterday = now - chrono::Duration::days(1);
        let mut clocks = Clocks::default();
        let id = Id::from_raw("restore-alarm");
        clocks.alarms.push(
            id.clone(),
            crate::Alarm {
                label: "Wake up".into(),
                hour: 12,
                minute: 0,
                repeat: 0,
                enabled: true,
                enabled_at_ms: 1,
            },
        );
        let mut delivery = Delivery::default();
        let mut state = ui::notify::AlarmState {
            tag: alarm_id(id.as_str()),
            alarm: ui::notify::CalendarAlarm {
                hour: 12,
                minute: 0,
                weekdays: 0,
            },
            enabled_at_ms: 1,
            next_at: None,
            last_at: Some(system_time(yesterday.with_timezone(&Utc))),
            last_rung: Some(yesterday.date_naive()),
        };
        restore_alarms(&clocks, &mut delivery, &[state.clone()]);
        let due = crate::schedule::tick(&clocks, &mut delivery, now.with_timezone(&Utc), now);
        assert!(matches!(
            due.due.as_slice(),
            [crate::DueEvent::Alarm {
                native_scheduled: true,
                ..
            }]
        ));
        assert_eq!(due.finished_alarms, vec![id.clone()]);
        assert!(
            crate::schedule::tick(&clocks, &mut delivery, now.with_timezone(&Utc), now)
                .due
                .is_empty()
        );
        // A timezone-adjusted future occurrence is restored exactly, rather
        // than cancelling it because the app retained the old UTC deadline.
        let future = now.with_timezone(&Utc) + chrono::Duration::hours(4);
        state.next_at = Some(system_time(future));
        state.last_at = None;
        state.last_rung = None;
        restore_alarms(&clocks, &mut delivery, &[state]);
        assert_eq!(
            delivery.alarms.get(id.as_str()).unwrap().next_due_ms,
            Some(future.timestamp_millis())
        );
        assert_eq!(
            delivery.native_alarms_scheduled.get(id.as_str()),
            Some(&future.timestamp_millis())
        );
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
                None,
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
            reconcile(None, &first, &mut delivery, &host, Utc::now(), store).unwrap();
            assert_eq!(
                stored.borrow().native_scheduled.len(),
                1,
                "case deleted-timer: the handoff is recorded"
            );
            let second = running("Other profile");
            let second_id = timer_id(&first_id(&second));
            reconcile(
                None,
                &second,
                &mut Delivery::default(),
                &host,
                Utc::now(),
                |_| Ok(()),
            )
            .unwrap();
            assert_eq!(
                host.pending.borrow().len(),
                2,
                "case deleted-timer: both profiles scheduled"
            );

            first.timers = Collection::default();
            host.fail_cancel.set(true);
            assert!(
                reconcile(None, &first, &mut delivery, &host, Utc::now(), store).is_err(),
                "case deleted-timer: failed cancellation is reported"
            );
            let mut restarted = stored.borrow().clone();
            assert!(
                restarted.native_owners.contains(&owned_id),
                "case deleted-timer: ownership retained for retry"
            );
            host.fail_cancel.set(false);
            reconcile(None, &first, &mut restarted, &host, Utc::now(), store).unwrap();
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
                reconcile(None, &clocks, &mut delivery, &host, Utc::now(), |_| Err(
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
            reconcile(
                None,
                &Clocks::default(),
                &mut delivery,
                &host,
                Utc::now(),
                |_| Ok(()),
            )
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
            reconcile(
                None,
                &Clocks::default(),
                &mut delivery,
                &host,
                Utc::now(),
                |_| Ok(()),
            )
            .unwrap();
            assert!(
                delivery.native_owners.is_empty(),
                "case elapsed: ownership released after delivery"
            );
        }
    }
}
