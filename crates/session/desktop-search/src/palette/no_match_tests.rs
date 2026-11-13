//! A query that matches nothing says so: no unrelated row stands first for Enter
//! to open, not even the web search every query is offered. The row handing
//! what was typed to the person's agent is no unrelated row and stands: it
//! takes text nothing matched, as the canvas does.
use super::*;

use crate::model::{Hit, Source, Target};

/// The row handing what was typed to the person's agent, as the session's
/// source answers it: every query, last.
struct AgentRow;

impl Source for AgentRow {
    fn id(&self) -> &str {
        crate::providers::ask_agent::PROVIDER
    }

    fn name(&self) -> String {
        crate::providers::ask_agent::kind().display_label()
    }

    fn verbatim(&self) -> bool {
        true
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        let typed = needle.trim();
        if typed.is_empty() {
            return Vec::new();
        }
        vec![Hit {
            row: None,
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind: crate::providers::ask_agent::kind(),
            provider: self.id().into(),
            id: format!("send:{typed}"),
            title: format!("Send to Assistant: {typed}"),
            subtitle: None,
            facts: Vec::new(),
            score: 1,
            icon: None,
            target: Target::Copy {
                text: typed.to_owned(),
            },
        }]
    }
}

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

/// Text that named nothing still stands the row handing it to the person's agent: it takes a
/// request the search found nothing for, as the canvas does, and Enter sends it there rather than
/// opening nothing. A query that matched keeps the row, after what it found.
#[test]
fn a_query_that_matches_nothing_still_offers_it_to_the_persons_agent() {
    let mut search = DesktopSearch::new();
    let mut data = super::tests::data();
    data.sources.push(crate::Held::new(AgentRow));
    search.open(data);
    settle(&mut search);
    search.set_query("xqzvwk");
    settle(&mut search);
    assert!(
        search
            .results()
            .iter()
            .any(crate::providers::ask_agent::is_row),
        "the agent's row stands for what nothing matched: {:?}",
        search.results()
    );

    // A query that found something keeps the row, after what it found.
    search.set_query("sheets");
    settle(&mut search);
    let rows = search.results();
    let agent = rows
        .iter()
        .position(crate::providers::ask_agent::is_row)
        .unwrap_or_else(|| panic!("the agent's row stands under a match: {rows:?}"));
    let sheet = rows
        .iter()
        .position(|hit| hit.title.contains("Sheets"))
        .expect("the app the query names");
    assert!(agent > sheet, "the row crowds nothing out: {rows:?}");
}
