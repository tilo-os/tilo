use std::cell::{Cell, RefCell};

use super::*;

/// The person the deadlines are kept for.
pub(crate) const PERSON: &str = "person";

/// A workspace's store, as the session hands it to its services: what the person reads of it,
/// and whether it refuses writes.
pub(crate) struct Workspace {
    store: RefCell<tilo_resource_store::Store>,
    reader: Cell<&'static str>,
    refuse: Cell<bool>,
}

impl Workspace {
    pub(crate) fn new() -> Self {
        Self {
            store: RefCell::new(tilo_resource_store::Store::new("person")),
            reader: Cell::new(PERSON),
            refuse: Cell::new(false),
        }
    }

    /// Write `json` as the record of `app`'s deadlines, as another device of the person wrote it.
    fn hold(&self, app: &str, json: &str) {
        let resource = collab_types::store::Resource {
            readers: PERSON.into(),
            fields: BTreeMap::from([(
                ENTRIES_FIELD.to_owned(),
                collab_types::store::Cell::One(json.into()),
            )]),
            ..collab_types::store::Resource::default()
        };
        tilo_resource_store::wire::write(
            &mut self.store.borrow_mut(),
            Command::Put {
                kind: RECORD_KIND.into(),
                id: app.into(),
                resource,
                stamp: tilo_resource_store::Stamp::default(),
            },
        )
        .expect("the record written");
    }
}

impl tilo_service::WorkspaceStore for Workspace {
    fn command(&self, app: &str, command: &[u8]) -> Result<Vec<u8>, String> {
        // The records are the session's: another app's command is refused, as the workspace
        // refuses an app the records of another.
        if app != RECORD_APP {
            return Err(format!("{app} asked for the session's records"));
        }
        let command: Command = serde_json::from_slice(command).map_err(|e| e.to_string())?;
        let reply = if command.writes() {
            if self.refuse.get() {
                return Err("the workspace refused the write".into());
            }
            tilo_resource_store::wire::write(&mut self.store.borrow_mut(), command)
                .map_err(|e| e.to_string())?;
            Reply::Written
        } else {
            let reader = self.reader.get();
            tilo_resource_store::wire::read(&self.store.borrow(), &command, &|readers| {
                readers == reader
            })
            .ok_or("not a read")?
        };
        serde_json::to_vec(&reply).map_err(|e| e.to_string())
    }

    /// The person's own workspace: they are its only member.
    fn access(&self) -> Option<tilo_actions::policy::MemberAccess> {
        Some(tilo_actions::policy::MemberAccess::person(
            tilo_actions::policy::WorkspacePolicy::new("workspace", PERSON),
            PERSON,
        ))
    }
}

/// A log kept in `workspace`, as the daemon keeps it once the session reached it: what a
/// restarted daemon holds.
fn kept(workspace: &Workspace) -> Log {
    let mut log = Log {
        person: Some(PERSON.into()),
        ..Log::default()
    };
    log.keep_in(workspace).expect("the deadlines kept");
    log
}

/// The instant `value` names, read in the offset it is written with.
fn at(value: &str) -> jiff::Zoned {
    let offset = jiff::fmt::temporal::Pieces::parse(value)
        .ok()
        .and_then(|pieces| pieces.to_numeric_offset())
        .expect("an offset");
    let instant: jiff::Timestamp = value.parse().expect("valid test instant");
    instant.to_zoned(jiff::tz::TimeZone::fixed(offset))
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

/// An alarm scheduled and rung once at `due`: the log (kept in `workspace` when given) and the
/// ringing occurrence.
fn rung(due: &jiff::Zoned, weekdays: u8, workspace: Option<&Workspace>) -> (Log, Occurrence) {
    let mut log = workspace.map_or_else(Log::default, kept);
    log.schedule(
        &owner("studio", "so.tilo.clocks"),
        alarm("alarm", due, weekdays),
    )
    .expect("handoff");
    let ringing = log.take_due(due, ids());
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

/// `log`'s changes written to `workspace`, and the log a restarted daemon holds from there.
fn restart(mut log: Log, workspace: &Workspace) -> Log {
    log.keep_in(workspace).expect("written");
    drop(log);
    kept(workspace)
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
    fn the_workspace_keeps_deadlines_across_restarts_and_refused_writes;

    phase a_deadline_another_device_wrote_is_delivered;
    {
        let workspace = Workspace::new();
        workspace.hold(
            "so.tilo.clocks",
            r#"[{"workspace":"studio","app":"so.tilo.clocks","tag":"alarm","title":"Alarm",
             "body":"Wake up","activation":"tilo-clocks:alarms","original_at_ms":1000,
             "next_at_ms":1000,"calendar":null,"last_rung":null,"last_at_ms":null,
             "token":"0123456789abcdef0123456789abcdef","active":null,"snooze_at_ms":null}]"#,
        );
        let log = kept(&workspace);
        assert_eq!(
            log.pending(&owner("studio", "so.tilo.clocks")),
            [("alarm".to_owned(), 1000)]
        );
    }

    phase deadlines_survive_restart_and_do_not_repeat_once_delivered;
    {
        let workspace = Workspace::new();
        let caller = owner("studio", "so.tilo.clocks");
        let due = at("2026-04-14T12:00:00+00:00");
        let mut log = kept(&workspace);
        log.schedule(&caller, alarm("alarm", &due, 0))
            .expect("handoff");
        // The app and the daemon may both exit before the deadline.
        let mut restarted = restart(log, &workspace);
        let delivered = restarted.take_due(&due, ids());
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
        let mut restarted = restart(restarted, &workspace);
        assert!(restarted.take_due(&due, ids()).is_empty());
        assert!(restarted.pending(&caller).is_empty());
    }

    phase what_was_scheduled_before_the_workspace_was_reached_is_kept_there;
    {
        let workspace = Workspace::new();
        let due = at("2026-04-14T12:00:00+00:00");
        let mut earlier = kept(&workspace);
        earlier
            .schedule(&owner("studio", "so.tilo.clocks"), alarm("alarm", &due, 0))
            .expect("schedule");
        earlier
            .schedule(&owner("studio", "so.tilo.clocks"), alarm("other", &due, 0))
            .expect("schedule");
        restart(earlier, &workspace);
        let mut log = Log {
            person: Some(PERSON.into()),
            ..Log::default()
        };
        let mut changed = alarm("alarm", &due, 0);
        changed.title = "Changed".into();
        log.schedule(&owner("studio", "so.tilo.clocks"), changed)
            .expect("scheduled before the workspace was reached");
        assert!(log.keep_in(&workspace).expect("kept"), "taken in now");
        let restarted = kept(&workspace);
        let mut titles: Vec<_> = restarted
            .deadlines()
            .into_iter()
            .map(|deadline| (deadline.tag, deadline.title))
            .collect();
        titles.sort();
        assert_eq!(
            titles,
            [
                ("alarm".to_owned(), "Changed".to_owned()),
                ("other".to_owned(), "Alarm".to_owned())
            ],
            "what was scheduled here stands over what the workspace held"
        );
    }

    phase a_refused_write_keeps_the_deadline_and_is_made_again;
    {
        let workspace = Workspace::new();
        let caller = owner("studio", "so.tilo.clocks");
        let due = at("2026-04-14T12:00:00+00:00");
        let mut log = kept(&workspace);
        log.schedule(&caller, alarm("alarm", &due, 0))
            .expect("schedule");
        workspace.refuse.set(true);
        assert!(log.keep_in(&workspace).is_err());
        assert_eq!(log.pending(&caller).len(), 1, "kept here meanwhile");
        workspace.refuse.set(false);
        assert!(kept(&workspace).pending(&caller).is_empty());
        let mut restarted = restart(log, &workspace);
        assert_eq!(restarted.take_due(&due, ids()).len(), 1);
    }

    phase unreadable_records_are_neither_taken_in_nor_written_over;
    {
        let workspace = Workspace::new();
        workspace.hold("app", "not deadlines");
        let mut log = Log {
            person: Some(PERSON.into()),
            ..Log::default()
        };
        log.schedule(&owner("studio", "app"), alarm("a", &at("2026-04-14T12:00:00+00:00"), 0))
            .expect("schedule");
        assert!(log.keep_in(&workspace).is_err());
        assert!(log.keep_in(&workspace).is_err(), "and not on the next pass either");
        assert_eq!(log.pending(&owner("studio", "app")).len(), 1);
    }

    phase the_deadlines_are_kept_for_their_person_alone;
    {
        let workspace = Workspace::new();
        let caller = owner("studio", "so.tilo.clocks");
        let mut log = kept(&workspace);
        log.schedule(&caller, alarm("alarm", &at("2026-04-14T12:00:00+00:00"), 0))
            .expect("schedule");
        log.keep_in(&workspace).expect("written");
        workspace.reader.set("someone else");
        assert!(kept(&workspace).pending(&caller).is_empty());
        let mut nobody = Log::default();
        assert!(nobody.keep_in(&workspace).is_err(), "kept for nobody");
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
        let workspace = Workspace::new();
        let due = at("2026-04-14T12:00:00+00:00");
        let (mut log, occurrence) = rung(&due, 127, Some(&workspace));
        let original = log.alarm_states(&owner("studio", "so.tilo.clocks"));
        log
            .act(
                "so.tilo.clocks",
                &occurrence,
                AlarmAction::Snooze,
                due.timestamp().as_millisecond(),
            )
            .expect("snooze");
        let mut restarted = restart(log, &workspace);
        let snoozed = due + jiff::SignedDuration::from_mins(5);
        assert!(
            restarted
                .take_due(&(&snoozed - jiff::SignedDuration::from_secs(1)), ids())
                .is_empty()
        );
        assert_eq!(restarted.take_due(&snoozed, ids()).len(), 1);
        assert!(
            restarted
                .take_due(&(&snoozed + jiff::SignedDuration::from_hours(1)), ids())
                .is_empty(),
            "no duplicate"
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

    phase duplicate_snooze_and_dismiss_are_kept_and_cancel_pending_snooze;
    {
        let due = at("2026-04-14T12:00:00+00:00");
        let workspace = Workspace::new();
        let (mut log, occurrence) = rung(&due, 0, Some(&workspace));
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
        let mut restarted = restart(log, &workspace);
        assert!(restarted.next_at_ms().is_none());
        assert!(
            restarted
                .take_due(&(due + jiff::SignedDuration::from_hours(1)), ids())
                .is_empty(),
            "after the cancelled snooze"
        );
    }

    phase cancelling_a_snoozed_alarm_survives_restart_and_a_stale_dismiss_cannot_rearm_it;
    {
        let workspace = Workspace::new();
        let due = at("2026-04-14T12:00:00+00:00");
        let (mut log, occurrence) = rung(&due, 0, Some(&workspace));
        let app = "so.tilo.clocks";
        log
            .act(
                app,
                &occurrence,
                AlarmAction::Snooze,
                due.timestamp().as_millisecond(),
            )
            .expect("snooze");
        log.cancel(&owner("studio", app), "alarm");
        let mut restarted = restart(log, &workspace);
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
                .is_empty(),
            "past the cancelled deadline"
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
        assert!(log.take_due(&at("2026-04-14T21:03:00+09:00"), ids()).is_empty());
        assert_eq!(log.next_at_ms(), Some(expected));
        assert!(log.take_due(&at("2026-04-14T20:03:00+09:00"), ids()).is_empty());
        assert_eq!(log.next_at_ms(), Some(expected));
        let forward = due + jiff::SignedDuration::from_mins(7);
        assert_eq!(log.take_due(&forward, ids()).len(), 1);
        assert!(log.take_due(&forward, ids()).is_empty(), "same clock");
    }

    phase a_rung_alarms_actions_reach_its_deadline_through_the_handle;
    {
        let schedules = Schedules::in_memory();
        let due = at("2026-04-14T12:00:00+00:00");
        schedules
            .schedule(&owner("studio", "so.tilo.clocks"), alarm("alarm", &due, 0))
            .expect("schedule");
        let rung = schedules.take_due(&due, ids());
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
        assert!(schedules.cancel_token(&deadline.token));
        assert!(schedules.deadlines().is_empty());
    }

    phase the_handle_keeps_its_deadlines_in_the_workspace_and_tells_a_refusal_once;
    {
        let workspace = Workspace::new();
        let schedules = Schedules::in_memory();
        let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wakes = Arc::clone(&woken);
        schedules.on_change(Arc::new(move || {
            wakes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }));
        let due = at("2026-04-14T12:00:00+00:00");
        schedules
            .schedule(&owner("studio", "so.tilo.clocks"), alarm("alarm", &due, 0))
            .expect("schedule");
        assert_eq!(woken.load(std::sync::atomic::Ordering::Relaxed), 1, "the writer is woken");
        assert!(schedules.keep_in(&workspace).is_err(), "kept for nobody yet");
        assert_eq!(schedules.keep_in(&workspace), Ok(false), "told once");
        schedules.own_person(PERSON);
        assert_eq!(schedules.keep_in(&workspace), Ok(true));
        assert_eq!(schedules.keep_in(&workspace), Ok(false));
        assert_eq!(kept(&workspace).pending(&owner("studio", "so.tilo.clocks")).len(), 1);
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
        let mut log = Log::default();
        let mut request = alarm("reopened", &tomorrow, 0);
        if let Some(alarm) = request.alarm.as_mut() {
            alarm.last_rung = Some(now.date());
        }
        log.schedule(&caller, request).expect("handoff");
        assert!(log.take_due(&now, ids()).is_empty());
        assert_eq!(
            log.pending(&caller),
            vec![("reopened".into(), tomorrow.timestamp().as_millisecond())],
            "recomputing must not move an already rung date back into the schedule"
        );
    }

    phase cancellation_and_tags_are_scoped_to_the_workspace_and_app;
    {
        let mut log = Log::default();
        let due = at("2026-04-14T12:00:00+00:00");
        let a = owner("one", "so.tilo.clocks");
        let b = owner("two", "so.tilo.clocks");
        let c = owner("one", "so.tilo.other");
        for caller in [&a, &b, &c] {
            log
                .schedule(caller, alarm("same-tag", &due, 0))
                .expect("schedule");
        }
        log.cancel(&a, "same-tag");
        assert!(log.pending(&a).is_empty());
        assert_eq!(log.pending(&b).len(), 1);
        assert_eq!(log.pending(&c).len(), 1);
        let delivered = log.take_due(&due, ids());
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
        let mut log = Log::default();
        let caller = owner("studio", "so.tilo.clocks");
        log
            .schedule(
                &caller,
                alarm("daily", &at("2026-04-14T12:00:00+00:00"), 127),
            )
            .expect("schedule");
        let late = at("2026-04-17T15:00:00+00:00");
        assert_eq!(
            log.take_due(&late, ids()).len(),
            1,
            "missed days are not replayed"
        );
        assert!(log.take_due(&late, ids()).is_empty());
        let changed_zone = at("2026-04-18T10:00:00+02:00");
        assert!(log.take_due(&changed_zone, ids()).is_empty());
        assert_eq!(
            log.pending(&caller),
            vec![(
                "daily".into(),
                at("2026-04-18T12:00:00+02:00").timestamp().as_millisecond()
            )]
        );
        let local_noon = at("2026-04-18T12:00:00+02:00");
        assert_eq!(log.take_due(&local_noon, ids()).len(), 1);
        assert!(log.take_due(&at("2026-04-18T11:00:00+02:00"), ids()).is_empty());
        assert!(
            log.take_due(&local_noon, ids()).is_empty(),
            "a clock rollback must not ring an alarm twice"
        );
    }
}
