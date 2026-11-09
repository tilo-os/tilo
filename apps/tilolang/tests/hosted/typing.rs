//! A person's typing into a bound field is one change of the record: it stays open while they
//! keep typing, and lands at a boundary — the field losing the keyboard, a pause, another field,
//! or the window closing. Every keystroke is still theirs to see at once, and no change of it is
//! lost when the window goes away.

use super::*;

/// How many changes the workspace's record holds: what the person's steps are, one per change,
/// and what a typing burst must not add one of per keystroke.
fn changes(session: &Session) -> usize {
    let hosted = session
        .host
        .instances()
        .next()
        .expect("the window's instance");
    hosted
        .record()
        .doc()
        .committed_document()
        .get_changes(&[])
        .len()
}

/// The store of the window's instance.
fn store(session: &Session) -> tilolang_tilo::store::TiloStore {
    let host = session
        .host
        .instances()
        .next()
        .expect("the window's instance")
        .instance()
        .host()
        .expect("the instance's store");
    host.store().clone()
}

/// The instance's store takes a pause of `pause` after the person's last keystroke to end the
/// change they are making: a test crosses it with a frame instead of waiting one out.
pub(super) fn after_a_pause(session: &Session, pause: std::time::Duration) {
    store(session).set_edit_pause(pause);
}

/// A person at a Notes window of the notebook the record `chunks` holds, as one opens it again
/// after closing it: what a window kept of a workspace they had open before.
fn notes_over(route: &RouteSpec, chunks: tilolang_tilo::chunk::Chunks) -> Session {
    let (sent, inbox): (Sent, Inbox) = (Rc::default(), Rc::default());
    let manager = CollaborationManager::with_peer_transport(Broker {
        route: route.clone(),
        sent: Rc::clone(&sent),
        inbox: Rc::clone(&inbox),
        joined: None,
    });
    Session::over(
        manager,
        route,
        NOTES,
        "ada",
        chunks,
        None,
        (Box::new(inbox), sent),
        |hosted| {
            Window::Harness(Box::new(FrameHarness::new(
                ui::State::new(),
                Box::new(hosted),
                VIEWPORT,
            )))
        },
    )
}

/// "+", a title typed, and a click in the body: the note is made, its title is a change of its
/// own, and the keyboard stands where a person types next.
fn new_note_with_title(session: &mut Session, title: &str) {
    let before = changes(session);
    let plus = session.new_note_button();
    session.window.click_at(plus.0, plus.1);
    session.settle();
    session.window.type_text(title);
    session.settle();
    let body = session.body();
    session.window.click_at(body.0, body.1);
    session.settle();
    assert_eq!(
        changes(session),
        before + 2,
        "the note is one change and its title another"
    );
}

/// The keyboard moved to the note's title: what a person clicking it does, and what ends the
/// change their typing in the body makes.
fn click_title(session: &mut Session) {
    let title = session.field("Untitled");
    session.window.click_at(title.0, title.1);
    session.settle();
}

/// Twenty keystrokes into a note's body are one change: the title taking the keyboard lands it,
/// and one undo takes the whole burst back.
fn typing_a_body_is_one_change_until_the_field_loses_the_keyboard(mut session: Session) {
    new_note_with_title(&mut session, "Groceries");
    let before = changes(&session);
    session.window.type_text("twenty characters in");
    session.settle();
    assert_eq!(
        changes(&session),
        before,
        "typing records no change while the person is still typing"
    );
    assert_eq!(
        body_text(&mut session).trim_end(),
        "twenty characters in",
        "the person sees every letter at once, before any change is recorded"
    );
    assert!(
        store(&session).edit_open(),
        "the store holds the change the person is making open"
    );
    click_title(&mut session);
    assert_eq!(
        changes(&session),
        before + 1,
        "the twenty keystrokes are one change, not twenty"
    );
    assert!(
        !store(&session).edit_open(),
        "the change landed, and none is open"
    );
    assert_eq!(
        body_text(&mut session).trim_end(),
        "twenty characters in",
        "what the person typed is the note's body"
    );
    session.window.chord(&["ctrl"], "z");
    session.settle();
    assert_eq!(
        body_text(&mut session).trim_end(),
        "",
        "one undo takes the whole burst back"
    );
    session.window.chord(&["ctrl", "shift"], "Z");
    session.settle();
    assert_eq!(
        body_text(&mut session).trim_end(),
        "twenty characters in",
        "one redo brings it back"
    );
}

#[test]
fn typing_a_body_is_one_change_in_the_frame_harness() {
    typing_a_body_is_one_change_until_the_field_loses_the_keyboard(Session::notes());
}

#[test]
fn typing_a_body_is_one_change_in_the_live_window() {
    typing_a_body_is_one_change_until_the_field_loses_the_keyboard(Session::live_notes());
}

/// A pause in the typing lands the change it was making: what is typed after the pause is a
/// change of its own, and each undo takes back one burst.
fn a_pause_ends_the_change_the_person_is_making(mut session: Session) {
    new_note_with_title(&mut session, "Groceries");
    after_a_pause(&session, std::time::Duration::ZERO);
    let before = changes(&session);
    session.window.type_text("milk");
    session.settle();
    assert_eq!(
        changes(&session),
        before + 1,
        "the pause lands what was typed"
    );
    session.window.type_text(" and eggs");
    session.settle();
    assert_eq!(
        changes(&session),
        before + 2,
        "what is typed after the pause is a second change"
    );
    session.window.chord(&["ctrl"], "z");
    session.settle();
    assert_eq!(
        body_text(&mut session).trim_end(),
        "milk",
        "undo takes back the burst the pause ended, not the one before it"
    );
}

#[test]
fn a_pause_ends_the_change_the_person_is_making_in_the_frame_harness() {
    a_pause_ends_the_change_the_person_is_making(Session::notes());
}

/// Turning to another field ends the change: the title is a change, the body another, and
/// neither is split into its keystrokes.
fn title_then_body_is_two_changes(mut session: Session) {
    let before = changes(&session);
    // "+", a title typed, then the body: turning to the body lands the title.
    let plus = session.new_note_button();
    session.window.click_at(plus.0, plus.1);
    session.settle();
    session.window.type_text("Groceries");
    session.settle();
    let body = session.body();
    session.window.click_at(body.0, body.1);
    session.settle();
    assert_eq!(
        changes(&session),
        before + 2,
        "the note is one change and the title another"
    );
    session.window.type_text("milk");
    session.settle();
    assert_eq!(
        changes(&session),
        before + 2,
        "the body is still being typed"
    );
    click_title(&mut session);
    assert_eq!(
        changes(&session),
        before + 3,
        "the body is a change of its own"
    );
    assert_eq!(
        open_note(&mut session),
        ("Groceries".to_owned(), "milk".to_owned()),
        "both fields hold what was typed into them"
    );
}

#[test]
fn typing_the_title_and_then_the_body_is_two_changes() {
    title_then_body_is_two_changes(Session::notes());
}

/// A window closed with the keyboard still in a field keeps what was typed: the change the person
/// was making lands as their record is written, and opening the workspace again reads it as one
/// change.
#[test]
fn what_was_typed_when_the_window_closed_is_one_change_of_the_record() {
    let mut session = Session::notes();
    new_note_with_title(&mut session, "Groceries");
    // The window closes before the pause would end the change: only the close can land it.
    after_a_pause(&session, std::time::Duration::from_secs(600));
    let before = changes(&session);
    session.window.type_text("twenty characters in");
    session.settle();
    assert_eq!(
        changes(&session),
        before,
        "nothing is recorded while typing"
    );
    // The window goes away with the keyboard still in the body: the session flushes the
    // instance's record before it ends it (`Hosted::flush`).
    for hosted in session.host.instances_mut() {
        hosted.flush();
    }
    assert_eq!(
        changes(&session),
        before + 1,
        "what was typed reached the record as one change"
    );
    let route = session
        .host
        .instances()
        .next()
        .expect("the window's instance")
        .record()
        .route()
        .clone();
    let chunks = session
        .host
        .instances()
        .next()
        .expect("the window's instance")
        .record()
        .chunks()
        .clone();
    drop(session);
    // The workspace opened again reads what the closed window wrote.
    let mut again = notes_over(&route, chunks);
    assert_eq!(changes(&again), before + 1, "one change, not twenty");
    let row = note_row(&mut again, "Groceries");
    again.window.click_at(row.0, row.1);
    again.settle();
    assert_eq!(
        body_text(&mut again).trim_end(),
        "twenty characters in",
        "the note holds what was typed before the window closed"
    );
}
