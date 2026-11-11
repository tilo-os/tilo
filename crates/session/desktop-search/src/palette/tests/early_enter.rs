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
    assert!(
        !arrived.get() && search.expecting(),
        "the app has not answered"
    );
    assert!(!search.results().is_empty(), "the typed query's rows stand");
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
