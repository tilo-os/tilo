use super::*;
use durable_store::DurableStore as _;

/// The instant `value` names, read in the offset it is written with.
fn at(value: &str) -> jiff::Zoned {
    let offset = jiff::fmt::temporal::Pieces::parse(value)
        .ok()
        .and_then(|pieces| pieces.to_numeric_offset())
        .expect("an offset");
    let instant: jiff::Timestamp = value.parse().expect("valid test instant");
    instant.to_zoned(jiff::tz::TimeZone::fixed(offset))
}

/// Keep `bytes` as the log of the store at `root`, as an earlier daemon left it.
fn keep_log(root: &Path, bytes: &[u8]) {
    use durable_store::DurableStore as _;
    let store = app_state::shared_store(root);
    let key = durable_store::StorageKey::app(LOG_KEY);
    {
        let _lease = store.lease(&key).expect("nothing else keeps the log");
        store
            .write(&key, durable_store::WriteOp::Replace(bytes.to_vec()), None)
            .expect("the log written");
    }
    store.flush(Some(&key)).expect("the log is on disk");
}

fn owner(workspace: &str, app: &str) -> Owner {
    Owner {
        app: app.into(),
        workspace: workspace.into(),
    }
}

fn alarm(tag: &str, due: &jiff::Zoned, weekdays: u8) -> ScheduleRequest {
    ScheduleRequest {
        tag: tag.into(),
        title: "Alarm".into(),
        body: "Wake up".into(),
        activation: "tilo-clocks:alarms".into(),
        at_ms: due.timestamp().as_millisecond(),
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

/// An alarm scheduled and rung once at `due`: the log and the ringing
/// occurrence.
fn rung(due: &jiff::Zoned, weekdays: u8, path: Option<&Path>) -> (Log, Occurrence) {
    let mut log = Log::open(path).expect("log");
    log.schedule(
        &owner("studio", "so.tilo.clocks"),
        alarm("alarm", due, weekdays),
    )
    .expect("handoff");
    let ringing = log.take_due(due, ids()).expect("rung");
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
    assert_eq!(occurrence.occurrence_ms, due.timestamp().as_millisecond());
    (log, occurrence)
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
        Date::new(2026, 4, 14).ok()
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

tilo_test_env::scenario! {
    fn the_log_keeps_deadlines_across_restarts_and_failed_writes;

    phase a_log_written_under_the_workspace_key_still_delivers_its_deadline;
    {
        let temp = tempfile::tempdir().expect("private fixture");
        let path = temp.path().to_path_buf();
        keep_log(
            &path,
            br#"[{"workspace":"studio","app":"so.tilo.clocks","tag":"alarm","title":"Alarm",
             "body":"Wake up","activation":"tilo-clocks:alarms","original_at_ms":1000,
             "next_at_ms":1000,"calendar":null,"last_rung":null,
             "token":"0123456789abcdef0123456789abcdef"}]"#,
        );
        let log = Log::open(Some(&path)).expect("log");
        assert_eq!(
            log.pending(&owner("studio", "so.tilo.clocks")),
            [("alarm".to_owned(), 1000)]
        );
    }

    phase deadlines_survive_restart_and_do_not_repeat_once_delivered;
    {
        let temp = tempfile::tempdir().expect("private fixture");
        let path = temp.path().to_path_buf();
        let caller = owner("studio", "so.tilo.clocks");
        let due = at("2026-04-14T12:00:00+00:00");
        let mut log = Log::open(Some(&path)).expect("new log");
        log
            .schedule(&caller, alarm("alarm", &due, 0))
            .expect("handoff");
        drop(log); // The app and the daemon may both exit before the deadline.
        let mut restarted = Log::open(Some(&path)).expect("restart");
        let delivered = restarted.take_due(&due, ids()).expect("deliver");
        assert_eq!(delivered.len(), 1);
        assert_eq!(
            delivered.first().and_then(|n| n.caller.as_ref()),
            Some(&Caller {
                app_id: "so.tilo.clocks".into(),
                workspace: Some("studio".into())
            })
        );
        // Reopening the app hands the same deadline over again: it is not rearmed.
        restarted
            .schedule(&caller, alarm("alarm", &due, 0))
            .expect("idempotent reopen");
        drop(restarted);
        let mut restarted = Log::open(Some(&path)).expect("second restart");
        assert!(
            restarted
                .take_due(&due, ids())
                .expect("second tick")
                .is_empty()
        );
        assert!(restarted.pending(&caller).is_empty());
    }

    phase a_failed_write_keeps_the_deadline_and_delivers_nothing;
    {
        let temp = tempfile::tempdir().expect("private fixture");
        let path = temp.path().to_path_buf();
        let caller = owner("studio", "so.tilo.clocks");
        let due = at("2026-04-14T12:00:00+00:00");
        let mut log = Log::open(Some(&path)).expect("log");
        log
            .schedule(&caller, alarm("alarm", &due, 0))
            .expect("schedule");
        let file = app_state::shared_store(&path)
            .local_path(&durable_store::StorageKey::app(LOG_KEY))
            .expect("the log is a file");
        std::fs::remove_file(&file).expect("remove the file");
        std::fs::create_dir(&file).expect("block its replacement with a directory");
        assert!(log.take_due(&due, ids()).is_err());
        assert_eq!(log.pending(&caller).len(), 1);
        std::fs::remove_dir(&file).expect("writable again");
        assert_eq!(log.take_due(&due, ids()).expect("retry").len(), 1);
    }

    phase an_unavailable_log_refuses_rather_than_losing_deadlines;
    {
        let temp = tempfile::tempdir().expect("private fixture");
        let path = temp.path().to_path_buf();
        keep_log(&path, b"not a log");
        let schedules = Schedules::open(Some(&path));
        assert!(
            schedules
                .schedule(
                    &owner("studio", "app"),
                    alarm("a", &at("2026-04-14T12:00:00+00:00"), 0)
                )
                .is_err()
        );
    }
}

tilo_test_env::scenario! {
    fn a_rung_alarm_is_snoozed_and_dismissed_through_its_occurrence;

    phase a_ringing_occurrence_carries_its_workspace;
    {
        let due = at("2026-04-14T12:00:00+00:00");
        let (_log, occurrence) = rung(&due, 127, None);
        let written = serde_json::to_value(&occurrence).expect("its data");
        assert_eq!(written["workspace"], "studio");
    }

    phase a_snooze_survives_restart_and_rings_once;
    {
        let temp = tempfile::tempdir().expect("private fixture");
        let path = temp.path().to_path_buf();
        let due = at("2026-04-14T12:00:00+00:00");
        let (mut log, occurrence) = rung(&due, 127, Some(&path));
        let original = log.alarm_states(&owner("studio", "so.tilo.clocks"));
        log
            .act(
                "so.tilo.clocks",
                &occurrence,
                AlarmAction::Snooze,
                due.timestamp().as_millisecond(),
            )
            .expect("snooze");
        drop(log);
        let mut restarted = Log::open(Some(&path)).expect("restart");
        let snoozed = due + jiff::SignedDuration::from_mins(5);
        assert!(
            restarted
                .take_due(&(&snoozed - jiff::SignedDuration::from_secs(1)), ids())
                .expect("before")
                .is_empty()
        );
        assert_eq!(restarted.take_due(&snoozed, ids()).expect("rings").len(), 1);
        assert!(
            restarted
                .take_due(&(&snoozed + jiff::SignedDuration::from_hours(1)), ids())
                .expect("no duplicate")
                .is_empty()
        );
        assert_eq!(
            restarted.alarm_states(&owner("studio", "so.tilo.clocks")),
            original,
            "a snooze does not rewrite the calendar's delivery history"
        );
        assert!(
            restarted
                .act(
                    "so.tilo.clocks",
                    &occurrence,
                    AlarmAction::Snooze,
                    snoozed.timestamp().as_millisecond()
                )
                .is_err(),
            "an old card cannot snooze the new ring"
        );
    }

    phase duplicate_snooze_and_dismiss_are_durable_and_cancel_pending_snooze;
    {
        let due = at("2026-04-14T12:00:00+00:00");
        let temp = tempfile::tempdir().expect("private fixture");
        let path = temp.path().to_path_buf();
        let (mut log, occurrence) = rung(&due, 0, Some(&path));
        let app = "so.tilo.clocks";
        log
            .act(
                app,
                &occurrence,
                AlarmAction::Snooze,
                due.timestamp().as_millisecond(),
            )
            .expect("snooze");
        let deadline = log.next_at_ms();
        log
            .act(
                app,
                &occurrence,
                AlarmAction::Snooze,
                (&due + jiff::SignedDuration::from_mins(2)).timestamp().as_millisecond(),
            )
            .expect("idempotent retry");
        assert_eq!(
            log.next_at_ms(),
            deadline,
            "a retry cannot extend a snooze"
        );
        log
            .act(
                app,
                &occurrence,
                AlarmAction::Dismiss,
                due.timestamp().as_millisecond(),
            )
            .expect("dismiss the pending snooze");
        log
            .act(
                app,
                &occurrence,
                AlarmAction::Dismiss,
                due.timestamp().as_millisecond(),
            )
            .expect("idempotent dismiss");
        assert!(log.next_at_ms().is_none());
        assert!(
            log
                .act(
                    app,
                    &occurrence,
                    AlarmAction::Snooze,
                    due.timestamp().as_millisecond()
                )
                .is_err()
        );
        drop(log);
        let mut restarted = Log::open(Some(&path)).expect("restart after dismissal");
        assert!(restarted.next_at_ms().is_none());
        assert!(
            restarted
                .take_due(&(due + jiff::SignedDuration::from_hours(1)), ids())
                .expect("after the cancelled snooze")
                .is_empty()
        );
    }

    phase cancelling_a_snoozed_alarm_survives_restart_and_a_stale_dismiss_cannot_rearm_it;
    {
        let temp = tempfile::tempdir().expect("private fixture");
        let path = temp.path().to_path_buf();
        let due = at("2026-04-14T12:00:00+00:00");
        let (mut log, occurrence) = rung(&due, 0, Some(&path));
        let app = "so.tilo.clocks";
        log
            .act(
                app,
                &occurrence,
                AlarmAction::Snooze,
                due.timestamp().as_millisecond(),
            )
            .expect("snooze");
        log
            .cancel(&owner("studio", app), "alarm")
            .expect("cancel");
        drop(log);
        let mut restarted = Log::open(Some(&path)).expect("restart");
        assert!(
            restarted
                .act(
                    app,
                    &occurrence,
                    AlarmAction::Snooze,
                    due.timestamp().as_millisecond()
                )
                .is_err()
        );
        restarted
            .act(
                app,
                &occurrence,
                AlarmAction::Dismiss,
                due.timestamp().as_millisecond(),
            )
            .expect("a stale card's dismissal");
        assert!(
            restarted
                .take_due(&(due + jiff::SignedDuration::from_hours(1)), ids())
                .expect("past the cancelled deadline")
                .is_empty()
        );
        assert!(restarted.pending(&owner("studio", app)).is_empty());
    }

    phase foreign_app_workspace_token_and_stale_occurrence_cannot_change_an_alarm;
    {
        let due = at("2026-04-14T12:00:00+00:00");
        let (mut log, occurrence) = rung(&due, 127, None);
        let original = log.alarm_states(&owner("studio", "so.tilo.clocks"));
        for changed in 0..4 {
            let mut target = occurrence.clone();
            let mut app = "so.tilo.clocks";
            match changed {
                0 => app = "another.app",
                1 => target.workspace = "another-workspace".into(),
                2 => target.token = "0123456789abcdef0123456789abcdef".into(),
                _ => target.occurrence_ms -= 1,
            }
            assert!(
                log
                    .act(app, &target, AlarmAction::Snooze, due.timestamp().as_millisecond())
                    .is_err()
            );
        }
        assert_eq!(
            log.alarm_states(&owner("studio", "so.tilo.clocks")),
            original
        );
    }

    phase snooze_stays_in_utc_across_time_zone_and_wall_clock_changes;
    {
        let due = at("2026-04-14T12:00:00+00:00");
        let (mut log, occurrence) = rung(&due, 127, None);
        log
            .act(
                "so.tilo.clocks",
                &occurrence,
                AlarmAction::Snooze,
                due.timestamp().as_millisecond(),
            )
            .expect("snooze");
        let expected = (&due + jiff::SignedDuration::from_mins(5)).timestamp().as_millisecond();
        assert_eq!(log.next_at_ms(), Some(expected));
        assert!(
            log
                .take_due(&at("2026-04-14T21:03:00+09:00"), ids())
                .expect("zone")
                .is_empty()
        );
        assert_eq!(log.next_at_ms(), Some(expected));
        assert!(
            log
                .take_due(&at("2026-04-14T20:03:00+09:00"), ids())
                .expect("back")
                .is_empty()
        );
        assert_eq!(log.next_at_ms(), Some(expected));
        let forward = due + jiff::SignedDuration::from_mins(7);
        assert_eq!(log.take_due(&forward, ids()).expect("forward").len(), 1);
        assert!(
            log
                .take_due(&forward, ids())
                .expect("same clock")
                .is_empty()
        );
    }

    phase a_rung_alarms_actions_reach_its_deadline_through_the_handle;
    {
        let schedules = Schedules::in_memory();
        let due = at("2026-04-14T12:00:00+00:00");
        schedules
            .schedule(&owner("studio", "so.tilo.clocks"), alarm("alarm", &due, 0))
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
}

tilo_test_env::scenario! {
    fn deadlines_are_delivered_once_to_their_owner_at_their_local_time;

    phase the_first_handoff_keeps_the_apps_already_delivered_local_date;
    {
        let now = at("2026-04-14T11:00:00+00:00");
        let tomorrow = CalendarAlarm {
            hour: 12,
            minute: 0,
            weekdays: 0,
        }
        .next_in(Some(now.date()), &now)
        .expect("next unconsumed occurrence");
        let caller = owner("studio", "so.tilo.clocks");
        let mut log = Log::open(None).expect("log");
        let mut request = alarm("reopened", &tomorrow, 0);
        if let Some(alarm) = request.alarm.as_mut() {
            alarm.last_rung = Some(now.date());
        }
        log.schedule(&caller, request).expect("handoff");
        assert!(log.take_due(&now, ids()).expect("tick").is_empty());
        assert_eq!(
            log.pending(&caller),
            vec![("reopened".into(), tomorrow.timestamp().as_millisecond())],
            "recomputing must not move an already rung date back into the schedule"
        );
    }

    phase cancellation_and_tags_are_scoped_to_the_workspace_and_app;
    {
        let mut log = Log::open(None).expect("log");
        let due = at("2026-04-14T12:00:00+00:00");
        let a = owner("one", "so.tilo.clocks");
        let b = owner("two", "so.tilo.clocks");
        let c = owner("one", "so.tilo.other");
        for caller in [&a, &b, &c] {
            log
                .schedule(caller, alarm("same-tag", &due, 0))
                .expect("schedule");
        }
        log.cancel(&a, "same-tag").expect("cancel its own");
        assert!(log.pending(&a).is_empty());
        assert_eq!(log.pending(&b).len(), 1);
        assert_eq!(log.pending(&c).len(), 1);
        let delivered = log.take_due(&due, ids()).expect("deliver the others");
        assert_eq!(delivered.len(), 2);
        assert!(delivered.iter().all(|n| {
            n.caller
                .as_ref()
                .map(|c| (c.app_id.as_str(), c.workspace.as_deref()))
                != Some(("so.tilo.clocks", Some("one")))
        }));
    }

    phase a_repeating_alarm_catches_up_once_and_follows_local_time;
    {
        let mut log = Log::open(None).expect("log");
        let caller = owner("studio", "so.tilo.clocks");
        log
            .schedule(
                &caller,
                alarm("daily", &at("2026-04-14T12:00:00+00:00"), 127),
            )
            .expect("schedule");
        let late = at("2026-04-17T15:00:00+00:00");
        assert_eq!(
            log.take_due(&late, ids()).expect("catch up").len(),
            1,
            "missed days are not replayed"
        );
        assert!(log.take_due(&late, ids()).expect("again").is_empty());
        let changed_zone = at("2026-04-18T10:00:00+02:00");
        assert!(
            log
                .take_due(&changed_zone, ids())
                .expect("zone")
                .is_empty()
        );
        assert_eq!(
            log.pending(&caller),
            vec![(
                "daily".into(),
                at("2026-04-18T12:00:00+02:00").timestamp().as_millisecond()
            )]
        );
        let local_noon = at("2026-04-18T12:00:00+02:00");
        assert_eq!(log.take_due(&local_noon, ids()).expect("noon").len(), 1);
        assert!(
            log
                .take_due(&at("2026-04-18T11:00:00+02:00"), ids())
                .expect("clock back")
                .is_empty()
        );
        assert!(
            log
                .take_due(&local_noon, ids())
                .expect("same date")
                .is_empty(),
            "a clock rollback must not ring an alarm twice"
        );
    }
}
