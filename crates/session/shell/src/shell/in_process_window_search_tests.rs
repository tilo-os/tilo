// An app's search, answered by the window it is scoped to.

/// Rows of the focused window's search for `needle`, read from what the
/// mounted window publishes, as the overlay host lends it.
fn window_rows(
    harness: &ui::headless::FrameHarness,
    tab: u64,
    app: &str,
    needle: &str,
) -> (
    Vec<crate::shell::search::SearchResult>,
    crate::shell::chrome_state::Chrome,
) {
    use crate::shell::search::SearchProvider as _;
    use crate::shell::search::providers::focused_window::{FocusedWindowProvider, WindowReading};
    let provider = Rc::new(FocusedWindowProvider::default());
    let doc = automerge::AutoCommit::new();
    let _lent = provider.lend(Some(WindowReading::of(
        harness.root_widget(),
        &doc,
        tab,
        app.into(),
    )));
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_default(),
    );
    chrome.borrow_mut().search_scope = crate::shell::chrome_state::SearchScope::ActiveApp;
    let rows = {
        let state = chrome.borrow();
        provider.query(crate::shell::search::SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle,
            wanted: provider.resource_kinds(),
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        })
    };
    (rows, chrome)
}

/// A row reads as a person reads it: never an id, a stored place or a
/// URI.
fn reads_well(row: &crate::shell::search::SearchResult) -> bool {
    let listed = tilo_actions::ListedRow {
        title: row.title.clone(),
        subtitle: row.subtitle.clone(),
    };
    tilo_actions::listed_row_problems(&listed).is_empty()
        && !row.title.contains("xdg:")
        && !row.title.contains("://")
        && row
            .subtitle
            .as_deref()
            .is_none_or(|line| !line.contains("xdg:"))
}

/// An app's search, scoped to its window, lists what the window holds
/// and what can be done there, read from the window itself: Files'
/// places by their names, then its actions by their short titles. A row
/// chosen goes to that window, to run as its own control would.
#[test]
fn an_apps_search_reads_its_window_and_runs_rows_there() {
    let profile = tempfile::tempdir().unwrap();
    let mut inputs = crate::shell::hosted_inputs::HostedTileInputs::default();
    inputs.mount_apps_in_process(
        InProcessApps::at(
            profile.path(),
            &profile.path().join("person"),
            "test-person",
        )
        .unwrap(),
    );
    let files = mount_tab(&inputs, 91, crate::shell::state::AppKind::FileExplorer);
    let (rows, chrome) = window_rows(&files, 91, "so.tilo.files", "");
    let titles: Vec<(&str, bool)> = rows
        .iter()
        .map(|row| {
            (
                row.title.as_str(),
                row.kind == crate::shell::search::ResourceKind::Action,
            )
        })
        .collect();
    let first_action = titles.iter().position(|(_, action)| *action);
    let home = titles
        .iter()
        .position(|row| *row == ("Home", false))
        .unwrap_or_else(|| panic!("Files' Home place is listed by its name: {titles:?}"));
    assert!(
        first_action.is_some_and(|action| home < action),
        "what the window holds comes before what it can do: {titles:?}"
    );
    assert!(
        titles.contains(&("New folder", true)),
        "Files' actions are listed by their short titles: {titles:?}"
    );
    for row in &rows {
        assert!(reads_well(row), "{:?} / {:?}", row.title, row.subtitle);
    }

    let row = &rows[home];
    let mut doc = automerge::AutoCommit::new();
    (row.on_open)(
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
        &chrome,
        &crate::shell::search::action_api::ActionArgs::default(),
    )
    .expect("choosing a place queues it for its window");
    let queued = std::mem::take(&mut chrome.borrow_mut().window_controls);
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued[0].tab, 91,
        "the row runs in the window it was found in"
    );

    for (tab, kind, action) in [
        (92, notes_kind(), "New note"),
        (
            93,
            crate::shell::state::AppKind::Other("Editor".into()),
            "New file",
        ),
    ] {
        let window = mount_tab(&inputs, tab, kind);
        let (rows, _) = window_rows(&window, tab, "app", "");
        assert!(
            rows.iter()
                .any(|row| row.title == action
                    && row.kind == crate::shell::search::ResourceKind::Action),
            "{action} is offered: {:?}",
            rows.iter().map(|row| &row.title).collect::<Vec<_>>()
        );
        for row in &rows {
            assert!(reads_well(row), "{:?} / {:?}", row.title, row.subtitle);
        }
    }
}
