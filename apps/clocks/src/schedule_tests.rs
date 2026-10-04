#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::*;
use crate::model::{Timer, WeekdayKey, repeat_mask};
use chrono::{NaiveTime, Timelike};

fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Local> {
    Local.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
}

fn local_ms(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
    local(y, mo, d, h, mi, s).timestamp_millis()
}

/// A 12:00 "Lunch" alarm turned on long before any case's clock.
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

fn at(when: DateTime<Local>) -> (DateTime<Utc>, DateTime<Local>) {
    (when.with_timezone(&Utc), when)
}

/// The ledger as a device reads it back after a restart.
fn restarted(delivery: &Delivery) -> Delivery {
    serde_json::from_value(serde_json::to_value(delivery).unwrap()).unwrap()
}

fn ring(clocks: &Clocks, delivery: &mut Delivery, when: DateTime<Local>) -> Tick {
    let (utc, local) = at(when);
    tick(clocks, delivery, utc, local)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn alarms_ring_once_per_device_and_survive_restarts() {
    // Phase: a new alarm for a past time waits until tomorrow; a one-shot
    // alarm turns off once it rang.
    {
        let (mut clocks, id) = with_alarm(lunch(&[]));
        let mut delivery = Delivery::default();
        assert!(
            ring(&clocks, &mut delivery, local(2026, 1, 15, 12, 1, 0))
                .due
                .is_empty(),
            "past time: nothing due at arming"
        );
        assert!(
            ring(&clocks, &mut delivery, local(2026, 1, 16, 11, 59, 59))
                .due
                .is_empty(),
            "past time: nothing due a second before tomorrow's deadline"
        );
        let rang = ring(&clocks, &mut delivery, local(2026, 1, 16, 12, 0, 0));
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
            ring(&clocks, &mut delivery, local(2026, 1, 16, 12, 0, 0)),
            Tick::default(),
            "past time: rings only once, even before the turn-off arrives"
        );
        clocks.alarms.get_mut(&id).unwrap().enabled = false;
        assert!(
            ring(&clocks, &mut delivery, local(2026, 1, 17, 12, 0, 0))
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
            ring(&clocks, &mut delivery, local(2026, 1, 15, 11, 0, 0))
                .due
                .is_empty(),
            "restart: nothing due at arming"
        );
        let mut reopened = restarted(&delivery);
        assert!(
            matches!(
                ring(&clocks, &mut reopened, local(2026, 1, 16, 8, 0, 0)).due.as_slice(),
                [DueEvent::Alarm { scheduled_for, .. }]
                    if *scheduled_for == local(2026, 1, 15, 12, 0, 0).with_timezone(&Utc)
            ),
            "restart: the missed alarm rings after reopening"
        );
        assert_eq!(
            reopened.alarms["alarm1"].last_rung,
            Some(chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap()),
            "restart: records the scheduled date"
        );
    }

    // Phase: a recurring alarm catches up once and arms the next weekday.
    {
        let (clocks, _) = with_alarm(lunch(&[WeekdayKey::Thu]));
        let mut delivery = Delivery::default();
        assert!(
            ring(&clocks, &mut delivery, local(2026, 1, 15, 11, 0, 0))
                .due
                .is_empty(),
            "recurring: nothing due at arming"
        );
        let reopened_at = local(2026, 1, 23, 8, 0, 0);
        let rang = ring(&clocks, &mut delivery, reopened_at);
        assert!(
            matches!(rang.due.as_slice(), [DueEvent::Alarm { scheduled_for, .. }]
                if *scheduled_for == local(2026, 1, 15, 12, 0, 0).with_timezone(&Utc)),
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
            ring(&clocks, &mut reopened, reopened_at).due.is_empty(),
            "recurring: the reopened ledger does not repeat the catch-up"
        );
        let next = local(2026, 1, 29, 12, 0, 0);
        assert_eq!(
            ring(&clocks, &mut reopened, next).due.len(),
            1,
            "recurring: rings on the armed weekday"
        );
        assert!(
            ring(&clocks, &mut reopened, next).due.is_empty(),
            "recurring: rings only once on that weekday"
        );
    }

    // Phase: turning an alarm off and on again (here, or on another device
    // while this one was closed) discards the deadline armed before.
    {
        let (mut clocks, id) = with_alarm(lunch(&[]));
        let mut delivery = Delivery::default();
        ring(&clocks, &mut delivery, local(2026, 1, 15, 11, 0, 0));
        let alarm = clocks.alarms.get_mut(&id).unwrap();
        alarm.enabled_at_ms = local_ms(2026, 1, 15, 12, 0, 30);
        assert!(
            ring(&clocks, &mut delivery, local(2026, 1, 15, 12, 1, 0))
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
        ring(&clocks, &mut delivery, local(2026, 1, 15, 12, 2, 0));
        assert!(
            delivery.alarms.is_empty(),
            "toggle: an alarm turned off holds no deadline"
        );
    }

    // Phase: an alarm moved to another time on another device is armed
    // afresh, and a removed alarm leaves the ledger.
    {
        let (mut clocks, id) = with_alarm(lunch(&[]));
        let mut delivery = Delivery::default();
        ring(&clocks, &mut delivery, local(2026, 1, 15, 11, 0, 0));
        clocks.alarms.get_mut(&id).unwrap().hour = 13;
        assert!(
            ring(&clocks, &mut delivery, local(2026, 1, 15, 12, 30, 0))
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
        ring(&clocks, &mut delivery, local(2026, 1, 15, 12, 31, 0));
        assert!(delivery.alarms.is_empty(), "removed: ledger entry dropped");
    }
}

#[test]
fn next_occurrence_honors_time_weekdays_history_and_dst() {
    // Case: keeps the requested time after today's occurrence.
    assert_eq!(
        next_occurrence(&lunch(&[]), None, local(2026, 1, 15, 12, 1, 0)),
        Some(local(2026, 1, 16, 12, 0, 0)),
        "case after-todays-occurrence"
    );

    // Case: does not repeat a rung date after a clock rollback.
    let after = local(2026, 1, 15, 11, 0, 0);
    assert_eq!(
        next_occurrence(&lunch(&[WeekdayKey::Thu]), Some(after.date_naive()), after),
        Some(local(2026, 1, 22, 12, 0, 0)),
        "case clock-rollback"
    );

    // Case: resolves DST gaps and never repeats the autumn hour.
    let zone = chrono_tz::Europe::Berlin;
    let wake = Alarm {
        label: "Wake".into(),
        hour: 2,
        minute: 30,
        enabled: true,
        ..Alarm::default()
    };
    let spring = zone.with_ymd_and_hms(2026, 3, 29, 1, 0, 0).unwrap();
    assert_eq!(
        next_occurrence_in(&wake, None, &spring),
        Some(zone.with_ymd_and_hms(2026, 3, 29, 3, 0, 0).unwrap()),
        "case dst-spring-gap"
    );
    let autumn = zone.with_ymd_and_hms(2026, 10, 25, 1, 0, 0).unwrap();
    let first = next_occurrence_in(&wake, None, &autumn).unwrap();
    assert_eq!(
        first.with_timezone(&Utc),
        Utc.with_ymd_and_hms(2026, 10, 25, 0, 30, 0).unwrap(),
        "case dst-autumn-first-occurrence"
    );
    assert_eq!(
        next_occurrence_in(&wake, None, &first),
        Some(zone.with_ymd_and_hms(2026, 10, 26, 2, 30, 0).unwrap()),
        "case dst-autumn-no-repeat"
    );

    // Case: honors turned-off and weekday settings.
    let after = Utc.with_ymd_and_hms(2026, 1, 15, 11, 0, 0).unwrap();
    let mut weekend = Alarm {
        label: "Weekend".into(),
        hour: 12,
        repeat: repeat_mask(&[WeekdayKey::Sat].into_iter().collect()),
        ..Alarm::default()
    };
    assert_eq!(
        next_occurrence_in(&weekend, None, &after),
        None,
        "case disabled"
    );
    weekend.enabled = true;
    assert_eq!(
        next_occurrence_in(&weekend, None, &after),
        Some(Utc.with_ymd_and_hms(2026, 1, 17, 12, 0, 0).unwrap()),
        "case weekday-only"
    );
    assert_eq!(
        weekend.local_time(),
        NaiveTime::from_hms_opt(12, 0, 0).unwrap()
    );
}

fn running_timer(label: &str, ends_at: DateTime<Utc>) -> Timer {
    Timer {
        label: label.into(),
        duration_ms: 60_000,
        ends_at_ms: Some(ends_at.timestamp_millis()),
        paused_remaining_ms: None,
    }
}

#[test]
fn timers_ring_once_per_deadline_across_restart_and_native_handoff() {
    let start = Utc::now().with_nanosecond(0).unwrap();
    let end = start + chrono::Duration::seconds(30);
    let mut clocks = Clocks::default();
    let id = Id::from_raw("timer1");
    clocks.timers.push(id.clone(), running_timer("Tea", end));
    let mut delivery = Delivery::default();

    // Case: a running timer counts down by deadline, so suspend cannot stall it.
    let timer = clocks.timers.get(&id).unwrap();
    assert_eq!(
        timer.remaining(start + chrono::Duration::seconds(12)),
        Duration::from_secs(18),
        "case suspend-by-deadline"
    );
    assert!(timer.is_running(start), "running before its deadline");

    // Case: nothing rings before the deadline.
    assert!(
        tick(&clocks, &mut delivery, start, Local::now())
            .due
            .is_empty(),
        "before the deadline"
    );

    // Case: the platform accepted the deadline, so the app does not show it
    // again, and a restart keeps the handoff.
    delivery
        .native_scheduled
        .insert("timer1".into(), end.timestamp_millis());
    let mut reopened = restarted(&delivery);
    let later = end + chrono::Duration::seconds(1);
    assert!(
        matches!(
            tick(&clocks, &mut reopened, later, Local::now())
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
        tick(&clocks, &mut restarted(&reopened), later, Local::now())
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
    let restarted_end = later + chrono::Duration::seconds(5);
    clocks.timers.get_mut(&id).unwrap().ends_at_ms = Some(restarted_end.timestamp_millis());
    reopened
        .native_scheduled
        .insert("timer1".into(), end.timestamp_millis());
    assert!(
        matches!(
            tick(
                &clocks,
                &mut reopened,
                restarted_end + chrono::Duration::seconds(1),
                Local::now()
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

    // Case: a removed timer leaves the ledger.
    clocks.timers.remove(&id);
    tick(&clocks, &mut reopened, restarted_end, Local::now());
    assert!(reopened.timers_rung.is_empty(), "removed timer forgotten");
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn shell_activity_shows_the_floating_clock_first() {
    let now = Utc::now();

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
            running_timer("Eggs", now + chrono::Duration::seconds(30)),
        );
        clocks.timers.push(
            Id::from_raw("focus"),
            running_timer("Focus", now + chrono::Duration::seconds(1_500)),
        );
        clocks.timers.push(
            Id::from_raw("done"),
            running_timer("Done", now - chrono::Duration::seconds(5)),
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
