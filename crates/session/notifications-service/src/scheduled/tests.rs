use chrono::FixedOffset;

use super::*;

fn at(value: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(value).expect("valid test instant")
}

fn owner(room: &str, app: &str) -> Owner {
    Owner {
        app: app.into(),
        room: room.into(),
    }
}

fn alarm(tag: &str, due: DateTime<FixedOffset>, weekdays: u8) -> ScheduleRequest {
    ScheduleRequest {
        tag: tag.into(),
        title: "Alarm".into(),
        body: "Wake up".into(),
        activation: "tilo-clocks:alarms".into(),
        at_ms: due.timestamp_millis(),
        alarm: Some(AlarmRequest {
            hour: 12,
            minute: 0,
            weekdays,
            enabled_at_ms: 1,
            last_rung: None,
        }),
    }
}

fn ids() -> impl FnMut() -> u32 {
    let mut next = 0_u32;
    move || {
        next = next.wrapping_add(1);
        next
    }
}

/// An alarm scheduled and rung once at `due`: the ledger and the ringing
/// occurrence.
fn rung(due: DateTime<FixedOffset>, weekdays: u8, path: Option<PathBuf>) -> (Ledger, Occurrence) {
    let mut ledger = Ledger::open(path).expect("ledger");
    ledger
        .schedule(
            &owner("room", "so.tilo.clocks"),
            alarm("alarm", due, weekdays),
        )
        .expect("handoff");
    let ringing = ledger.take_due(&due, ids()).expect("rung");
    let [notification] = ringing.as_slice() else {
        panic!("one ring, not {}", ringing.len());
    };
    assert_eq!(
        notification
            .actions
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        [DISMISS, SNOOZE]
    );
    let occurrence = notification.scheduled.clone().expect("its deadline");
    assert!(occurrence.calendar_alarm);
    assert_eq!(occurrence.occurrence_ms, due.timestamp_millis());
    (ledger, occurrence)
}

#[test]
fn the_wire_arguments_make_a_request() {
    let request = ScheduleRequest::from_wire((
        "t".into(),
        "Title".into(),
        String::new(),
        String::new(),
        5,
        vec![(7, 30, 1, 2, "2026-04-14".into())],
    ))
    .expect("request");
    assert_eq!(
        request.alarm.and_then(|alarm| alarm.last_rung),
        NaiveDate::from_ymd_opt(2026, 4, 14)
    );
    assert!(
        ScheduleRequest::from_wire((
            "t".into(),
            "T".into(),
            String::new(),
            String::new(),
            5,
            vec![(7, 30, 1, 2, String::new()), (8, 0, 0, 0, String::new())],
        ))
        .is_err()
    );
}

#[test]
fn a_snooze_survives_restart_and_rings_once() {
    let temp = tempfile::tempdir().expect("private fixture");
    let path = temp.path().join("schedules.json");
    let due = at("2026-04-14T12:00:00+00:00");
    let (mut ledger, occurrence) = rung(due, 127, Some(path.clone()));
    let original = ledger.alarm_states(&owner("room", "so.tilo.clocks"));
    ledger
        .act(
            "so.tilo.clocks",
            &occurrence,
            AlarmAction::Snooze,
            due.timestamp_millis(),
        )
        .expect("snooze");
    drop(ledger);
    let mut restarted = Ledger::open(Some(path)).expect("restart");
    let snoozed = due + chrono::Duration::minutes(5);
    assert!(
        restarted
            .take_due(&(snoozed - chrono::Duration::seconds(1)), ids())
            .expect("before")
            .is_empty()
    );
    assert_eq!(restarted.take_due(&snoozed, ids()).expect("rings").len(), 1);
    assert!(
        restarted
            .take_due(&(snoozed + chrono::Duration::hours(1)), ids())
            .expect("no duplicate")
            .is_empty()
    );
    assert_eq!(
        restarted.alarm_states(&owner("room", "so.tilo.clocks")),
        original,
        "a snooze does not rewrite the calendar's delivery history"
    );
    assert!(
        restarted
            .act(
                "so.tilo.clocks",
                &occurrence,
                AlarmAction::Snooze,
                snoozed.timestamp_millis()
            )
            .is_err(),
        "an old card cannot snooze the new ring"
    );
}

#[test]
fn duplicate_snooze_and_dismiss_are_durable_and_cancel_pending_snooze() {
    let due = at("2026-04-14T12:00:00+00:00");
    let temp = tempfile::tempdir().expect("private fixture");
    let path = temp.path().join("schedules.json");
    let (mut ledger, occurrence) = rung(due, 0, Some(path.clone()));
    let app = "so.tilo.clocks";
    ledger
        .act(
            app,
            &occurrence,
            AlarmAction::Snooze,
            due.timestamp_millis(),
        )
        .expect("snooze");
    let deadline = ledger.next_at_ms();
    ledger
        .act(
            app,
            &occurrence,
            AlarmAction::Snooze,
            (due + chrono::Duration::minutes(2)).timestamp_millis(),
        )
        .expect("idempotent retry");
    assert_eq!(
        ledger.next_at_ms(),
        deadline,
        "a retry cannot extend a snooze"
    );
    ledger
        .act(
            app,
            &occurrence,
            AlarmAction::Dismiss,
            due.timestamp_millis(),
        )
        .expect("dismiss the pending snooze");
    ledger
        .act(
            app,
            &occurrence,
            AlarmAction::Dismiss,
            due.timestamp_millis(),
        )
        .expect("idempotent dismiss");
    assert!(ledger.next_at_ms().is_none());
    assert!(
        ledger
            .act(
                app,
                &occurrence,
                AlarmAction::Snooze,
                due.timestamp_millis()
            )
            .is_err()
    );
    drop(ledger);
    let mut restarted = Ledger::open(Some(path)).expect("restart after dismissal");
    assert!(restarted.next_at_ms().is_none());
    assert!(
        restarted
            .take_due(&(due + chrono::Duration::hours(1)), ids())
            .expect("after the cancelled snooze")
            .is_empty()
    );
}

#[test]
fn cancelling_a_snoozed_alarm_survives_restart_and_a_stale_dismiss_cannot_rearm_it() {
    let temp = tempfile::tempdir().expect("private fixture");
    let path = temp.path().join("schedules.json");
    let due = at("2026-04-14T12:00:00+00:00");
    let (mut ledger, occurrence) = rung(due, 0, Some(path.clone()));
    let app = "so.tilo.clocks";
    ledger
        .act(
            app,
            &occurrence,
            AlarmAction::Snooze,
            due.timestamp_millis(),
        )
        .expect("snooze");
    ledger.cancel(&owner("room", app), "alarm").expect("cancel");
    drop(ledger);
    let mut restarted = Ledger::open(Some(path)).expect("restart");
    assert!(
        restarted
            .act(
                app,
                &occurrence,
                AlarmAction::Snooze,
                due.timestamp_millis()
            )
            .is_err()
    );
    restarted
        .act(
            app,
            &occurrence,
            AlarmAction::Dismiss,
            due.timestamp_millis(),
        )
        .expect("a stale card's dismissal");
    assert!(
        restarted
            .take_due(&(due + chrono::Duration::hours(1)), ids())
            .expect("past the cancelled deadline")
            .is_empty()
    );
    assert!(restarted.pending(&owner("room", app)).is_empty());
}

#[test]
fn foreign_app_room_token_and_stale_occurrence_cannot_change_an_alarm() {
    let due = at("2026-04-14T12:00:00+00:00");
    let (mut ledger, occurrence) = rung(due, 127, None);
    let original = ledger.alarm_states(&owner("room", "so.tilo.clocks"));
    for changed in 0..4 {
        let mut target = occurrence.clone();
        let mut app = "so.tilo.clocks";
        match changed {
            0 => app = "another.app",
            1 => target.room = "another-room".into(),
            2 => target.token = "0123456789abcdef0123456789abcdef".into(),
            _ => target.occurrence_ms -= 1,
        }
        assert!(
            ledger
                .act(app, &target, AlarmAction::Snooze, due.timestamp_millis())
                .is_err()
        );
    }
    assert_eq!(
        ledger.alarm_states(&owner("room", "so.tilo.clocks")),
        original
    );
}

#[test]
fn snooze_stays_in_utc_across_time_zone_and_wall_clock_changes() {
    let due = at("2026-04-14T12:00:00+00:00");
    let (mut ledger, occurrence) = rung(due, 127, None);
    ledger
        .act(
            "so.tilo.clocks",
            &occurrence,
            AlarmAction::Snooze,
            due.timestamp_millis(),
        )
        .expect("snooze");
    let expected = (due + chrono::Duration::minutes(5)).timestamp_millis();
    assert_eq!(ledger.next_at_ms(), Some(expected));
    assert!(
        ledger
            .take_due(&at("2026-04-14T21:03:00+09:00"), ids())
            .expect("zone")
            .is_empty()
    );
    assert_eq!(ledger.next_at_ms(), Some(expected));
    assert!(
        ledger
            .take_due(&at("2026-04-14T20:03:00+09:00"), ids())
            .expect("back")
            .is_empty()
    );
    assert_eq!(ledger.next_at_ms(), Some(expected));
    let forward = due + chrono::Duration::minutes(7);
    assert_eq!(ledger.take_due(&forward, ids()).expect("forward").len(), 1);
    assert!(
        ledger
            .take_due(&forward, ids())
            .expect("same clock")
            .is_empty()
    );
}

#[test]
fn the_first_handoff_keeps_the_apps_already_delivered_local_date() {
    let now = at("2026-04-14T11:00:00+00:00");
    let tomorrow = CalendarAlarm {
        hour: 12,
        minute: 0,
        weekdays: 0,
    }
    .next_in(Some(now.date_naive()), &now)
    .expect("next unconsumed occurrence");
    let caller = owner("room", "so.tilo.clocks");
    let mut ledger = Ledger::open(None).expect("ledger");
    let mut request = alarm("reopened", tomorrow, 0);
    if let Some(alarm) = request.alarm.as_mut() {
        alarm.last_rung = Some(now.date_naive());
    }
    ledger.schedule(&caller, request).expect("handoff");
    assert!(ledger.take_due(&now, ids()).expect("tick").is_empty());
    assert_eq!(
        ledger.pending(&caller),
        vec![("reopened".into(), tomorrow.timestamp_millis())],
        "recomputing must not move an already rung date back into the schedule"
    );
}

#[test]
fn deadlines_survive_restart_and_do_not_repeat_once_delivered() {
    let temp = tempfile::tempdir().expect("private fixture");
    let path = temp.path().join("schedules.json");
    let caller = owner("room", "so.tilo.clocks");
    let due = at("2026-04-14T12:00:00+00:00");
    let mut ledger = Ledger::open(Some(path.clone())).expect("new ledger");
    ledger
        .schedule(&caller, alarm("alarm", due, 0))
        .expect("handoff");
    drop(ledger); // The app and the daemon may both exit before the deadline.
    let mut restarted = Ledger::open(Some(path.clone())).expect("restart");
    let delivered = restarted.take_due(&due, ids()).expect("deliver");
    assert_eq!(delivered.len(), 1);
    assert_eq!(
        delivered.first().and_then(|n| n.caller.as_ref()),
        Some(&Caller {
            app_id: "so.tilo.clocks".into(),
            workspace: Some("room".into())
        })
    );
    // Reopening the app hands the same deadline over again: it is not rearmed.
    restarted
        .schedule(&caller, alarm("alarm", due, 0))
        .expect("idempotent reopen");
    drop(restarted);
    let mut restarted = Ledger::open(Some(path)).expect("second restart");
    assert!(
        restarted
            .take_due(&due, ids())
            .expect("second tick")
            .is_empty()
    );
    assert!(restarted.pending(&caller).is_empty());
}

#[test]
fn cancellation_and_tags_are_scoped_to_the_workspace_and_app() {
    let mut ledger = Ledger::open(None).expect("ledger");
    let due = at("2026-04-14T12:00:00+00:00");
    let a = owner("one", "so.tilo.clocks");
    let b = owner("two", "so.tilo.clocks");
    let c = owner("one", "so.tilo.other");
    for caller in [&a, &b, &c] {
        ledger
            .schedule(caller, alarm("same-tag", due, 0))
            .expect("schedule");
    }
    ledger.cancel(&a, "same-tag").expect("cancel its own");
    assert!(ledger.pending(&a).is_empty());
    assert_eq!(ledger.pending(&b).len(), 1);
    assert_eq!(ledger.pending(&c).len(), 1);
    let delivered = ledger.take_due(&due, ids()).expect("deliver the others");
    assert_eq!(delivered.len(), 2);
    assert!(delivered.iter().all(|n| {
        n.caller
            .as_ref()
            .map(|c| (c.app_id.as_str(), c.workspace.as_deref()))
            != Some(("so.tilo.clocks", Some("one")))
    }));
}

#[test]
fn a_repeating_alarm_catches_up_once_and_follows_local_time() {
    let mut ledger = Ledger::open(None).expect("ledger");
    let caller = owner("room", "so.tilo.clocks");
    ledger
        .schedule(
            &caller,
            alarm("daily", at("2026-04-14T12:00:00+00:00"), 127),
        )
        .expect("schedule");
    let late = at("2026-04-17T15:00:00+00:00");
    assert_eq!(
        ledger.take_due(&late, ids()).expect("catch up").len(),
        1,
        "missed days are not replayed"
    );
    assert!(ledger.take_due(&late, ids()).expect("again").is_empty());
    let changed_zone = at("2026-04-18T10:00:00+02:00");
    assert!(
        ledger
            .take_due(&changed_zone, ids())
            .expect("zone")
            .is_empty()
    );
    assert_eq!(
        ledger.pending(&caller),
        vec![(
            "daily".into(),
            at("2026-04-18T12:00:00+02:00").timestamp_millis()
        )]
    );
    let local_noon = at("2026-04-18T12:00:00+02:00");
    assert_eq!(ledger.take_due(&local_noon, ids()).expect("noon").len(), 1);
    assert!(
        ledger
            .take_due(&at("2026-04-18T11:00:00+02:00"), ids())
            .expect("clock back")
            .is_empty()
    );
    assert!(
        ledger
            .take_due(&local_noon, ids())
            .expect("same date")
            .is_empty(),
        "a clock rollback must not ring an alarm twice"
    );
}

#[test]
fn a_failed_write_keeps_the_deadline_and_delivers_nothing() {
    let temp = tempfile::tempdir().expect("private fixture");
    let path = temp.path().join("schedules.json");
    let caller = owner("room", "so.tilo.clocks");
    let due = at("2026-04-14T12:00:00+00:00");
    let mut ledger = Ledger::open(Some(path.clone())).expect("ledger");
    ledger
        .schedule(&caller, alarm("alarm", due, 0))
        .expect("schedule");
    std::fs::remove_file(&path).expect("remove the file");
    std::fs::create_dir(&path).expect("block its replacement with a directory");
    assert!(ledger.take_due(&due, ids()).is_err());
    assert_eq!(ledger.pending(&caller).len(), 1);
    std::fs::remove_dir(&path).expect("writable again");
    assert_eq!(ledger.take_due(&due, ids()).expect("retry").len(), 1);
}

#[test]
fn a_rung_alarms_actions_reach_its_deadline_through_the_handle() {
    let schedules = Schedules::in_memory();
    let due = at("2026-04-14T12:00:00+00:00");
    schedules
        .schedule(&owner("room", "so.tilo.clocks"), alarm("alarm", due, 0))
        .expect("schedule");
    let rung = schedules.take_due(&due, ids()).expect("rung");
    let id = rung.first().expect("one").id;
    assert!(
        !schedules.alarm_action(id, "default"),
        "not an alarm's action"
    );
    assert!(!schedules.alarm_action(id + 1, SNOOZE), "no such ring");
    assert!(schedules.alarm_action(id, SNOOZE));
    let [deadline] = schedules.deadlines().try_into().expect("one deadline");
    assert!(deadline.snoozed_until.is_some());
    assert_eq!(deadline.alarm, Some((12, 0, 0)));
    assert_eq!(schedules.dismiss_snooze(&deadline.token, 0), Ok(true));
    assert!(schedules.next_at_ms().is_none());
    assert_eq!(schedules.cancel_token(&deadline.token), Ok(true));
    assert!(schedules.deadlines().is_empty());
}

#[test]
fn an_unavailable_ledger_refuses_rather_than_losing_deadlines() {
    let temp = tempfile::tempdir().expect("private fixture");
    let path = temp.path().join("schedules.json");
    std::fs::write(&path, b"not a ledger").expect("corrupt");
    let schedules = Schedules::open(Some(path));
    assert!(
        schedules
            .schedule(
                &owner("room", "app"),
                alarm("a", at("2026-04-14T12:00:00+00:00"), 0)
            )
            .is_err()
    );
}
