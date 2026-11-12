//! A person's typing into a bound field: every letter is a change of the record the moment it is
//! typed — kept with the record, and on its way to every peer — and the burst of them is one step
//! of the person's: one entry in History, one undo.

use super::*;
use tilolang_tilo::chunk::{Chunks, Stamps};
use tilolang_tilo::host::Host;
use tilolang_tilo::runtime::Principal;
use tilolang_tilo::runtime::Value;
use tilolang_tilo::runtime::value::ResourceRef;

/// How many changes the workspace's record holds: what the person did, one per letter typed.
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

/// A person at a Notes window of the notebook the record `chunks` holds, as one opens it again
/// after closing it: what a window kept of a workspace they had open before.
fn notes_over(route: &RouteSpec, chunks: Chunks) -> Session {
    let mut session = Session::brokered(
        route,
        NOTES,
        "ada",
        "en-US",
        chunks,
        None,
        Window::in_harness,
    );
    session.settle();
    session
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

/// The window's record: the workspace as this device holds it.
fn record(session: &Session) -> Chunks {
    session
        .host
        .instances()
        .next()
        .expect("the window's instance")
        .record()
        .chunks()
        .clone()
}

/// The record's heads now: what the changes a peer takes start after.
fn marks(session: &Session) -> Stamps {
    session
        .host
        .instances()
        .next()
        .expect("the window's instance")
        .record()
        .marks()
}

/// The changes the window's record holds past `marks`, as the workspace hands them to a peer.
fn changes_since(session: &Session, marks: &Stamps) -> Vec<u8> {
    session
        .host
        .instances()
        .next()
        .expect("the window's instance")
        .record()
        .changes_since(marks)
}

/// How long a pause without a keystroke ends the burst the person is typing.
fn after_a_pause(session: &Session, pause: std::time::Duration) {
    store(session).set_edit_pause(pause);
}

/// Another device of the workspace, reading the record as it receives it: what a peer sees of what
/// the person types, one sync turn at a time.
struct Peer {
    host: Host,
    /// This device's copy of the record, as the changes arrive.
    doc: automerge::AutoCommit,
    /// What this device has taken of the window's record.
    taken: Stamps,
}

impl Peer {
    /// This device over the record `session`'s window holds now, running the notes package the
    /// window runs.
    fn over(session: &Session) -> Self {
        let held = record(session);
        let doc = held.first().expect("the record is held").clone();
        let host = Host::new(&doc, Principal::new("bo"), || None).expect("the peer's host");
        host.load_bundle(&package(package_of(NOTES)))
            .expect("the notes package");
        host.wait_index();
        Self {
            host,
            doc,
            taken: marks(session),
        }
    }

    /// Take what the window's record holds past what this device took, and bring its store up to
    /// date with it: one sync turn. Whether anything arrived.
    fn take(&mut self, session: &Session) -> bool {
        let changes = changes_since(session, &self.taken);
        self.taken = marks(session);
        if changes.is_empty() {
            return false;
        }
        self.doc
            .load_incoming_whole(&changes)
            .expect("a peer takes every change");
        self.host.sync_from(&self.doc);
        self.host.wait_index();
        true
    }

    /// The note the person types into: the last of the notebook's notes, as this device reads it.
    fn note(&self) -> ResourceRef {
        let notebook = self.host.root_of(NOTES).expect("the notebook");
        let notes = self
            .host
            .runtime()
            .read(&notebook, "notes")
            .unwrap_or_else(|error| panic!("the notebook lists its notes: {error:?}"));
        let Value::List(items) = notes else {
            panic!("a collection: {notes:?}");
        };
        items
            .into_iter()
            .rev()
            .find_map(|item| match item {
                Value::Ref(note) => Some(std::rc::Rc::unwrap_or_clone(note.target)),
                _ => None,
            })
            .expect("the note the person made")
    }

    /// What this device reads of the note's body.
    fn body(&self) -> String {
        match self
            .host
            .runtime()
            .read(&self.note(), "body")
            .expect("the note's body")
        {
            Value::Rich(rich) => rich
                .blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            Value::Text(text) => text,
            other => panic!("a body: {other:?}"),
        }
    }

    /// The gestures the note's versions name, oldest first: the acts its History folds them
    /// into, as this device reads them from the record.
    fn gestures(&self) -> Vec<Option<String>> {
        self.host
            .store()
            .versions(&self.note())
            .expect("the note's history")
            .iter()
            .map(|version| version.gesture.clone())
            .collect()
    }
}

/// How many acts of the person the note's History counts: the page the window shows says so under
/// the entry, so a burst of any number of letters counts as one of them.
fn history_acts(session: &mut Session) -> usize {
    let page = session.shown();
    page.iter()
        .find_map(|text| {
            text.strip_suffix(" changes")
                .and_then(|count| count.parse::<usize>().ok())
                .or_else(|| (text == "1 change").then_some(1))
        })
        .unwrap_or_else(|| panic!("the note's History counts what was done to it: {page:?}"))
}

/// The steps the person `person` has in effect in the record, newest first, each the number of
/// changes it takes back: one for the burst of their typing, however many letters it was.
fn steps(session: &Session, person: &str) -> Vec<usize> {
    let scope = ui::history_source::steps::Scope::Invoker(person.to_owned());
    let doc = record(session).first().expect("the record is held").clone();
    ui::history_source::steps::steps_in_effect(&doc, &|version| scope.owns(version), 16)
        .iter()
        .map(Vec::len)
        .collect()
}

/// "+", a title typed, and a click in the body: the note is made, its title is a burst of its own,
/// and the keyboard stands where a person types next.
fn new_note_with_title(session: &mut Session, title: &str) {
    let plus = session.new_note_button();
    session.window.click_at(plus.0, plus.1);
    session.settle();
    session.window.type_text(title);
    session.settle();
    let body = session.body();
    session.window.click_at(body.0, body.1);
    session.settle();
}

/// The keyboard moved to the note's title: what a person clicking it does, and what ends the burst
/// their typing in the body is.
fn click_title(session: &mut Session) {
    let title = session.field("Untitled");
    session.window.click_at(title.0, title.1);
    session.settle();
}

/// Twenty keystrokes into a note's body are twenty changes of the record — every letter is kept
/// and sent as it is typed, so nothing typed is only in memory — and one step of the person's: one
/// entry in History, and one undo takes the whole burst back.
fn typing_a_body_is_one_step_of_however_many_letters(mut session: Session) {
    new_note_with_title(&mut session, "Groceries");
    let before = changes(&session);
    let acts = history_acts(&mut session);
    let steps_before = steps(&session, "ada");
    session.window.type_text("twenty characters in");
    session.settle();
    assert_eq!(
        changes(&session),
        before + "twenty characters in".len(),
        "every letter is a change of the record as it is typed"
    );
    assert_eq!(
        body_text(&mut session).trim_end(),
        "twenty characters in",
        "the person sees every letter at once"
    );
    let steps_typed = steps(&session, "ada");
    assert_eq!(
        steps_typed.len(),
        steps_before.len() + 1,
        "the burst is one step of the person's: {steps_typed:?}"
    );
    assert_eq!(
        steps_typed[0],
        "twenty characters in".len(),
        "the one step holds every letter of the burst: {steps_typed:?}"
    );
    assert_eq!(
        history_acts(&mut session),
        acts + 1,
        "History counts the burst as one act, not one per letter"
    );
    click_title(&mut session);
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
fn typing_a_body_is_one_step_of_however_many_letters_in_the_frame_harness() {
    typing_a_body_is_one_step_of_however_many_letters(Session::notes());
}

#[test]
fn typing_a_body_is_one_step_of_however_many_letters_in_the_live_window() {
    typing_a_body_is_one_step_of_however_many_letters(Session::live_notes());
}

/// A pause in the typing ends the burst: what is typed after it is a step of its own, and each
/// undo takes back one burst.
fn a_pause_ends_the_burst_the_person_is_typing(mut session: Session) {
    new_note_with_title(&mut session, "Groceries");
    // A pause a test can cross: the window's frames between two bursts are longer than this.
    after_a_pause(&session, std::time::Duration::from_millis(200));
    session.window.type_text("milk");
    session.settle();
    let steps_typed = steps(&session, "ada");
    assert_eq!(steps_typed[0], 4, "the burst is one step: {steps_typed:?}");
    std::thread::sleep(std::time::Duration::from_millis(400));
    session.window.type_text(" and eggs");
    session.settle();
    let steps_typed = steps(&session, "ada");
    assert_eq!(
        steps_typed[0], 9,
        "what is typed after the pause is a step of its own: {steps_typed:?}"
    );
    assert_eq!(steps_typed[1], 4, "{steps_typed:?}");
    session.window.chord(&["ctrl"], "z");
    session.settle();
    assert_eq!(
        body_text(&mut session).trim_end(),
        "milk",
        "undo takes back the burst the pause ended, not the one before it"
    );
}

#[test]
fn a_pause_ends_the_burst_the_person_is_typing_in_the_frame_harness() {
    a_pause_ends_the_burst_the_person_is_typing(Session::notes());
}

/// Turning to another field ends the burst: the title is a step, the body another, and neither is
/// split into its letters.
fn title_then_body_is_two_steps(mut session: Session) {
    let plus = session.new_note_button();
    session.window.click_at(plus.0, plus.1);
    session.settle();
    session.window.type_text("Groceries");
    session.settle();
    let body = session.body();
    session.window.click_at(body.0, body.1);
    session.settle();
    let steps_typed = steps(&session, "ada");
    assert_eq!(steps_typed[0], 9, "the title is one step: {steps_typed:?}");
    session.window.type_text("milk");
    session.settle();
    assert_eq!(
        steps(&session, "ada")[0],
        4,
        "the body's burst is a step of its own"
    );
    click_title(&mut session);
    assert_eq!(
        open_note(&mut session),
        ("Groceries".to_owned(), "milk".to_owned()),
        "both fields hold what was typed into them"
    );
}

#[test]
fn typing_the_title_and_then_the_body_is_two_steps() {
    title_then_body_is_two_steps(Session::notes());
}

/// A window dropped with the keyboard still in a field keeps every letter: each was a change of the
/// record as it was typed, so another device reads the whole burst — one step as far as history
/// and undo go.
#[test]
fn a_window_dropped_mid_burst_keeps_every_letter() {
    let mut session = Session::notes();
    new_note_with_title(&mut session, "Groceries");
    // The window goes away before any pause would end the burst: only the record holds the letters.
    after_a_pause(&session, std::time::Duration::from_secs(600));
    session.window.type_text("twenty characters in");
    session.settle();
    let route = session
        .host
        .instances()
        .next()
        .expect("the window's instance")
        .record()
        .route()
        .clone();
    let held = record(&session);
    // What the record holds, as another device of the workspace reads it.
    let mut reader = Peer::over(&session);
    assert_eq!(
        reader.body().trim_end(),
        "twenty characters in",
        "every letter typed is the record's before any window writes it"
    );
    // The window goes away without a flush: nothing writes the store's memory to the record now.
    drop(session);
    let mut again = notes_over(&route, held);
    assert_eq!(
        steps(&again, "ada")[0],
        "twenty characters in".len(),
        "the burst the window did not outlive is one step"
    );
    assert!(
        !reader.take(&again),
        "the workspace opened again holds the burst the window typed"
    );
    assert_eq!(
        reader.body().trim_end(),
        "twenty characters in",
        "the workspace opened again reads every letter"
    );
    again.window.chord(&["ctrl"], "z");
    again.settle();
    assert!(reader.take(&again), "the undo reached the reader");
    assert_eq!(
        reader.body().trim_end(),
        "",
        "one undo takes the whole burst back"
    );
}

/// Each letter the person types is in another device's replica after one sync turn, and the burst
/// is one step there too: History lists one entry, and one undo takes the whole of it back on both.
#[test]
fn a_peer_sees_each_letter_and_one_undo_takes_the_burst_back_on_both() {
    let mut session = Session::notes();
    new_note_with_title(&mut session, "Groceries");
    let mut peer = Peer::over(&session);
    let acts = history_acts(&mut session);
    let before = peer.gestures();
    let mut typed = String::new();
    for letter in "twenty characters in".chars() {
        typed.push(letter);
        session.window.type_text(&letter.to_string());
        session.settle();
        assert!(peer.take(&session), "the letter reached the peer");
        assert_eq!(
            peer.body().trim_end(),
            typed.trim_end(),
            "the peer reads what the person typed, letter by letter"
        );
    }
    assert_eq!(
        body_text(&mut session).trim_end(),
        typed,
        "the person's own window reads the same"
    );
    // Every letter is a version of the note in the peer's record, and all of them name one act:
    // the peer's History folds the burst into one entry, as the person's does.
    let gestures = peer.gestures();
    let typed_gestures = &gestures[before.len()..];
    assert_eq!(
        typed_gestures.len(),
        typed.len(),
        "the peer reads a version for every letter: {gestures:?}"
    );
    assert!(
        typed_gestures
            .iter()
            .all(|gesture| *gesture == typed_gestures[0])
            && typed_gestures[0].is_some(),
        "the letters name one act of the person's: {typed_gestures:?}"
    );
    assert_eq!(
        history_acts(&mut session),
        acts + 1,
        "the person's History counts the burst as one act"
    );
    // One undo on the person's device takes the whole burst back, and the peer reads that too.
    session.window.chord(&["ctrl"], "z");
    session.settle();
    assert_eq!(body_text(&mut session).trim_end(), "");
    assert!(peer.take(&session), "the undo reached the peer");
    assert_eq!(
        peer.body().trim_end(),
        "",
        "one undo takes the whole burst back on the peer too"
    );
    // And one redo brings it back, there as here.
    session.window.chord(&["ctrl", "shift"], "Z");
    session.settle();
    assert!(peer.take(&session), "the redo reached the peer");
    assert_eq!(
        peer.body().trim_end(),
        typed.trim_end(),
        "one redo brings it back"
    );
    assert_eq!(body_text(&mut session).trim_end(), typed);
}
