//! A document that does not read as a calendar: the calendar the view holds
//! stays, and a synchronization or a reconciliation refuses to run on it.

use super::tests::{demo_document, present};
use super::{CalendarApp, SyncOutcome, caldav};
use crate::model::{Event, Palette};
use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ROOT};
use chrono::{Duration, NaiveTime};
use ui::widget::Edits;

/// `doc` with its events no longer a keyed collection: what a document a
/// newer or broken writer left behind reads as.
fn corrupt_events(doc: &mut AutoCommit) {
    doc.put(ROOT, "events", "not a keyed collection")
        .expect("the field is written");
}

#[test]
fn a_calendar_that_does_not_read_is_not_read_as_an_empty_one() {
    let mut app = CalendarApp::new_demo();
    let mut state = demo_document(&app);
    present(&mut app, &mut state, &automerge::ROOT);
    let shown: Vec<String> = app.model.events.iter().map(|e| e.title.clone()).collect();
    assert!(!shown.is_empty(), "the demo calendar has events");

    corrupt_events(&mut state);

    // The frame's read fails: the calendar the view holds stays, and the bar
    // says why instead of showing an empty month.
    present(&mut app, &mut state, &automerge::ROOT);
    assert!(
        app.model_read.failed(),
        "the document does not read as a calendar"
    );
    assert_eq!(
        app.model
            .events
            .iter()
            .map(|e| e.title.clone())
            .collect::<Vec<_>>(),
        shown,
        "the last calendar that read whole is kept, not emptied"
    );
    let status = app.status.as_deref().unwrap_or_default();
    assert!(
        status.contains("can't be read") && status.contains("events"),
        "the bar says the calendar cannot be read and where: {status:?}"
    );

    // Reading it again only repeats the same failure: it is not said twice.
    let said = app.status.as_deref().unwrap_or_default().to_owned();
    present(&mut app, &mut state, &automerge::ROOT);
    assert_eq!(app.status.as_deref().unwrap_or_default(), said);
}

#[test]
fn an_empty_calendar_that_reads_whole_is_not_a_failure() {
    // A document no calendar was ever written to reads as an empty one, not
    // as a document that does not read.
    assert!(
        crate::model::Calendar::read_whole(&AutoCommit::new()).is_ok(),
        "an unwritten document holds an empty calendar"
    );

    let mut app = CalendarApp::new_detached();
    let mut state = demo_document(&app);
    present(&mut app, &mut state, &automerge::ROOT);
    assert!(!app.model_read.failed(), "an empty calendar is valid");
    assert!(app.model.events.is_empty());
    assert!(
        app.model_read.take_report().is_none(),
        "an empty document has nothing to report"
    );
}

#[test]
fn synchronization_and_reconciliation_refuse_a_document_that_does_not_read() {
    // A pull is not started from a calendar read as empty: it would drop what
    // every queued edit is protected by and start from no sync token.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        corrupt_events(&mut state);
        let refusal = app
            .start_sync(&mut Edits::lend(&mut state))
            .expect_err("an unreadable calendar starts no sync");
        assert!(refusal.contains("can't be read"), "{refusal:?}");
        assert!(!app.syncing, "no worker was started");
        assert!(app.sync_rx.is_none());
    }
    // A finished pull is not folded into it either: the items the server holds
    // would each be given a new identity, as if none were known yet.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        let before = app.model.events.len();
        corrupt_events(&mut state);

        let (tx, rx) = std::sync::mpsc::channel();
        let start = app
            .today
            .and_time(NaiveTime::from_hms_opt(8, 0, 0).unwrap());
        tx.send((
            caldav::SyncResult {
                events: vec![Event {
                    id: uuid::Uuid::new_v4(),
                    title: "Pulled into a broken calendar".into(),
                    start,
                    end: start + Duration::hours(1),
                    location: "Remote".into(),
                    color: Palette::Blue,
                    ..Event::default()
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
        app.syncing = true;

        app.poll_sync(&mut Edits::lend(&mut state));

        assert!(!app.syncing, "the drain still clears the in-flight flag");
        assert_eq!(
            app.model.events.len(),
            before,
            "nothing was reconciled into a calendar that does not read"
        );
        assert!(
            !app.model
                .events
                .iter()
                .any(|e| e.title == "Pulled into a broken calendar")
        );
    }
}
