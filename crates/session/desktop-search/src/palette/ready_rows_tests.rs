//! A content source that takes seconds to answer must not hold the search
//! "searching" for the rows that are ready.
//!
//! The palette publishes `searching` until every source has answered
//! ([`DesktopSearch::settled`]). A window opened a moment ago has its
//! content index built as its provider is asked, and on a page that build
//! runs on the one thread there is: the apps, actions and places that were
//! ready in the first frame stayed under "Searching…" for as long as the
//! last source took — 14 s for api-tester, 57 s for calendar, measured on
//! the page desk. What is ready must show at once, and the slow source's
//! rows join when they come.
use super::*;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use ui::search::{
    PagePoll, PendingToken, SearchCursor, SearchPage, SearchProvider, SearchQuery, SearchResult,
};
use ui::time::Duration;

/// An app whose content takes until `release` says so, as a window opened
/// a moment ago does while its index is built.
struct SlowContent {
    released: Rc<AtomicBool>,
    asked: Rc<std::cell::Cell<usize>>,
}

impl SearchProvider for SlowContent {
    fn name(&self) -> &str {
        "Calendar"
    }
    fn search(&self, _: &str) -> Vec<SearchResult> {
        Vec::new()
    }
    fn search_page(
        &self,
        query: &SearchQuery,
        _: Option<&SearchCursor>,
        _: usize,
        _: usize,
    ) -> SearchPage {
        self.asked.set(self.asked.get() + 1);
        if !self.released.load(Ordering::SeqCst) {
            let mut page = SearchPage::empty();
            page.pending = Some(PendingToken::new(1));
            return page;
        }
        let row = SearchResult::for_action(
            "Calendar",
            format!("{} this week", query.text()),
            tilo_actions::ActionAddress::new("calendar", "open"),
        );
        SearchPage::of(vec![row], None)
    }
    fn poll_page(&self, _: &PendingToken) -> PagePoll {
        if self.released.load(Ordering::SeqCst) {
            PagePoll::Answered(SearchPage::empty())
        } else {
            PagePoll::Pending
        }
    }
}

fn window_of(app: &str, title: &str, tab: u64) -> WindowEntry {
    WindowEntry {
        tab,
        title: title.into(),
        app_title: app.into(),
        app: Some(format!("so.tilo.{app}")),
        place: vec!["Workspace".into(), "Main".into()],
    }
}

/// The search over a desk where one app's content is still coming: what
/// the apps, windows and places ready in the first frame look like to
/// the person while it is out.
fn with_slow_content() -> (DesktopSearch, Rc<AtomicBool>, Rc<std::cell::Cell<usize>>) {
    let released = Rc::new(AtomicBool::new(false));
    let asked = Rc::new(std::cell::Cell::new(0));
    let mut meeting = ResourceSpec::new("calendar", "calendar.event", "Calendar");
    meeting
        .actions
        .push(tilo_actions::ActionSpec::immediate_encoded(
            "open",
            "Open",
            tilo_actions::ExecutionLocus::ClientLocal,
            tilo_actions::EncodedAction {
                type_id: "test.open".into(),
                payload: Vec::new(),
            },
        ));
    let mut search = DesktopSearch::new();
    search.open(SearchData {
        applications: vec![
            super::tests::data().applications[0].clone(),
            AppEntry {
                id: "so.tilo.calendar".into(),
                title: "Calendar".into(),
                category: None,
                description: None,
            },
        ],
        windows: vec![window_of("notes", "Notes", 7)],
        resources: vec![meeting],
        providers: vec![Box::new(SlowContent {
            released: Rc::clone(&released),
            asked: Rc::clone(&asked),
        })],
        scope: Scope::Workspace,
        ..SearchData::default()
    });
    // The card's room, as a host gives it: the rows are cut to it.
    search.set_room(600.0, 400.0);
    (search, released, asked)
}

/// Draw a frame; the source that answers on a frame's own share is the
/// content one, so a few frames let it be asked.
fn frame(search: &mut DesktopSearch) {
    search.refresh();
}

/// The row titles the palette shows now.
fn titles(search: &DesktopSearch) -> Vec<String> {
    search
        .results()
        .iter()
        .map(|hit| hit.title.clone())
        .collect()
}

/// Frames until the desk shows `title`, or the deadline passes.
fn frames_until(search: &mut DesktopSearch, title: &str) {
    let deadline = ui::time::now() + Duration::from_secs(10);
    while !titles(search).iter().any(|shown| shown == title) && ui::time::now() < deadline {
        frame(search);
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_content_source_that_takes_seconds_does_not_keep_the_ready_rows_searching() {
    let (mut search, released, _asked) = with_slow_content();
    search.set_query("cal");
    // The rows the desktop's own sources bring (the app, the window):
    // they answer on workers, so a few frames let them in. The app's
    // content does not answer at all until it is released.
    frames_until(&mut search, "Calendar");
    let ready = titles(&search);
    assert!(
        ready.iter().any(|title| title == "Calendar"),
        "the app's row is ready: {ready:?}"
    );
    assert!(
        !ready.iter().any(|title| title.contains("this week")),
        "the content has not come yet: {ready:?}"
    );
    // What the person sees is not drawn as still searching: the rows
    // that are ready are shown, and the one that is not yet does not
    // hold them behind "Searching…".
    // THE BUG: the desk publishes `searching` until every source has
    // answered, so a window whose content is still coming kept the
    // apps, actions and places that were ready marked "still
    // searching" — for 14 s on api-tester, 57 s on calendar, measured on
    // the page desk. The rows that are ready are the search's answer.
    assert!(
        search.settled(),
        "the rows that are ready are not held behind a source still out"
    );
    assert!(
        !search.palette().is_searching(),
        "the rows that are ready are not drawn as 'Searching…': {ready:?}"
    );
    // Frames while it is still out change none of that.
    for _ in 0..5 {
        frame(&mut search);
        assert!(
            !search.palette().is_searching(),
            "still not searching while the content is out"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    // When it answers, its row joins.
    released.store(true, Ordering::SeqCst);
    let deadline = ui::time::now() + Duration::from_secs(10);
    while !titles(&search)
        .iter()
        .any(|title| title.contains("this week"))
        && ui::time::now() < deadline
    {
        frame(&mut search);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        titles(&search)
            .iter()
            .any(|title| title.contains("this week")),
        "the content's row joined when it answered: {:?}",
        titles(&search)
    );
    // Every source that owed a row has answered: the search is over.
    let deadline = ui::time::now() + Duration::from_secs(10);
    while search.answering() && ui::time::now() < deadline {
        frame(&mut search);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!search.answering(), "every source answered");
    assert!(search.settled(), "the search is over");
    assert!(
        !search.palette().is_searching(),
        "the card is not searching"
    );
}
