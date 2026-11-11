//! A query that matches nothing says so: no unrelated row stands first for
//! Enter to open, not even the web search every query is offered.
use super::*;

/// Refresh until no source owes an answer and a frame changes nothing.
fn settle(search: &mut DesktopSearch) {
    let now = ui::time::now();
    let deadline = now.checked_add(crate::tests::SETTLE).unwrap_or(now);
    while search.refresh() || search.answering() {
        assert!(ui::time::now() < deadline, "the search never settled");
    }
}

#[test]
fn a_query_that_matches_nothing_says_so_and_enter_opens_nothing() {
    let mut search = DesktopSearch::new();
    search.open(super::tests::data());
    settle(&mut search);
    search.set_query("xqzvwk");
    settle(&mut search);
    assert!(
        search.palette().visible_entries().is_empty(),
        "nothing matched, so no row stands: {:?}",
        search.palette().visible_entries()
    );
    assert!(
        search.palette().empty_text().contains("xqzvwk"),
        "the card says nothing matched what was typed: {:?}",
        search.palette().empty_text()
    );
    search.palette_mut().activate_selected();
    search.observe();
    assert_eq!(
        search.take_choice(),
        None,
        "Enter on no match opens nothing"
    );

    // A query that matches something still ends in the web search.
    search.set_query("sheets");
    settle(&mut search);
    assert!(
        search
            .results()
            .iter()
            .any(crate::providers::WebSearch::is_every_query_row),
        "the web search stays offered under a match"
    );
}
