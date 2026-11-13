use super::tests::{char_event, ctrl_mods, feed, sample_app, seeded_app};
use super::*;
use ui::framework::testing::TestApp;
use ui::window_toolbar::SearchOpening;

fn raw_at(app: &TestApp<crate::resource::SheetsShell>, idx: usize, addr: Addr) -> String {
    app.state()
        .sheets
        .values()
        .nth(idx)
        .map(crate::workbook::Sheet::snapshot)
        .and_then(|snapshot| snapshot.raw.get(&addr).cloned())
        .unwrap_or_default()
}

#[test]
fn find_and_replace_open_the_window_search() {
    let (mut app, mut doc) = seeded_app();
    let mut chord = |key: &str| {
        assert!(feed(&mut app, &mut doc, &char_event(key), ctrl_mods()));
        ui::window_toolbar::take_search_request()
    };
    assert_eq!(chord("f"), Some(SearchOpening::Find));
    assert_eq!(chord("h"), Some(SearchOpening::Replace));
}

#[test]
fn the_window_search_counts_and_steps_through_the_workbook() {
    let mut app = sample_app();
    app.harness().present_in_window_bar();
    app.settle();
    let found = app.harness().search_in_window(SearchOpening::Find, "Total");
    assert_eq!(
        found,
        Some((1, 3)),
        "one budget cell, another, and one of the sales sheet"
    );
    assert!(
        app.harness().window_search_replaces(),
        "the bar offers the replacement this workbook can take"
    );
    let shown = |app: &mut TestApp<crate::resource::SheetsShell>| {
        app.with_surface::<SheetsApp, _>(|sheets| {
            (
                sheets.active_sheet,
                sheets.grid.active(),
                sheets.find_matches.len(),
                sheets.find_current,
            )
        })
        .expect("the grid is the surface")
    };
    assert_eq!(
        shown(&mut app),
        (0, (4, 0), 3, Some(0)),
        "the budget's first match is current and every match is kept to paint"
    );

    let stepped = app.harness().step_window_search(true);
    assert_eq!(stepped, Some((2, 3)), "the bar steps on");
    assert_eq!(
        shown(&mut app).1,
        (0, 6),
        "the budget's Total row is the match now current"
    );

    let stepped = app.harness().step_window_search(true);
    assert_eq!(stepped, Some((3, 3)));
    let (sheet, cell, _, _) = shown(&mut app);
    assert_eq!((sheet, cell), (1, (0, 5)), "the match switched sheets");

    let wrapped = app.harness().step_window_search(true);
    assert_eq!(wrapped, Some((1, 3)), "past the last match is the first");
}

#[test]
fn the_window_search_replaces_the_workbook_as_one_undo_step() {
    let mut app = sample_app();
    app.harness().present_in_window_bar();
    app.settle();
    assert_eq!(
        app.harness()
            .search_in_window(SearchOpening::Replace, "Total"),
        Some((1, 3))
    );
    assert!(app.harness().replace_in_window("Sum", true), "Replace all");
    app.settle();
    assert_eq!(raw_at(&app, 0, (4, 0)), "Q1 Sum", "the budget's header");
    assert_eq!(raw_at(&app, 0, (0, 6)), "Sum", "and its total row");
    assert_eq!(raw_at(&app, 1, (0, 5)), "Sum", "and the sales sheet's");

    assert!(app.undo(), "replacing every match is one step");
    assert_eq!(raw_at(&app, 0, (4, 0)), "Q1 Total", "the budget is back");
    assert_eq!(raw_at(&app, 0, (0, 6)), "Total");
    assert_eq!(raw_at(&app, 1, (0, 5)), "Total", "and the sales sheet");
}

#[test]
fn closing_the_window_search_clears_the_matches() {
    let mut app = sample_app();
    app.harness().present_in_window_bar();
    app.settle();
    assert!(
        app.harness()
            .search_in_window(SearchOpening::Find, "Total")
            .is_some()
    );
    let painted = |app: &mut TestApp<crate::resource::SheetsShell>| {
        app.with_surface::<SheetsApp, _>(|sheets| sheets.find_matches.len())
            .expect("the grid is the surface")
    };
    assert_eq!(painted(&mut app), 3);
    app.harness()
        .perform_named("window-search-close", &ui::a11y::Action::Activate)
        .expect("the search closes");
    app.settle();
    assert_eq!(painted(&mut app), 0, "no match is painted any more");
}
