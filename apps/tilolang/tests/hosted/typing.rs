//! A person's typing into a bound field: every letter is written to the workspace the moment it
//! is typed, and one undo takes the burst back.

use super::*;
use tilolang_tilo::runtime::Value;
use tilolang_tilo::runtime::value::ResourceRef;

/// The note the person typed into last: the last of the notebook's notes, as the store reads it.
fn last_note(session: &Session) -> ResourceRef {
    let host = session.instance_host();
    let notebook = host.root_of(NOTES).expect("the notebook");
    let notes = host
        .runtime()
        .read(&notebook, "notes")
        .expect("the notebook lists its notes");
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

/// The versions of the note the person types into, oldest first.
fn versions(session: &Session) -> Vec<tilolang_tilo::runtime::store::Version> {
    session
        .instance_host()
        .store()
        .versions(&last_note(session))
        .expect("the note's history")
}

/// The steps the note's versions fold into, oldest first, each holding the versions one act of
/// the person's made: the versions of one gesture are the keystrokes of one burst.
fn steps(session: &Session) -> Vec<usize> {
    let mut steps: Vec<usize> = Vec::new();
    let mut previous: Option<tilolang_tilo::runtime::store::Version> = None;
    for version in versions(session) {
        let same = previous.as_ref().is_some_and(|last| {
            last.who == version.who
                && match (&last.gesture, &version.gesture) {
                    (Some(last), Some(now)) => last == now,
                    (None, None) => true,
                    _ => false,
                }
        });
        match steps.last_mut() {
            Some(step) if same => *step = step.saturating_add(1),
            _ => steps.push(1),
        }
        previous = Some(version);
    }
    steps
}

/// How many acts of the person the note's History counts: the window's History says so under the
/// entry, so a burst of any number of letters counts as one of them. The keyboard goes back to the
/// body after, where the person types on.
fn history_acts(session: &mut Session) -> usize {
    let page = session.history_shown();
    let body = session.body();
    session.window.click_at(body.0, body.1);
    session.settle();
    page.iter()
        .find_map(|text| {
            text.strip_suffix(" changes")
                .and_then(|count| count.parse::<usize>().ok())
                .or_else(|| (text == "1 change").then_some(1))
        })
        .unwrap_or_else(|| panic!("the note's History counts what was done to it: {page:?}"))
}

/// "+", a title typed, and a click in the body: the note is made, its title is a burst of its own,
/// and the keyboard stands where a person types next.
fn new_note_with_title(session: &mut Session, title: &str) {
    let plus = session.control(&session.doing("Notebook", "new_note"));
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
    let title = session.field("title");
    session.window.click_at(title.0, title.1);
    session.settle();
}

/// Twenty keystrokes into a note's body are twenty writes of the workspace — every letter is kept
/// as it is typed, so nothing typed is only in memory — and one step of the person's: one entry
/// in History, and one undo takes the whole burst back.
fn typing_a_body_is_one_step_of_however_many_letters(mut session: Session) {
    new_note_with_title(&mut session, "Groceries");
    let before = versions(&session).len();
    let acts = history_acts(&mut session);
    let steps_before = steps(&session).len();
    session.window.type_text("twenty characters in");
    session.settle();
    assert_eq!(
        versions(&session).len(),
        before + "twenty characters in".len(),
        "every letter is a write of the workspace as it is typed"
    );
    assert_eq!(
        body_text(&mut session).trim_end(),
        "twenty characters in",
        "the person sees every letter at once"
    );
    let steps_typed = steps(&session);
    assert_eq!(
        steps_typed.len(),
        steps_before + 1,
        "the burst is one step of the person's: {steps_typed:?}"
    );
    assert_eq!(
        steps_typed.last().copied(),
        Some("twenty characters in".len()),
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
    session
        .instance_host()
        .store()
        .set_edit_pause(std::time::Duration::from_millis(200));
    session.window.type_text("milk");
    session.settle();
    let steps_typed = steps(&session);
    assert_eq!(
        steps_typed.last().copied(),
        Some(4),
        "the burst is one step: {steps_typed:?}"
    );
    std::thread::sleep(std::time::Duration::from_millis(400));
    session.window.type_text(" and eggs");
    session.settle();
    let steps_typed = steps(&session);
    assert_eq!(
        steps_typed.last().copied(),
        Some(9),
        "what is typed after the pause is a step of its own: {steps_typed:?}"
    );
    assert_eq!(steps_typed[steps_typed.len() - 2], 4, "{steps_typed:?}");
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
    let plus = session.control(&session.doing("Notebook", "new_note"));
    session.window.click_at(plus.0, plus.1);
    session.settle();
    session.window.type_text("Groceries");
    session.settle();
    let body = session.body();
    session.window.click_at(body.0, body.1);
    session.settle();
    let steps_typed = steps(&session);
    assert_eq!(
        steps_typed.last().copied(),
        Some(9),
        "the title is one step: {steps_typed:?}"
    );
    session.window.type_text("milk");
    session.settle();
    assert_eq!(
        steps(&session).last().copied(),
        Some(4),
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
