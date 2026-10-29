use super::*;

// --- caldav accounts seam ------------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn caldav_sync_runs_off_thread_and_drains_results_tags_or_failure() {
    // Case `sync_merges_visible_calendar_events`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        // Sync is explicit now (no fabricated auto-pull on the first frame).
        app.apply_action(Action::Sync, &mut Edits::lend(&mut state));
        assert!(app.model.events.iter().any(|e| e.title == "Synced standup"));
        // The hidden "Holidays" calendar's event must not appear.
        assert!(!app.model.events.iter().any(|e| e.title == "Public holiday"));
    }
    // Case `live_sync_runs_off_the_ui_thread_without_blocking`.
    {
        // A live (non-demo) app with a configured account must dispatch the
        // pull to a worker thread and return immediately — the click never
        // blocks on the network. We assert the async hand-off, then abandon the
        // detached worker (its result is irrelevant to this behaviour).
        let mut app = CalendarApp::new_demo();
        app.sample_accounts = false; // exercise the real (threaded) branch
        app.accounts = AccountStore::sample();
        let mut state = demo_document(&app);
        app.start_sync(&mut Edits::lend(&mut state))
            .expect("a calendar that reads is synchronized");
        assert!(app.state().syncing, "a live sync is in flight");
        assert!(app.sync_rx.is_some(), "worker result channel is armed");
        // The worker's reply wakes the app through its waking channel;
        // waiting on a slow server paints nothing.
        assert!(
            !app.needs_animation_frame(),
            "waiting on the worker must not pump frames"
        );
        // Detach the worker so the test doesn't depend on the network.
        app.sync_rx = None;
        app.state().syncing = false;
    }
    // Case `live_sync_with_no_accounts_answers_instantly`.
    {
        // No worker, no thread, no spin: an empty account set reports its state
        // synchronously.
        let mut app = CalendarApp::new_demo();
        app.sample_accounts = false;
        app.accounts = AccountStore::default();
        let mut state = demo_document(&app);
        app.start_sync(&mut Edits::lend(&mut state))
            .expect("a calendar that reads is synchronized");
        assert!(!app.state().syncing, "nothing to sync → no background work");
        assert!(app.status.text().unwrap_or("").contains("account"));
    }
    // Case `poll_sync_drains_a_finished_worker_into_the_model`.
    {
        // The drain path (`poll_sync` → `apply_sync_result`) folds a completed
        // pull into the model + CRDT and clears the in-flight flag. Driven with
        // a hand-fed channel so it is deterministic and network-free, but it is
        // the exact production code the live worker feeds.
        use crate::model::{Event, Palette};
        use chrono::NaiveTime;

        let mut app = CalendarApp::new_demo();
        app.sample_accounts = false;
        let mut state = demo_document(&app);
        let before = app.model.events.len();

        let (tx, rx) = std::sync::mpsc::channel();
        let start = app
            .today
            .and_time(NaiveTime::from_hms_opt(8, 0, 0).unwrap());
        let synced = Event {
            id: uuid::Uuid::new_v4(),
            title: "Pulled from server".into(),
            start,
            end: start + Duration::hours(1),
            location: "Remote".into(),
            color: Palette::Blue,
            ..Event::default()
        };
        tx.send((
            caldav::SyncResult {
                events: vec![synced],
                tasks: Vec::new(),
                ..caldav::SyncResult::default()
            },
            SyncOutcome::Synced {
                calendars: 1,
                events: 1,
            },
        ))
        .unwrap();
        app.sync_rx = Some(rx);
        app.state().syncing = true;

        app.poll_sync(&mut Edits::lend(&mut state));

        assert!(!app.state().syncing, "drain clears the in-flight flag");
        assert_eq!(app.model.events.len(), before + 1, "synced event landed");
        assert!(
            app.model
                .events
                .iter()
                .any(|e| e.title == "Pulled from server"),
            "the pulled event is now in the model"
        );
        // And it mirrored into the CRDT (persistence source of truth).
        assert!(!crate::resource::read_doc(&state).events.is_empty());
    }
    // Case `sync_tags_landed_events_with_their_source_calendar`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        app.apply_action(Action::Sync, &mut Edits::lend(&mut state));
        // The demo pulls "Synced standup" from the work/default collection.
        let standup = app
            .model
            .events
            .iter()
            .find(|e| e.title == "Synced standup")
            .expect("sync pulled the standup");
        assert_eq!(
            standup.calendar_id.as_deref(),
            Some("work/default"),
            "a synced event remembers which calendar it came from"
        );
        // A locally-created event stays untagged, so it is always visible.
        app.apply_action(Action::NewEvent, &mut Edits::lend(&mut state));
        let Some(Sel::Event(new_id)) = app.selection else {
            panic!("NewEvent selects the created event");
        };
        let new = app.model.event(new_id).unwrap();
        assert!(
            new.calendar_id.is_none(),
            "local events carry no source calendar"
        );
    }
    // Case `poll_sync_reports_failure_when_the_worker_drops`.
    {
        // The Disconnected branch: a worker that panics/drops its sender without
        // a reply must surface "Sync failed" and clear the in-flight state.
        let mut app = CalendarApp::new_demo();
        app.sample_accounts = false;
        let mut state = demo_document(&app);
        let (tx, rx) = std::sync::mpsc::channel::<SyncWorkerResult>();
        app.sync_rx = Some(rx);
        app.state().syncing = true;
        drop(tx); // worker vanished without sending
        app.poll_sync(&mut Edits::lend(&mut state));
        assert!(!app.state().syncing, "the in-flight flag is cleared");
        assert!(app.sync_rx.is_none(), "the dead channel is dropped");
        assert_eq!(
            app.status.text(),
            Some(crate::resource::t("calendar-sync-failed").as_str())
        );
    }
}

// --- create → name → persist flow ----------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn caldav_reconciliation_keeps_stable_resources_by_href() {
    // Case `caldav_reconciliation_updates_and_deletes_by_href_without_uuid_churn`.
    {
        let mut app = CalendarApp::new_demo();
        let mut doc = demo_document(&app);
        let start = NaiveDate::from_ymd_opt(2026, 8, 3)
            .unwrap()
            .and_hms_opt(9, 0, 0)
            .unwrap();
        let remote_event = |title: &str| Event {
            id: Uuid::new_v4(),
            title: title.into(),
            start,
            end: start + Duration::hours(1),
            color: Palette::Blue,
            calendar_id: Some("work/default".into()),
            ..Event::default()
        };
        app.apply_sync_result(
            caldav::SyncResult {
                resources: vec![caldav::RemoteResource {
                    calendar_id: "work/default".into(),
                    href: "/dav/work/meeting.ics".into(),
                    etag: Some("\"one\"".into()),
                    events: vec![remote_event("First title")],
                    tasks: Vec::new(),
                }],
                checkpoints: vec![caldav::CollectionCheckpoint {
                    calendar_id: "work/default".into(),
                    token: "token-1".into(),
                    full: true,
                    seen_hrefs: vec!["/dav/work/meeting.ics".into()],
                }],
                ..caldav::SyncResult::default()
            },
            &SyncOutcome::Synced {
                calendars: 1,
                events: 1,
            },
            &mut Edits::lend(&mut doc),
        )
        .expect("the calendar reads");
        let id = remote_at(&doc, "work/default", "/dav/work/meeting.ics")
            .expect("what the server keeps there is known");
        app.apply_sync_result(
            caldav::SyncResult {
                resources: vec![caldav::RemoteResource {
                    calendar_id: "work/default".into(),
                    href: "/dav/work/meeting.ics".into(),
                    etag: Some("\"two\"".into()),
                    events: vec![remote_event("Server-edited title")],
                    tasks: Vec::new(),
                }],
                checkpoints: vec![caldav::CollectionCheckpoint {
                    calendar_id: "work/default".into(),
                    token: "token-2".into(),
                    full: false,
                    seen_hrefs: vec!["/dav/work/meeting.ics".into()],
                }],
                ..caldav::SyncResult::default()
            },
            &SyncOutcome::Synced {
                calendars: 1,
                events: 1,
            },
            &mut Edits::lend(&mut doc),
        )
        .expect("the calendar reads");
        assert_eq!(app.model.event(id).unwrap().title, "Server-edited title");
        assert_eq!(remote_etag(&doc, id).as_deref(), Some("\"two\""));
        app.apply_sync_result(
            caldav::SyncResult {
                deletions: vec![caldav::RemoteDeletion {
                    calendar_id: "work/default".into(),
                    href: "/dav/work/meeting.ics".into(),
                }],
                checkpoints: vec![caldav::CollectionCheckpoint {
                    calendar_id: "work/default".into(),
                    token: "token-3".into(),
                    full: false,
                    seen_hrefs: Vec::new(),
                }],
                ..caldav::SyncResult::default()
            },
            &SyncOutcome::Synced {
                calendars: 1,
                events: 0,
            },
            &mut Edits::lend(&mut doc),
        )
        .expect("the calendar reads");
        assert!(app.model.event(id).is_none());
        assert!(
            !crate::resource::read_doc(&doc)
                .remote
                .contains_key(&id.to_string())
        );
        assert_eq!(
            crate::resource::read_doc(&doc).sync_tokens["work/default"],
            "token-3"
        );
    }
    // Case `caldav_reconciliation_keeps_a_vtodo_at_its_stable_resource_across_pulls`.
    {
        let uid = Uuid::new_v4();
        let remote_task = |done| crate::model::Task {
            id: uid,
            title: "Server task".into(),
            done,
            due: NaiveDate::from_ymd_opt(2026, 8, 3),
            priority: Priority::High,
            project: None,
            parent: None,
        };
        // Send the record through the real VTODO parser before reconciliation:
        // a second pull must update the href-bound task, never append one.
        let parsed_task = |done| {
            let (_, tasks) = crate::ics::parse(&crate::ics::export(&Calendar {
                events: ui::tilo_resource::Keyed::default(),
                tasks: vec![remote_task(done)].into(),
                projects: ui::tilo_resource::Keyed::default(),
                ..Calendar::default()
            }));
            tasks.into_iter().next().expect("one VTODO parses")
        };
        let mut app = CalendarApp::new_detached();
        let mut doc = ui::tilo_resource::Replica::<Calendar>::new().doc().clone();
        let apply =
            |app: &mut CalendarApp, doc: &mut AutoCommit, task: crate::model::Task, etag: &str| {
                app.apply_sync_result(
                    caldav::SyncResult {
                        resources: vec![caldav::RemoteResource {
                            calendar_id: "work/default".into(),
                            href: "/dav/work/tasks.ics".into(),
                            etag: Some(etag.into()),
                            events: Vec::new(),
                            tasks: vec![task],
                        }],
                        ..caldav::SyncResult::default()
                    },
                    &SyncOutcome::Synced {
                        calendars: 1,
                        events: 0,
                    },
                    &mut Edits::lend(doc),
                )
                .expect("the calendar reads");
            };

        apply(&mut app, &mut doc, parsed_task(false), "\"one\"");
        let id = remote_at(&doc, "work/default", "/dav/work/tasks.ics#tilo-task-0")
            .expect("the VTODO is known at its server address");
        apply(&mut app, &mut doc, parsed_task(true), "\"two\"");

        assert_eq!(
            app.model.tasks.iter().filter(|task| task.id == id).count(),
            1
        );
        assert!(app.model.task(id).is_some_and(|task| task.done));
        assert_eq!(remote_etag(&doc, id).as_deref(), Some("\"two\""));
    }
}

/// `doc` with its events no longer a keyed collection: what a newer or
/// broken writer can leave behind.
fn corrupt_events(doc: &mut AutoCommit) {
    automerge::transaction::Transactable::put(doc, ROOT, "events", "not a keyed collection")
        .expect("the field is written");
}

#[test]
fn a_calendar_that_does_not_read_is_neither_emptied_nor_synchronized() {
    let mut app = CalendarApp::new_demo();
    let mut state = demo_document(&app);
    present(&mut app, &mut state, &ROOT);
    let shown: Vec<Id> = app.model.events.iter().map(|event| event.id).collect();
    assert!(!shown.is_empty(), "the demo calendar has events");

    corrupt_events(&mut state);
    assert!(Calendar::read_whole(&state).is_err());
    present(&mut app, &mut state, &ROOT);
    assert_eq!(
        app.model
            .events
            .iter()
            .map(|event| event.id)
            .collect::<Vec<_>>(),
        shown,
        "the calendar that last read whole stays, not an empty one"
    );

    // A pull is not planned from nothing: it would protect no queued upload
    // and start from no sync token.
    app.sample_accounts = false;
    app.accounts = AccountStore::sample();
    let refusal = app
        .start_sync(&mut Edits::lend(&mut state))
        .expect_err("no pull starts from a calendar that does not read");
    assert!(refusal.starts_with("Sync unavailable"), "{refusal:?}");
    assert!(app.sync_rx.is_none() && !app.state().syncing);
    assert_eq!(app.status.text(), Some(refusal.as_str()));

    // Nor is a finished pull folded into nothing, which would give each item
    // the server holds a new identity.
    let start = app
        .today
        .and_time(chrono::NaiveTime::from_hms_opt(8, 0, 0).unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send((
        caldav::SyncResult {
            resources: vec![caldav::RemoteResource {
                calendar_id: "work/default".into(),
                href: "/dav/work/meeting.ics".into(),
                etag: Some("\"one\"".into()),
                events: vec![crate::model::Event {
                    id: Uuid::new_v4(),
                    title: "Pulled into a broken calendar".into(),
                    start,
                    end: start + Duration::hours(1),
                    ..crate::model::Event::default()
                }],
                tasks: Vec::new(),
            }],
            ..caldav::SyncResult::default()
        },
        SyncOutcome::Synced {
            calendars: 1,
            events: 1,
        },
    ))
    .unwrap();
    app.sync_rx = Some(rx);
    app.state().syncing = true;
    app.poll_sync(&mut Edits::lend(&mut state));
    assert!(!app.state().syncing, "the drain clears the in-flight flag");
    assert!(
        app.status
            .text()
            .is_some_and(|text| text.starts_with("Sync unavailable")),
        "the bar says the pull was not folded in, not that it synced: {:?}",
        app.status.text()
    );
    assert_eq!(
        app.model
            .events
            .iter()
            .map(|event| event.id)
            .collect::<Vec<_>>(),
        shown,
        "nothing was reconciled into a calendar that does not read"
    );
}
