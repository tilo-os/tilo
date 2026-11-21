use super::*;
use crate::model::{Timer, WeekdayKey, repeat_mask};
use jiff::civil::Time;
use std::collections::BTreeSet;

fn local(y: i16, mo: i8, d: i8, h: i8, mi: i8, s: i8) -> jiff::Zoned {
    in_zone(&jiff::tz::TimeZone::system(), y, mo, d, h, mi, s)
}

fn in_zone(zone: &jiff::tz::TimeZone, y: i16, mo: i8, d: i8, h: i8, mi: i8, s: i8) -> jiff::Zoned {
    jiff::civil::date(y, mo, d)
        .at(h, mi, s, 0)
        .to_zoned(zone.clone())
        .unwrap()
}

fn local_ms(y: i16, mo: i8, d: i8, h: i8, mi: i8, s: i8) -> i64 {
    local(y, mo, d, h, mi, s).timestamp().as_millisecond()
}

fn lunch(days: &[WeekdayKey]) -> Alarm {
    Alarm {
        label: "Lunch".into(),
        hour: 12,
        minute: 0,
        repeat: repeat_mask(&days.iter().copied().collect()),
        enabled: true,
        enabled_at_ms: local_ms(2026, 1, 1, 0, 0, 0),
    }
}

fn with_alarm(alarm: Alarm) -> (Clocks, Id) {
    let mut clocks = Clocks::default();
    let id = Id::from_raw("alarm1");
    clocks.alarms.push(id.clone(), alarm);
    (clocks, id)
}

fn at(when: jiff::Zoned) -> (jiff::Timestamp, jiff::Zoned) {
    (when.timestamp(), when)
}

fn restarted(delivery: &Delivery) -> Delivery {
    serde_json::from_value(serde_json::to_value(delivery).unwrap()).unwrap()
}

fn ring(clocks: &Clocks, delivery: &mut Delivery, when: &jiff::Zoned) -> Tick {
    let (utc, local) = at(when.clone());
    tick(clocks, delivery, utc, &local)
}

fn running_timer(label: &str, ends_at: jiff::Timestamp) -> Timer {
    Timer {
        label: label.into(),
        duration_ms: 60_000,
        ends_at_ms: Some(ends_at.as_millisecond()),
        paused_remaining_ms: None,
    }
}

#[test]
fn timers_ring_once_per_deadline_across_restart_and_native_handoff() {
    let start = jiff::Timestamp::from_second(jiff::Timestamp::now().as_second()).unwrap();
    let end = start + jiff::SignedDuration::from_secs(30);
    let mut clocks = Clocks::default();
    let id = Id::from_raw("timer1");
    clocks.timers.push(id.clone(), running_timer("Tea", end));
    let mut delivery = Delivery::default();

    // Case: a running timer counts down by deadline, so suspend cannot stall it.
    let timer = clocks.timers.get(&id).unwrap();
    assert_eq!(
        timer.remaining(start + jiff::SignedDuration::from_secs(12)),
        Duration::from_secs(18),
        "case suspend-by-deadline"
    );
    assert!(timer.is_running(start), "running before its deadline");

    // Case: nothing rings before the deadline.
    assert!(
        tick(&clocks, &mut delivery, start, &jiff::Zoned::now())
            .due
            .is_empty(),
        "before the deadline"
    );

    // Case: the platform accepted the deadline, so the app does not show it
    // again, and a restart keeps the handoff.
    delivery
        .native_scheduled
        .insert("timer1".into(), end.as_millisecond());
    let mut reopened = restarted(&delivery);
    let later = end + jiff::SignedDuration::from_secs(1);
    assert!(
        matches!(
            tick(&clocks, &mut reopened, later, &jiff::Zoned::now())
                .due
                .as_slice(),
            [DueEvent::Timer {
                native_scheduled: true,
                ..
            }]
        ),
        "case native-handoff: the scheduled deadline is the platform's"
    );
    assert!(
        reopened.native_scheduled.is_empty(),
        "case native-handoff: consumed"
    );
    assert!(
        tick(
            &clocks,
            &mut restarted(&reopened),
            later,
            &jiff::Zoned::now()
        )
        .due
        .is_empty(),
        "case restart: a rung deadline does not ring again after a restart"
    );
    assert!(
        clocks.timers.get(&id).unwrap().is_finished(later),
        "a timer past its deadline is finished"
    );

    // Case: a timer restarted with a new deadline rings again, and not as
    // the platform's.
    let restarted_end = later + jiff::SignedDuration::from_secs(5);
    clocks.timers.get_mut(&id).unwrap().ends_at_ms = Some(restarted_end.as_millisecond());
    reopened
        .native_scheduled
        .insert("timer1".into(), end.as_millisecond());
    assert!(
        matches!(
            tick(
                &clocks,
                &mut reopened,
                restarted_end + jiff::SignedDuration::from_secs(1),
                &jiff::Zoned::now()
            )
            .due
            .as_slice(),
            [DueEvent::Timer {
                native_scheduled: false,
                ..
            }]
        ),
        "case changed-deadline: rings itself"
    );

    // Case: a removed timer leaves the log.
    clocks.timers.remove(&id);
    tick(&clocks, &mut reopened, restarted_end, &jiff::Zoned::now());
    assert!(reopened.timers_rung.is_empty(), "removed timer forgotten");
}

#[test]
fn shell_activity_shows_the_floating_clock_first() {
    let now = jiff::Timestamp::now();

    // Case: a paused floating timer stays in the shell, marked resumable.
    {
        let mut clocks = Clocks::default();
        clocks.timers.push(
            Id::from_raw("tea"),
            Timer {
                label: "Tea".into(),
                duration_ms: 90_000,
                ends_at_ms: None,
                paused_remaining_ms: Some(90_000),
            },
        );
        let device = OnThisDevice {
            floating_timers: BTreeSet::from(["tea".to_owned()]),
            ..OnThisDevice::default()
        };
        let activity = shell_activity(&clocks, &device, now).unwrap();
        assert_eq!(activity.remaining, Duration::from_secs(90), "paused: left");
        assert!(activity.overlay, "paused: floats");
        assert!(activity.paused, "paused: resumable");
        assert!(!activity.stopwatch, "paused: a timer");
        assert!(
            shell_activity(&clocks, &OnThisDevice::default(), now).is_none(),
            "a paused timer that does not float is not shown"
        );
    }

    // Case: a paused floating stopwatch stays in the shell.
    {
        let mut clocks = Clocks::default();
        clocks.stopwatch.accumulated_ms = 12_000;
        let device = OnThisDevice {
            floating_stopwatch: true,
            ..OnThisDevice::default()
        };
        let activity = shell_activity(&clocks, &device, now).unwrap();
        assert_eq!(
            activity.remaining,
            Duration::from_secs(12),
            "stopwatch: elapsed"
        );
        assert!(activity.paused, "stopwatch: paused");
        assert!(activity.stopwatch, "stopwatch: the stopwatch");
        assert_eq!(activity.activity_id, None, "stopwatch: no timer id");
    }

    // Case: the floated timer owns the shell over an urgent one that does
    // not float, and only running timers otherwise show.
    {
        let mut clocks = Clocks::default();
        clocks.timers.push(
            Id::from_raw("eggs"),
            running_timer("Eggs", now + jiff::SignedDuration::from_secs(30)),
        );
        clocks.timers.push(
            Id::from_raw("focus"),
            running_timer("Focus", now + jiff::SignedDuration::from_secs(1_500)),
        );
        clocks.timers.push(
            Id::from_raw("done"),
            running_timer("Done", now - jiff::SignedDuration::from_secs(5)),
        );
        let device = OnThisDevice {
            floating_timers: BTreeSet::from(["focus".to_owned()]),
            ..OnThisDevice::default()
        };
        let activity = shell_activity(&clocks, &device, now).unwrap();
        assert_eq!(activity.activity_id.as_deref(), Some("focus"));
        assert_eq!(activity.label, "Focus");
        assert!(activity.overlay);
        let unfloated = shell_activity(&clocks, &OnThisDevice::default(), now).unwrap();
        assert_eq!(
            unfloated.activity_id.as_deref(),
            Some("eggs"),
            "without a floating timer the most urgent running one shows"
        );
        assert!(!unfloated.overlay);
    }
}

tilo_test_env::scenario! {
    fn an_alarm_rings_once_at_each_occurrence_it_is_due;

    phase alarms_ring_once_per_device_and_survive_restarts;
    {
        // Phase: a new alarm for a past time waits until tomorrow; a one-shot
        // alarm turns off once it rang.
        {
            let (mut clocks, id) = with_alarm(lunch(&[]));
            let mut delivery = Delivery::default();
            assert!(
                ring(&clocks, &mut delivery, &local(2026, 1, 15, 12, 1, 0))
                    .due
                    .is_empty(),
                "past time: nothing due at arming"
            );
            assert!(
                ring(&clocks, &mut delivery, &local(2026, 1, 16, 11, 59, 59))
                    .due
                    .is_empty(),
                "past time: nothing due a second before tomorrow's deadline"
            );
            let rang = ring(&clocks, &mut delivery, &local(2026, 1, 16, 12, 0, 0));
            assert!(
                matches!(rang.due.as_slice(), [DueEvent::Alarm { label, .. }] if label == "Lunch"),
                "past time: rings at tomorrow's deadline"
            );
            assert_eq!(
                rang.finished_alarms,
                vec![id.clone()],
                "past time: a one-shot alarm turns off"
            );
            assert_eq!(
                ring(&clocks, &mut delivery, &local(2026, 1, 16, 12, 0, 0)),
                Tick::default(),
                "past time: rings only once, even before the turn-off arrives"
            );
            clocks.alarms.get_mut(&id).unwrap().enabled = false;
            assert!(
                ring(&clocks, &mut delivery, &local(2026, 1, 17, 12, 0, 0))
                    .due
                    .is_empty(),
                "past time: an alarm turned off stays quiet"
            );
        }

        // Phase: a pending alarm survives a restart on another day.
        {
            let (clocks, _) = with_alarm(lunch(&[]));
            let mut delivery = Delivery::default();
            assert!(
                ring(&clocks, &mut delivery, &local(2026, 1, 15, 11, 0, 0))
                    .due
                    .is_empty(),
                "restart: nothing due at arming"
            );
            let mut reopened = restarted(&delivery);
            assert!(
                matches!(
                    ring(&clocks, &mut reopened, &local(2026, 1, 16, 8, 0, 0)).due.as_slice(),
                    [DueEvent::Alarm { scheduled_for, .. }]
                        if *scheduled_for == local(2026, 1, 15, 12, 0, 0).timestamp()
                ),
                "restart: the missed alarm rings after reopening"
            );
            assert_eq!(
                reopened.alarms["alarm1"].last_rung,
                Some(jiff::civil::Date::new(2026, 1, 15).unwrap()),
                "restart: records the scheduled date"
            );
        }

        // Phase: a recurring alarm catches up once and arms the next weekday.
        {
            let (clocks, _) = with_alarm(lunch(&[WeekdayKey::Thu]));
            let mut delivery = Delivery::default();
            assert!(
                ring(&clocks, &mut delivery, &local(2026, 1, 15, 11, 0, 0))
                    .due
                    .is_empty(),
                "recurring: nothing due at arming"
            );
            let reopened_at = local(2026, 1, 23, 8, 0, 0);
            let rang = ring(&clocks, &mut delivery, &reopened_at);
            assert!(
                matches!(rang.due.as_slice(), [DueEvent::Alarm { scheduled_for, .. }]
                    if *scheduled_for == local(2026, 1, 15, 12, 0, 0).timestamp()),
                "recurring: exactly one catch-up for the first missed occurrence"
            );
            assert!(
                rang.finished_alarms.is_empty(),
                "recurring: a repeating alarm stays on"
            );
            assert_eq!(
                delivery.alarms["alarm1"].next_due_ms,
                Some(local_ms(2026, 1, 29, 12, 0, 0)),
                "recurring: next Thursday after the reopen"
            );
            let mut reopened = restarted(&delivery);
            assert!(
                ring(&clocks, &mut reopened, &reopened_at).due.is_empty(),
                "recurring: the reopened log does not repeat the catch-up"
            );
            let next = local(2026, 1, 29, 12, 0, 0);
            assert_eq!(
                ring(&clocks, &mut reopened, &next).due.len(),
                1,
                "recurring: rings on the armed weekday"
            );
            assert!(
                ring(&clocks, &mut reopened, &next).due.is_empty(),
                "recurring: rings only once on that weekday"
            );
        }

        // Phase: turning an alarm off and on again (here, or on another device
        // while this one was closed) discards the deadline armed before.
        {
            let (mut clocks, id) = with_alarm(lunch(&[]));
            let mut delivery = Delivery::default();
            ring(&clocks, &mut delivery, &local(2026, 1, 15, 11, 0, 0));
            let alarm = clocks.alarms.get_mut(&id).unwrap();
            alarm.enabled_at_ms = local_ms(2026, 1, 15, 12, 0, 30);
            assert!(
                ring(&clocks, &mut delivery, &local(2026, 1, 15, 12, 1, 0))
                    .due
                    .is_empty(),
                "toggle: the stale deadline does not ring"
            );
            assert_eq!(
                delivery.alarms["alarm1"].next_due_ms,
                Some(local_ms(2026, 1, 16, 12, 0, 0)),
                "toggle: re-armed for tomorrow"
            );
            clocks.alarms.get_mut(&id).unwrap().enabled = false;
            ring(&clocks, &mut delivery, &local(2026, 1, 15, 12, 2, 0));
            assert!(
                delivery.alarms.is_empty(),
                "toggle: an alarm turned off holds no deadline"
            );
        }

        // Phase: an alarm moved to another time on another device is armed
        // afresh, and a removed alarm leaves the log.
        {
            let (mut clocks, id) = with_alarm(lunch(&[]));
            let mut delivery = Delivery::default();
            ring(&clocks, &mut delivery, &local(2026, 1, 15, 11, 0, 0));
            clocks.alarms.get_mut(&id).unwrap().hour = 13;
            assert!(
                ring(&clocks, &mut delivery, &local(2026, 1, 15, 12, 30, 0))
                    .due
                    .is_empty(),
                "moved: the old time does not ring"
            );
            assert_eq!(
                delivery.alarms["alarm1"].next_due_ms,
                Some(local_ms(2026, 1, 15, 13, 0, 0)),
                "moved: armed for the new time"
            );
            clocks.alarms.remove(&id);
            ring(&clocks, &mut delivery, &local(2026, 1, 15, 12, 31, 0));
            assert!(delivery.alarms.is_empty(), "removed: log entry dropped");
        }
    }

    phase next_occurrence_honors_time_weekdays_history_and_dst;
    {
        // Case: keeps the requested time after today's occurrence.
        assert_eq!(
            next_occurrence(&lunch(&[]), None, &local(2026, 1, 15, 12, 1, 0)),
            Some(local(2026, 1, 16, 12, 0, 0)),
            "case after-todays-occurrence"
        );

        // Case: does not repeat a rung date after a clock rollback.
        let after = local(2026, 1, 15, 11, 0, 0);
        assert_eq!(
            next_occurrence(&lunch(&[WeekdayKey::Thu]), Some(after.date()), &after),
            Some(local(2026, 1, 22, 12, 0, 0)),
            "case clock-rollback"
        );

        // Case: resolves DST gaps and never repeats the autumn hour.
        let zone = tilo_recurrence::zone::named("Europe/Berlin").unwrap();
        let wake = Alarm {
            label: "Wake".into(),
            hour: 2,
            minute: 30,
            enabled: true,
            ..Alarm::default()
        };
        let spring = in_zone(&zone, 2026, 3, 29, 1, 0, 0);
        assert_eq!(
            next_occurrence(&wake, None, &spring),
            Some(in_zone(&zone, 2026, 3, 29, 3, 0, 0)),
            "case dst-spring-gap"
        );
        let autumn = in_zone(&zone, 2026, 10, 25, 1, 0, 0);
        let first = next_occurrence(&wake, None, &autumn).unwrap();
        assert_eq!(
            first.timestamp(),
            in_zone(&jiff::tz::TimeZone::UTC, 2026, 10, 25, 0, 30, 0).timestamp(),
            "case dst-autumn-first-occurrence"
        );
        assert_eq!(
            next_occurrence(&wake, None, &first),
            Some(in_zone(&zone, 2026, 10, 26, 2, 30, 0)),
            "case dst-autumn-no-repeat"
        );

        // Case: honors turned-off and weekday settings.
        let after = in_zone(&jiff::tz::TimeZone::UTC, 2026, 1, 15, 11, 0, 0);
        let mut weekend = Alarm {
            label: "Weekend".into(),
            hour: 12,
            repeat: repeat_mask(&[WeekdayKey::Sat].into_iter().collect()),
            ..Alarm::default()
        };
        assert_eq!(
            next_occurrence(&weekend, None, &after),
            None,
            "case disabled"
        );
        weekend.enabled = true;
        assert_eq!(
            next_occurrence(&weekend, None, &after),
            Some(in_zone(&jiff::tz::TimeZone::UTC, 2026, 1, 17, 12, 0, 0)),
            "case weekday-only"
        );
        assert_eq!(
            weekend.local_time(),
            Time::new(12, 0, 0, 0).unwrap()
        );
    }

    phase completed_one_shot_does_not_rearm_if_shared_settings_remain_enabled;
    {
        let (clocks, _) = with_alarm(lunch(&[]));
        let mut delivery = Delivery::default();
        assert!(
            ring(&clocks, &mut delivery, &local(2026, 4, 14, 11, 0, 0))
                .due
                .is_empty()
        );
        assert_eq!(
            ring(&clocks, &mut delivery, &local(2026, 4, 14, 12, 0, 0))
                .due
                .len(),
            1
        );
        assert!(
            ring(&clocks, &mut restarted(&delivery), &local(2026, 4, 15, 12, 0, 0))
            .due
            .is_empty(),
            "a read-only member still delivers a one-shot only once per device"
        );
    }

    phase snoozing_rings_the_same_alarm_once_more_and_survives_a_restart;
    {
        let (clocks, id) = with_alarm(lunch(&[]));
        let mut delivery = Delivery::default();
        let noon = local(2026, 1, 15, 12, 0, 0);
        assert!(
            ring(&clocks, &mut delivery, &local(2026, 1, 15, 11, 0, 0))
                .due
                .is_empty()
        );
        assert_eq!(ring(&clocks, &mut delivery, &noon).due.len(), 1);
        let alarm = clocks.alarms.get(&id).unwrap();
        snooze(&mut delivery, &id, alarm, noon.timestamp().as_millisecond(), 5).unwrap();
        assert!(
            ring(&clocks, &mut delivery, &local(2026, 1, 15, 12, 4, 59))
                .due
                .is_empty(),
            "a snooze waits its five minutes"
        );
        let mut reopened = restarted(&delivery);
        let again = ring(&clocks, &mut reopened, &local(2026, 1, 15, 12, 5, 0));
        assert!(
            matches!(again.due.as_slice(), [DueEvent::Alarm { label, .. }] if label == "Lunch"),
            "a snooze rings once after being kept across a restart"
        );
        assert!(
            ring(&clocks, &mut reopened, &local(2026, 1, 15, 12, 5, 1))
                .due
                .is_empty(),
            "the snoozed occurrence rings once"
        );
    }

    phase editing_or_removing_an_alarm_cancels_its_snooze;
    {
        let (mut clocks, id) = with_alarm(lunch(&[]));
        let mut delivery = Delivery::default();
        let noon = local(2026, 1, 15, 12, 0, 0);
        ring(&clocks, &mut delivery, &local(2026, 1, 15, 11, 0, 0));
        ring(&clocks, &mut delivery, &noon);
        let alarm = clocks.alarms.get(&id).unwrap();
        snooze(&mut delivery, &id, alarm, noon.timestamp().as_millisecond(), 5).unwrap();

        let alarm = clocks.alarms.get_mut(&id).unwrap();
        alarm.minute = 30;
        assert!(
            ring(&clocks, &mut delivery, &local(2026, 1, 15, 12, 5, 0))
                .due
                .is_empty(),
            "moving the alarm cancels the occurrence snoozed under the old time"
        );

        let alarm = clocks.alarms.get(&id).unwrap();
        snooze(&mut delivery, &id, alarm, noon.timestamp().as_millisecond(), 5).unwrap();
        clocks.alarms.remove(&id);
        assert!(
            ring(&clocks, &mut delivery, &local(2026, 1, 15, 12, 5, 0))
                .due
                .is_empty(),
            "a deleted alarm has no snooze"
        );
    }
}
