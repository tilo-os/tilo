//! A note's history in the Notes window as a person uses it (`std.history`): opened, the note
//! shows who changed it and when, an agent's run as one entry that opens to each change; and
//! each moment reads as how long ago it was, the window sleeping until that reads differently
//! and showing it anew when it does.
//!
//! Another app's change to the same note reads as that app, by its own name in the person's
//! language: Today files a note in Notes and the note's history says "Today", or "Heute".
//!
//! With `TILO_HISTORY_SHOTS=<dir>` the test writes the note's window, light and dark, with the
//! run's entry closed and open.

use serde_json::Map;
use tilolang_tilo::action::resource_ids;
use ui::framework::testing::HostedApp;
use ui::time::testing::with_wall;

use crate::change_marks::{action_id, named, published_with, run, settle, walk};
use crate::history_window::{after, shoot, sleeps_for, times};

const NOTES: &str = "so.tilo.lang.notes";
/// The app whose action files a note in Notes' notebook (`morning_cleanup`).
const TODAY: &str = "so.tilo.lang.today";

/// The names the note's history opens its steps with, newest first: who made each change.
fn said_by(app: &mut HostedApp) -> Vec<String> {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    let heading = all
        .iter()
        .filter(|node| {
            node.widget == "Text"
                && node
                    .semantics
                    .as_ref()
                    .is_some_and(|semantics| semantics.name.as_deref() == Some("History"))
        })
        .map(|node| node.rect[1])
        .fold(None, |least: Option<f64>, y| {
            Some(least.map_or(y, |held| held.min(y)))
        });
    all.into_iter()
        .filter(|node| node.widget == "Text")
        .filter(|node| heading.is_none_or(|heading| node.rect[1] >= heading))
        .filter_map(|node| node.semantics.as_ref()?.name.clone())
        .collect()
}

/// Today's call names a note in Notes' notebook, and the note's own history says so: the app
/// that changed another app's record is named by its name in the reader's language, never by
/// its principal id and never by part of one.
fn today_files_a_note(app: &mut HostedApp) {
    let root = app.read(|doc| resource_ids(doc, NOTES)).remove(0);
    let notebook = format!("tl:{NOTES}:{root}");
    let file = action_id(app, &notebook, "File a note");
    let mut args = Map::new();
    args.insert("title".to_owned(), "Morning: 2 done".into());
    args.insert("t".to_owned(), "morning".into());
    // The broker admits the app's invocation under the app's own principal: what Notes' record
    // keeps of it.
    let by_today = ui::tilo_actions::ActionInvoker::actor(format!("app:{TODAY}"))
        .with_kind(ui::tilo_actions::ActorKind::App);
    app.invoke(Some(by_today), None, &notebook, &file, &args, false)
        .expect("Today files a note");
    settle(app);
    // The note it made, opened on its own page, which reads its history.
    let row = named(app, "ListRow", "Morning: 2 done");
    app.harness()
        .click_at(row[0] + row[2] / 4.0, row[1] + row[3] / 2.0);
    settle(app);
}

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
    let note = published_with(&mut app, &notebook, "Rename…");
    let retitle = action_id(&mut app, &note, "Rename…");
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
        let row = named(&mut app, "ListRow", "Weekly shop");
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

#[test]
fn a_note_another_app_filed_reads_as_that_app_in_the_person_s_language() {
    let _settled = ui::motion::testing::settled();
    let mut app = HostedApp::open(crate::support::installed("notes"), "ada").unwrap();
    settle(&mut app);
    today_files_a_note(&mut app);
    let said = said_by(&mut app);
    assert_eq!(
        said.first().map(String::as_str),
        Some("Today"),
        "the change Today made reads as Today: {said:?}"
    );
    assert!(
        !said
            .iter()
            .any(|who| who.contains("app:") || who.starts_with("so.tilo")),
        "no principal id, whole or cut short, is shown: {said:?}"
    );
}

#[test]
fn the_same_note_reads_as_the_app_s_own_name_in_german() {
    let _settled = ui::motion::testing::settled();
    let mut app = HostedApp::open(crate::support::installed("notes"), "ada").unwrap();
    settle(&mut app);
    // The words of the apps this device has: Today's, as its package ships them. A session
    // carries an installed app's catalogue beside its own (`tilo_ui_host::i18n`), which is what
    // makes its name read in the person's language here.
    tilo_ui_host::i18n::add_shared_catalogues(&[(
        "de-DE".to_owned(),
        include_str!("../../tl/today/locales/de-DE.ftl").to_owned(),
    )]);
    ui::i18n::I18nManager::set_locale("de-DE");
    settle(&mut app);
    today_files_a_note(&mut app);
    let said = said_by(&mut app);
    assert_eq!(
        said.first().map(String::as_str),
        Some("Heute"),
        "the app's own name in German: {said:?}"
    );
}
