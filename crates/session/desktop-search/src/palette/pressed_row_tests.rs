//! Enter on a row the person saw opens that row, though a source's answer
//! rebuilt and renumbered the list in the frame the press was taken.
use super::tests::{arrow_down, data, selected, settled, with_a_late_app};

/// Enter opens the row the person saw under the highlight, not the row that took its place
/// when a source answered in the same frame: the press names the row by the id the frame
/// handed out, and that id is read as the thing it stood for, never as a place in the list.
#[test]
fn enter_opens_the_row_the_person_saw_though_an_answer_renumbered_the_list() {
    let (mut search, arrived) = with_a_late_app(data());
    search.set_query("notes");
    settled(&mut search);
    assert!(search.waiting(), "the app's answer is still coming");
    // The person arrows down to the second row and presses Enter there.
    search.palette_mut().set_cursor(1);
    let seen: Vec<String> = search
        .results()
        .iter()
        .map(|hit| hit.title.clone())
        .collect();
    let under = search
        .palette()
        .selected_id()
        .map(str::to_owned)
        .expect("a row is selected");
    let chosen = search
        .rows
        .get(search.palette().cursor())
        .map(|hit| (hit.title.clone(), hit.target.clone()))
        .expect("the row under the highlight");

    // The answer lands in the frame the press is taken: the list is rebuilt and renumbered
    // under it before the press reaches the desk.
    arrived.set(true);
    assert!(search.refresh(), "the answer re-ranks the rows");
    settled(&mut search);
    assert!(
        search.results().len() > seen.len(),
        "the answer listed its row: {seen:?}"
    );
    // Enter, as the page routes it: the id the person saw, taken against the rows now.
    search.take_the_press(&under);
    search.observe();
    assert_eq!(
        search.take_choice(),
        Some(chosen.1),
        "the press opened another row than {under:?}, the one the person saw ({seen:?})"
    );
}

/// The person arrowed to a row; an app's answer arriving a frame later
/// re-ranks the list. Enter must still open the row they moved to, not
/// whatever slid under the cursor.
#[test]
fn an_answer_that_arrives_late_keeps_the_row_the_person_moved_to() {
    let (mut search, arrived) = with_a_late_app(data());
    search.set_query("notes");
    settled(&mut search);
    assert!(search.waiting(), "the app's answer is still coming");
    assert!(search.results().len() >= 2, "{:?}", search.results());
    assert!(arrow_down(&mut search));
    let chosen = selected(&search).expect("a row is selected");

    arrived.set(true);
    assert!(search.refresh(), "the answer re-ranks the rows");
    settled(&mut search);
    assert!(
        search
            .results()
            .iter()
            .any(|hit| hit.title == "notes from the meeting"),
        "the late answer is listed: {:?}",
        search.results()
    );
    assert_eq!(selected(&search), Some(chosen));
}
