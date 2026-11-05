//! A person's own state of a workspace: the window and lens state they pick in a window and what
//! they have seen are kept in their own record of it, so what they picked is still theirs after a
//! restart — and nobody else's record holds any of it.

use super::*;
use tilolang_tilo::personal::{Keep, Personal};
use tilolang_tilo::runtime::seen::SeenMark;
use tilolang_tilo::runtime::view_state::StateLayer;
use ui::prelude::TestApp;

/// The value of the place the person picked, as the runtime holds a filter: Today.
fn today_filter() -> RtValue {
    RtValue::Tuple(vec![
        RtValue::text("today"),
        RtValue::text("Today"),
        RtValue::Opt(Some(Box::new(RtValue::text("star")))),
        RtValue::text("is:today"),
        RtValue::Bool(true),
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

/// The app the person's state is kept for, and their own record of the workspace as the app's own
/// window opens it.
fn record(
    app: &str,
    person: &str,
) -> (
    ui::prelude::TestApp<crate::app::TlApp>,
    ui::prelude::PersonRecord<Personal>,
) {
    crate::app::builtin::choose(app);
    let mut app_window = TestApp::<crate::app::TlApp>::for_person_in(person, ROOM)
        .expect("the app opens in the workspace");
    let record = app_window
        .person::<Personal>()
        .expect("the person has a record of the workspace");
    (app_window, record)
}

#[test]
fn the_place_a_person_picks_in_a_window_is_theirs_after_a_restart_and_no_one_elses() {
    let (_app, mine) = record("tasks", &ada());
    let mut first = Workspace::personal("tasks", &ada(), mine.clone());
    assert_eq!(
        first.composed().props.get("overview_title"),
        Some(&Value::text("Inbox")),
        "the window starts on the inbox"
    );

    // The person picks Today in the sidebar.
    let today = first.key_of("ui.NavItem", "label", "Today");
    first.fire(&today, "select", None);
    assert_eq!(
        first.composed().props.get("overview_title"),
        Some(&Value::text("Today"))
    );

    // Their own record keeps the place their window shows.
    let kept = mine.with(Clone::clone);
    assert!(
        kept.slots
            .iter()
            .any(|slot| slot.layer == StateLayer::Window && slot.value == today_filter()),
        "{kept:?}"
    );

    // A restart: a new window over the same record opens where they left it.
    let again = Workspace::personal("tasks", &ada(), mine);
    assert_eq!(
        again.composed().props.get("overview_title"),
        Some(&Value::text("Today")),
        "the place they picked is theirs again"
    );
}

#[test]
fn the_channel_a_person_opens_is_theirs_after_a_restart() {
    let (_app, mine) = record("chat", &ada());
    let mut first = Workspace::personal_seeded("chat", &ada(), mine.clone());
    let chosen = first.key_of("ui.NavItem", "label", "general");
    first.fire(&chosen, "select", None);

    // A lens is where the person is in the window: their record keeps it as one.
    let kept = mine.with(Clone::clone);
    assert!(
        kept.slots
            .iter()
            .any(|slot| slot.layer == StateLayer::Lens && slot.value == RtValue::text("/general")),
        "{kept:?}"
    );

    // A restart opens the sidebar on the channel they had open.
    let again = Workspace::personal_seeded("chat", &ada(), mine);
    assert!(
        node_with(&again.composed(), "ui.NavList", "selected", "/general").is_some(),
        "the sidebar opens on the channel they picked"
    );
}

#[test]
fn what_a_person_saw_is_theirs_after_a_restart_and_not_another_persons() {
    let (_app, mine) = record("tasks", &ada());
    let resource = "tl:so.tilo.lang.tasks:inbox".to_owned();
    let mark = SeenMark {
        change: "ab".repeat(32),
        at: "2026-11-04T12:00:00Z".to_owned(),
    };
    mine.apply(&Keep::Seen {
        resource: resource.clone(),
        mark: Some(mark.clone()),
    })
    .expect("their record keeps what they saw");

    // A window over their record reads their mark.
    let window = Workspace::personal("tasks", &ada(), mine.clone());
    let host = window.window.current_host().expect("the host started");
    assert_eq!(host.runtime().seen(&ada(), &resource), Some(mark));

    // Another person of the workspace keeps a record of their own, with none of it.
    let (_theirs_app, theirs) = record("tasks", &bo());
    assert_ne!(
        theirs.record().route(),
        mine.record().route(),
        "each person's own record is theirs"
    );
    assert!(theirs.with(|state| state.seen.is_empty()));
    let other = Workspace::personal("tasks", &bo(), theirs);
    let host = other.window.current_host().expect("the host started");
    assert_eq!(host.runtime().seen(&ada(), &resource), None);
}
