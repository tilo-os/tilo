//! A row in before the desktop's own sources is not the search's answer.
//!
//! On a page the sources a search moves off the frame run from the page's one queue, between
//! frames, while an open window's own rows answer in the frame that typed. Under load a frame
//! took seconds, so "sheets" showed the open workbook ("Sheet1") a frame before the Sheets app
//! and the web search, and the shell said the search was over with the workbook alone: a page
//! that waited for the answer read one row, and an Enter there opened the workbook, not the app.
//! The desktop's own sources (the apps, the windows, the commands, the web answer) answer from
//! memory at once, so the search is over, and an Enter on its top row chooses, only once they
//! are in. Sources that may take long — the host's, an index still being built — still join
//! later ([`super::ready_rows_tests`]).
use super::*;
use ui::rt::background::Pumped;

/// Sheets' window, whose provider answers with the open workbook in the
/// frame that asks it, as an app's own providers do.
struct OpenWorkbook;

impl ui::search::SearchProvider for OpenWorkbook {
    fn name(&self) -> &'static str {
        "Sheets"
    }
    fn search(&self, query: &str) -> Vec<ui::search::SearchResult> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        vec![ui::search::SearchResult::for_action(
            "Workbook",
            "Sheets budget",
            tilo_actions::ActionAddress::new("budget", "open"),
        )]
    }
}

/// The open workbook, as Sheets' window publishes it, with the action its
/// row runs.
fn workbook() -> ResourceSpec {
    let mut sheet = ResourceSpec::new("budget", "sheets.workbook", "Sheets budget");
    sheet
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
    sheet
}

/// A search over the apps and the open workbook, whose off-frame sources answer only when
/// `pumped` runs them, as a page's queue does after the frame.
fn search_with_held_workers(pumped: &Pumped) -> DesktopSearch {
    let _background = pumped.background().enter();
    let mut search = DesktopSearch::new();
    search.open(SearchData {
        applications: super::tests::data().applications,
        resources: vec![workbook()],
        providers: vec![Box::new(OpenWorkbook)],
        scope: Scope::Workspace,
        ..SearchData::default()
    });
    search.set_room(600.0, 400.0);
    search
}

fn titles(search: &DesktopSearch) -> Vec<String> {
    search
        .results()
        .iter()
        .map(|hit| hit.title.clone())
        .collect()
}

/// The frames a page draws before its queue runs: the frame's own sources answer.
fn frames(search: &mut DesktopSearch) {
    for _ in 0..4 {
        search.refresh();
    }
}

tilo_test_env::scenario! {
    fn a_row_in_before_the_desktops_own_sources_is_not_the_answer;

    phase the_search_is_not_over_with_the_workbook_alone;
    {
        let pumped = Pumped::new();
        let mut search = search_with_held_workers(&pumped);
        search.set_query("sheets");
        frames(&mut search);
        let shown = titles(&search);
        assert!(
            shown.iter().any(|title| title == "Sheets budget"),
            "the window's own row answers in the frame: {shown:?}"
        );
        assert!(
            !shown.iter().any(|title| title == "Sheets"),
            "the apps answer off the frame, not yet: {shown:?}"
        );
        // THE BUG: the shell published the search as over here, with the
        // workbook alone, and the Sheets app and the web search never in it.
        assert!(
            !search.settled(),
            "the apps are still to answer: {shown:?}"
        );
        pumped.run_pending();
        frames(&mut search);
        let shown = titles(&search);
        assert!(search.settled(), "every own source answered: {shown:?}");
        assert_eq!(
            shown.first().map(String::as_str),
            Some("Sheets"),
            "the app a name is for leads: {shown:?}"
        );
    }

    phase an_enter_on_the_workbook_alone_waits_for_the_app;
    {
        let pumped = Pumped::new();
        let mut search = search_with_held_workers(&pumped);
        search.set_query("sheets");
        frames(&mut search);
        search.palette_mut().activate_selected();
        search.observe();
        assert!(
            search.take_choice().is_none(),
            "nothing is opened while the apps are still to answer"
        );
        assert!(search.is_open(), "the search waits for them");
        pumped.run_pending();
        frames(&mut search);
        let chosen = format!("{:?}", search.take_choice().expect("Enter chose the top row"));
        assert!(
            chosen.contains("so.tilo.sheets") && !chosen.contains("budget"),
            "Enter on \"sheets\" chose {chosen}"
        );
    }
}
