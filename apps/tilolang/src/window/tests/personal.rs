//! A person's own state of a workspace: the view state they pick in a window and what they have
//! seen are kept under their own entries of the workspace's one record of it, so what they picked
//! is still theirs after a restart, reaches their other devices, and is nobody else's to write.

use super::*;
use tilolang_tilo::personal::{self, Keep, Personal};
use tilolang_tilo::runtime::seen::SeenMark;
use ui::automerge::{ObjType, ROOT, ReadDoc as _, Value as AmValue};
use ui::prelude::TestApp;

/// The value of the place the person picked, as the runtime holds a filter: Today. A filter is
/// its key, label, icon, what choosing it asks and whether it is a toggle.
fn today_filter() -> RtValue {
    RtValue::Tuple(vec![
        RtValue::text("today"),
        RtValue::text("Today"),
        RtValue::Opt(Some(Box::new(RtValue::text("star")))),
        RtValue::text("is:today"),
        RtValue::Bool(true),
    ])
}

/// The workspace the windows are open in.
const ROOM: &str = "00000000-0000-4000-8000-00000000000a";

/// Two people of it, by the hex of their user root keys.
fn ada() -> String {
    "ab".repeat(32)
}

fn bo() -> String {
    "cd".repeat(32)
}

/// The app the person's state is kept for, and the workspace's record of each person's own state,
/// as the app's own window opens it.
fn record(
    app: &str,
    person: &str,
) -> (
    ui::prelude::TestApp<crate::app::TlApp>,
    WorkspaceRecord<Personal>,
) {
    crate::app::builtin::choose(app);
    let mut app_window = TestApp::<crate::app::TlApp>::for_person_in(person, ROOM)
        .expect("the app opens in the workspace");
    let record = app_window
        .shared_workspace::<Personal>(personal::OWNER)
        .expect("the workspace has a record of each person's own state");
    (app_window, record)
}

/// The keys held under `path` in the record's own document, in order.
fn kept_keys(record: &WorkspaceRecord<Personal>, path: &[&str]) -> Vec<String> {
    record
        .record()
        .read(|doc| {
            let mut at = ROOT.clone();
            for key in path {
                at = match doc.get(&at, *key) {
                    Ok(Some((AmValue::Object(ObjType::Map), id))) => id,
                    _ => return Vec::new(),
                };
            }
            let mut keys: Vec<String> = doc.keys(&at).collect();
            keys.sort();
            keys
        })
        .unwrap_or_default()
}

#[test]
fn the_place_a_person_picks_in_a_window_is_theirs_after_a_restart_and_no_one_elses() {
    let (_app, mine) = record("tasks", &ada());
    let mut first = Workspace::personal("tasks", &ada(), mine.clone());
    assert_eq!(
        first.composed().props.get("title"),
        Some(&Value::text("Inbox")),
        "the window starts on the inbox"
    );

    // The person picks Today in the sidebar.
    let today = first.key_of("ui.NavItem", "label", "Today");
    first.fire(&today, "select", None);
    assert_eq!(
        first.composed().props.get("title"),
        Some(&Value::text("Today"))
    );

    // Their entries of the workspace's record keep the place their window shows, under their
    // person and the window that declared it.
    let windows = kept_keys(&mine, &["$personal", &ada(), "views"]);
    assert_eq!(windows.len(), 1, "{windows:?}");
    let picked: Vec<String> = kept_keys(&mine, &["$personal", &ada(), "views", &windows[0]]);
    assert!(!picked.is_empty(), "the place is kept as view state");
    let kept = mine.get().of(&ada());
    assert!(
        kept.slots.iter().any(|slot| slot.value == today_filter()),
        "{kept:?}"
    );

    // A restart: a new window over the same record opens where they left it.
    let again = Workspace::personal("tasks", &ada(), mine);
    assert_eq!(
        again.composed().props.get("title"),
        Some(&Value::text("Today")),
        "the place they picked is theirs again"
    );
}

#[test]
fn a_window_shows_the_place_the_record_gives_it_after_it_drew() {
    let (_app, mine) = record("tasks", &ada());
    // A window opened while the record holds nothing yet shows the inbox.
    let mut later = Workspace::personal("tasks", &ada(), mine.clone());
    assert_eq!(
        later.composed().props.get("title"),
        Some(&Value::text("Inbox"))
    );

    // The record gains the place while that window is open: the person picked it in another
    // window of the record, or on another device of theirs.
    let mut first = Workspace::personal("tasks", &ada(), mine.clone());
    let today = first.key_of("ui.NavItem", "label", "Today");
    first.fire(&today, "select", None);
    assert!(
        mine.get()
            .of(&ada())
            .slots
            .iter()
            .any(|slot| slot.value == today_filter()),
        "the record keeps the place the person picked"
    );

    // The window that had drawn the inbox shows it, without anything else happening.
    later.frame();
    assert_eq!(
        later.composed().props.get("title"),
        Some(&Value::text("Today")),
        "the place the record gives back reaches a window already drawn"
    );
}

#[test]
fn the_channel_a_person_opens_is_theirs_after_a_restart() {
    let (_app, mine) = record("chat", &ada());
    let mut first = Workspace::personal_seeded("chat", &ada(), mine.clone());
    let chosen = first.key_of("ui.NavItem", "label", "general");
    first.fire(&chosen, "select", None);

    // Where the person is in the window is view state: their entries keep it as such.
    let windows = kept_keys(&mine, &["$personal", &ada(), "views"]);
    assert_eq!(windows.len(), 1, "{windows:?}");
    let opened: Vec<String> = kept_keys(&mine, &["$personal", &ada(), "views", &windows[0]]);
    assert!(!opened.is_empty(), "the channel is kept as view state");
    assert!(
        mine.get()
            .of(&ada())
            .slots
            .iter()
            .any(|slot| slot.value == RtValue::text("/general")),
        "{:?}",
        mine.get()
    );

    // A restart opens the sidebar on the channel they had open.
    let again = Workspace::personal_seeded("chat", &ada(), mine);
    assert!(
        node_with(&again.composed(), "ui.NavList", "selected", "/general").is_some(),
        "the sidebar opens on the channel they picked"
    );
}

#[test]
fn what_a_person_saw_is_theirs_on_their_own_entries_and_not_another_persons() {
    let (_app, mine) = record("tasks", &ada());
    let resource = "tl:so.tilo.lang.tasks:inbox".to_owned();
    let mark = SeenMark {
        change: "ab".repeat(32),
        at: "2026-11-04T12:00:00Z".to_owned(),
    };
    mine.invoke(
        &Invoke::<Personal>::apply(&Keep::Seen {
            person: ada(),
            resource: resource.clone(),
            mark: Some(mark.clone()),
        })
        .expect("the change encodes"),
    )
    .expect("their entries keep what they saw");
    assert_eq!(
        kept_keys(&mine, &["$personal", &ada(), "seen"]),
        vec![resource.clone()],
        "the mark is kept under the person who saw it"
    );

    // A window over the record reads their mark.
    let window = Workspace::personal("tasks", &ada(), mine.clone());
    let host = window.window.current_host().expect("the host started");
    assert_eq!(host.runtime().seen(&ada(), &resource), Some(mark));

    // Another person of the workspace opens the same record: it is one record, theirs to read
    // and not to write.
    let (_theirs_app, theirs) = record("tasks", &bo());
    assert_eq!(
        theirs.record().route(),
        mine.record().route(),
        "one record of each person's state for the workspace"
    );
    theirs
        .record()
        .merge_peer_document(mine.record().document().expect("the record reads"))
        .expect("the other person's copy takes the record in");
    assert!(
        theirs.get().of(&bo()).is_empty(),
        "none of it is Bo's own state"
    );

    // Bo's window shows Bo's state, never Ada's mark.
    let other = Workspace::personal("tasks", &bo(), theirs.clone());
    let host = other.window.current_host().expect("the host started");
    assert_eq!(host.runtime().seen(&ada(), &resource), None);

    // Bo cannot keep a mark as Ada's.
    let refusal = theirs
        .invoke(
            &Invoke::<Personal>::apply(&Keep::Seen {
                person: ada(),
                resource: resource.clone(),
                mark: None,
            })
            .expect("the change encodes"),
        )
        .expect_err("a person keeps only their own state");
    assert!(refusal.to_string().contains("their own state"), "{refusal}");
    assert!(
        mine.get()
            .of(&ada())
            .seen
            .iter()
            .any(|seen| seen.resource == resource),
        "Ada's mark stays hers"
    );
}
