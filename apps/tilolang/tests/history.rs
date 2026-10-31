//! A note's history in the Notes window as a person uses it (`std.history`): opened, the note
//! shows who changed it and when, an agent's run as one entry that opens to each change; and
//! each moment reads as how long ago it was, the window sleeping until that reads differently
//! and showing it anew when it does.
//!
//! With `TILO_HISTORY_SHOTS=<dir>` the test writes the note's window, light and dark, with the
//! run's entry closed and open.

use serde_json::Map;
use tilolang_tilo::action::resource_ids;
use ui::framework::testing::HostedApp;
use ui::time::testing::with_wall;

use crate::change_marks::{action_id, named, published_with, run, settle};
use crate::history_window::{after, shoot, sleeps_for, times};

const NOTES: &str = "so.tilo.lang.notes";

#[test]
fn a_notes_history_shows_who_changed_it_and_when_and_reads_anew_as_time_passes() {
    let _settled = ui::motion::testing::settled();
    let mut app = HostedApp::open(crate::support::installed("notes"), "ada").unwrap();
    settle(&mut app);
    let root = app.read(|doc| resource_ids(doc, NOTES)).remove(0);
    let notebook = format!("tl:{NOTES}:{root}");
    let new_note = action_id(&mut app, &notebook, "New note");
    let made = ui::time::SystemTime::now();
    app.invoke(None, None, &notebook, &new_note, &Map::new(), false)
        .unwrap();
    settle(&mut app);
    let note = published_with(&mut app, &notebook, "Retitle");
    let retitle = action_id(&mut app, &note, "Retitle");
    let titled = |title: &str| {
        let mut args = Map::new();
        args.insert("title".to_owned(), title.into());
        args
    };
    app.invoke(None, None, &note, &retitle, &titled("Groceries"), false)
        .unwrap();
    // An agent's run renames it three times: one entry of three changes.
    let (agent, ambient) = run();
    for title in ["Shopping", "Shopping list", "Weekly shop"] {
        app.invoke(
            Some(agent.clone()),
            Some(&ambient),
            &note,
            &retitle,
            &titled(title),
            false,
        )
        .unwrap();
    }
    let done = ui::time::SystemTime::now();
    settle(&mut app);

    // Opened, a moment after: the note's history, newest first, each entry saying when.
    let soon = after(done, 1);
    let shown = with_wall(soon, || {
        let row = named(&mut app, "SwipeRow", "Weekly shop");
        app.harness()
            .click_at(row[0] + row[2] / 4.0, row[1] + row[3] / 2.0);
        settle(&mut app);
        times(&mut app)
    });
    assert_eq!(shown.len(), 2, "the run's entry and Ada's own: {shown:?}");
    // The window sleeps until a moment it shows reads differently: under a minute and a half
    // after it was made (the first count a just-made moment moves to), and not at once.
    let sleeps = with_wall(soon, || sleeps_for(&mut app)).expect("the window waits to read anew");
    let made_ago = soon.duration_since(made).unwrap();
    assert!(
        sleeps > ui::time::Duration::ZERO
            && sleeps <= ui::time::Duration::from_secs(90).saturating_sub(made_ago),
        "the window wakes when the moment's text next changes: {sleeps:?}"
    );
    with_wall(soon, || {
        shoot(&mut app, "notes-history", settle);
        let show = named(&mut app, "Button", "Show changes");
        app.harness()
            .click_at(show[0] + show[2] / 2.0, show[1] + show[3] / 2.0);
        settle(&mut app);
        assert_eq!(
            times(&mut app).len(),
            5,
            "opened, the run's entry shows each change's moment"
        );
        shoot(&mut app, "notes-history-open", settle);
    });

    // Three minutes on, the deadline is due: the frame it wakes for shows each moment anew.
    let later = after(made, 180);
    let anew = with_wall(later, || {
        let due = app
            .harness()
            .frame_schedule()
            .next_wake_deadline
            .is_some_and(|deadline| deadline <= ui::time::now());
        assert!(
            due,
            "the window's deadline is due once the text reads differently"
        );
        settle(&mut app);
        times(&mut app)
    });
    assert!(
        anew.iter().all(|now| !shown.contains(now)),
        "every moment reads anew: {shown:?} then {anew:?}"
    );
}
