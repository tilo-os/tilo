//! An Enter pressed before the search answered what was typed: it waits for the typed query's rows
//! and takes their top row, never a row of the query before, and nothing that has not answered
//! holds it.
use super::*;

/// Enter pressed in the same batch of input as the keys before it, before a frame answered
/// them, opens the top result of what was typed, never a row of the query before.
#[test]
fn enter_right_after_typing_opens_what_was_typed() {
    let mut search = DesktopSearch::new();
    search.open(data());
    search.set_query("sheets");
    settled(&mut search);
    // "notes", then Enter, with no frame between them.
    search.palette_mut().set_query("notes");
    search.palette_mut().activate_selected();
    search.observe();
    assert!(
        search.take_choice().is_none(),
        "nothing is opened from the earlier rows"
    );
    settled(&mut search);
    let chosen = format!("{:?}", search.take_choice().expect("Enter chose a row"));
    assert!(
        chosen.contains("notes") && !chosen.contains("sheets"),
        "Enter after typing \"notes\" chose {chosen}"
    );
}

/// An Enter that waited for the typed query's rows opens their top row once they stand, though
/// an app's answer to the query is still coming: the rows shown are the search's answer, as
/// the desk publishes it, and an app that never answers must not hold the key for ever.
#[test]
fn an_early_enter_opens_the_typed_rows_while_an_app_still_answers() {
    let (mut search, arrived) = with_a_late_app(data());
    search.palette_mut().set_query("notes");
    search.palette_mut().activate_selected();
    search.observe();
    assert!(
        search.take_choice().is_none(),
        "nothing from the earlier rows"
    );
    settled(&mut search);
    assert!(!arrived.get(), "the app has not answered");
    assert!(
        !search.is_open(),
        "the typed query's rows let Enter choose, and search ended"
    );
    let chosen = format!(
        "{:?}",
        search
            .take_choice()
            .expect("Enter chose the top row without the app's answer")
    );
    assert!(
        chosen.contains("notes"),
        "Enter after \"notes\" chose {chosen}"
    );
}

/// An Enter pressed in the same batch as the keys before it opens in the first pass whose rows
/// answer what was typed, as an Enter on rows already shown does: the search's slower sources
/// still answering must not cost the open the frames they take.
#[test]
fn an_early_enter_opens_once_the_typed_rows_stand() {
    let mut search = DesktopSearch::new();
    search.open(data());
    search.set_query("sheets");
    settled(&mut search);
    search.palette_mut().set_query("notes");
    search.palette_mut().activate_selected();
    search.observe();
    // The desk's passes after the batch, up to the first that shows rows for "notes".
    for _ in 0..50 {
        search.refresh();
        if !search.is_open()
            || (search.shown_query.as_deref() == Some("notes") && !search.rows.is_empty())
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    // The pass that chose also leaves search: the desk reads that in the same pass, and a desk with
    // nothing left to draw may take no other to notice it later.
    assert!(
        !search.is_open(),
        "the held Enter chose, and search is still open"
    );
    let chosen = format!(
        "{:?}",
        search
            .take_choice()
            .expect("Enter chose a row in the pass that showed the typed rows")
    );
    assert!(
        chosen.contains("notes"),
        "Enter after \"notes\" chose {chosen}"
    );
}
