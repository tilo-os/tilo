//! Books' controls live in the window's bar: the shelf, the reader, the
//! settings page and the search page draw no header row of their own.

use automerge::AutoCommit;
use ui::framework::testing::TestApp;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Edits, Queryable as _, Widget as _};
use ui::window_toolbar::{ToolbarEvent, WindowToolbar};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::{
    ANNOTATIONS, APPEARANCE, BOOKMARK, HIDE_FINISHED, OPEN_BOOK, SETTINGS, sort_id, tab_id,
};
use crate::app::{Action, BooksApp, SettingsTab, View};
use crate::content::load_sample_library;
use crate::model::LibrarySort;

fn sample() -> BooksApp {
    BooksApp::with_library(
        crate::resource::library_dispatcher_for_test(),
        load_sample_library(),
    )
}

fn library_doc() -> AutoCommit {
    crate::resource::doc_with(&[], &[])
}

/// One frame of `app` at 1280 by 800, as the window runs it.
fn frame(app: &mut BooksApp, doc: &mut AutoCommit) {
    app.read_record(doc);
    app.settle(&mut Edits::lend(doc), &automerge::ROOT);
    app.sync(doc, &automerge::ROOT);
    app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    app.resolve_drawers();
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    app.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &Theme::dark(),
        &PresenceCtx::empty(),
    );
}

fn press(app: &mut BooksApp, doc: &mut AutoCommit, event: &ToolbarEvent) {
    app.run_window_bar_event(event, &mut Edits::lend(doc));
}

fn action(id: &str) -> ToolbarEvent {
    ToolbarEvent::Action { id: id.into() }
}

fn action_ids(toolbar: &WindowToolbar) -> Vec<&str> {
    toolbar
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect()
}

fn location(toolbar: &WindowToolbar) -> Option<&str> {
    toolbar
        .location
        .as_ref()
        .map(|location| location.label.as_str())
}

/// Every name and action target in `node`'s subtree.
fn labels(node: &ui::a11y::AccessNode, out: &mut Vec<String>) {
    out.extend(node.name.clone());
    out.extend(node.action_target.clone());
    for child in &node.children {
        labels(child, out);
    }
}

/// What the view draws: every name and action target it exposes.
fn drawn(app: &BooksApp) -> Vec<String> {
    let mut out = Vec::new();
    labels(
        &app.accessibility().expect("the view's semantics"),
        &mut out,
    );
    out
}

/// Whether the view drew a control for an action the bar holds.
fn draws_bar_control(app: &BooksApp) -> bool {
    app.hits.iter().any(|hit| {
        matches!(
            hit.action,
            Action::Back
                | Action::ToggleToc
                | Action::ToggleNotes
                | Action::ToggleTypePopover
                | Action::ToggleBookmark
                | Action::OpenSettings
                | Action::CloseSettings
                | Action::SetSettingsTab(_)
                | Action::CloseSearch
                | Action::SetSort(_)
                | Action::ToggleHideFinished
        )
    })
}

#[test]
fn the_shelf_names_the_library_in_the_bar_and_draws_no_header_row() {
    let mut app = sample();
    let mut doc = library_doc();
    app.view = View::Library;
    frame(&mut app, &mut doc);

    let toolbar = app.window_toolbar();
    assert_eq!(location(&toolbar), Some("Library"));
    let views = toolbar
        .views
        .as_ref()
        .expect("the sort order is the bar's views");
    assert_eq!(
        views
            .choices
            .iter()
            .map(|view| view.id.clone())
            .collect::<Vec<_>>(),
        LibrarySort::ALL.map(sort_id)
    );
    assert_eq!(views.chosen, sort_id(app.library_prefs.sort));
    assert_eq!(action_ids(&toolbar), [OPEN_BOOK, HIDE_FINISHED, SETTINGS]);
    let menu: Vec<&str> = toolbar
        .actions
        .iter()
        .filter(|action| action.menu)
        .map(|action| action.id.as_str())
        .collect();
    assert_eq!(
        menu,
        [HIDE_FINISHED, SETTINGS],
        "Open book stays in the row"
    );

    assert!(!draws_bar_control(&app), "the shelf draws no bar control");
    let drawn = drawn(&app);
    for own in [
        "books.library.sort",
        "books.library.hide-read",
        "books.settings.open",
        "Sort books",
        "Hide finished books",
        "Book settings",
    ] {
        assert!(
            !drawn.iter().any(|label| label == own),
            "the shelf draws no {own} of its own: {drawn:?}"
        );
    }
}

#[test]
fn the_bar_sorts_the_shelf_hides_finished_books_and_opens_settings() {
    let mut app = sample();
    let mut doc = library_doc();
    app.view = View::Library;
    frame(&mut app, &mut doc);

    press(&mut app, &mut doc, &action(&sort_id(LibrarySort::Author)));
    assert_eq!(app.library_prefs.sort, LibrarySort::Author);
    assert_eq!(
        crate::resource::read_doc(&doc).prefs.sort,
        LibrarySort::Author,
        "the order is kept in the library"
    );
    assert_eq!(
        app.window_toolbar().views.map(|views| views.chosen),
        Some(sort_id(LibrarySort::Author))
    );

    press(&mut app, &mut doc, &action(HIDE_FINISHED));
    assert!(crate::resource::read_doc(&doc).prefs.hide_read);
    let hide = app
        .window_toolbar()
        .actions
        .into_iter()
        .find(|action| action.id == HIDE_FINISHED)
        .expect("Hide finished");
    assert!(hide.on, "the item shows that finished books are hidden");

    press(&mut app, &mut doc, &action(SETTINGS));
    assert_eq!(app.view, View::Settings);
}

#[test]
fn an_empty_shelf_offers_only_settings_in_the_bar() {
    let mut app =
        BooksApp::with_library(crate::resource::library_dispatcher_for_test(), Vec::new());
    let mut doc = library_doc();
    frame(&mut app, &mut doc);
    let toolbar = app.window_toolbar();
    assert_eq!(location(&toolbar), Some("Library"));
    assert_eq!(toolbar.views, None, "nothing to sort");
    assert_eq!(
        action_ids(&toolbar),
        [SETTINGS],
        "the empty state offers Open a book itself"
    );
}

#[test]
fn the_reader_names_the_book_in_the_bar_and_draws_no_header_row() {
    let mut app = sample();
    let mut doc = library_doc();
    frame(&mut app, &mut doc);
    assert_eq!(app.view, View::Reader);

    let toolbar = app.window_toolbar();
    let title = app.open_book().expect("an open book").title.clone();
    assert_eq!(location(&toolbar), Some(title.as_str()));
    assert_eq!(
        toolbar.sidebar,
        Some(app.toc_open()),
        "the contents are the sidebar"
    );
    assert!(toolbar.navigation.is_some_and(|navigation| navigation.back));
    assert_eq!(action_ids(&toolbar), [APPEARANCE, BOOKMARK, ANNOTATIONS]);

    assert!(!draws_bar_control(&app), "the reader draws no bar control");
    assert_eq!(
        app.reader_body.0, app.rect.y0,
        "the page starts at the top of the window, under the bar"
    );
    let drawn = drawn(&app);
    for own in [
        "books.reader.back",
        "books.reader.contents",
        "books.reader.annotations",
        "books.reader.typography",
        "books.reader.bookmark",
        "Back to library",
        "Reading appearance",
        "Bookmark this page",
    ] {
        assert!(
            !drawn.iter().any(|label| label == own),
            "the reader draws no {own} of its own: {drawn:?}"
        );
    }
}

#[test]
fn the_bar_drives_the_reader() {
    let mut app = sample();
    let mut doc = library_doc();
    frame(&mut app, &mut doc);

    let contents = app.toc_open();
    press(&mut app, &mut doc, &ToolbarEvent::ToggleSidebar);
    assert_eq!(app.toc_open(), !contents);
    assert_eq!(app.window_toolbar().sidebar, Some(!contents));

    let notes = app.notes_open();
    press(&mut app, &mut doc, &action(ANNOTATIONS));
    assert_eq!(app.notes_open(), !notes);

    press(&mut app, &mut doc, &action(APPEARANCE));
    assert!(app.reading_settings.is_open());
    let appearance = app
        .window_toolbar()
        .actions
        .into_iter()
        .find(|action| action.id == APPEARANCE)
        .expect("Reading appearance");
    assert!(appearance.on, "the button shows its popover is open");
    press(&mut app, &mut doc, &action(APPEARANCE));
    assert!(!app.reading_settings.is_open());

    press(&mut app, &mut doc, &ToolbarEvent::Back);
    assert_eq!(app.view, View::Library);
}

#[test]
fn the_settings_page_offers_its_tabs_and_back_in_the_bar() {
    let mut app = sample();
    let mut doc = library_doc();
    app.view = View::Settings;
    app.settings_tab = SettingsTab::Library;
    frame(&mut app, &mut doc);

    let toolbar = app.window_toolbar();
    assert_eq!(location(&toolbar), Some("Settings"));
    assert!(toolbar.navigation.is_some_and(|navigation| navigation.back));
    let views = toolbar.views.expect("the tabs are the bar's views");
    assert_eq!(
        views
            .choices
            .iter()
            .map(|view| view.label.as_str())
            .collect::<Vec<_>>(),
        ["Library", "Fonts", "Colour schemes", "Shortcuts"]
    );
    assert_eq!(views.chosen, tab_id(SettingsTab::Library));
    assert!(!draws_bar_control(&app), "the page draws no tabs or Back");

    press(&mut app, &mut doc, &action(&tab_id(SettingsTab::Colours)));
    assert_eq!(app.settings_tab, SettingsTab::Colours);
    press(&mut app, &mut doc, &ToolbarEvent::Back);
    assert_ne!(app.view, View::Settings);
}

#[test]
fn the_search_page_names_the_search_in_the_bar_and_goes_back_from_it() {
    let mut app = sample();
    let mut doc = library_doc();
    app.view = View::Library;
    app.query = super::super::query_view::BookQuery::hermetic();
    frame(&mut app, &mut doc);
    app.query_opened();
    app.set_query_text("glass");
    app.settle_query();
    frame(&mut app, &mut doc);
    assert_eq!(app.view, View::Search);

    let toolbar = app.window_toolbar();
    assert_eq!(location(&toolbar), Some("glass"));
    assert!(toolbar.navigation.is_some_and(|navigation| navigation.back));
    assert!(!draws_bar_control(&app), "the page draws no Back");

    press(&mut app, &mut doc, &ToolbarEvent::Back);
    assert_eq!(app.view, View::Library);
}

#[test]
fn a_hosted_window_declares_its_slots_and_hears_their_presses() {
    let mut app = TestApp::<crate::EbookShell>::new().expect("Books opens");
    app.harness().present_in_window_bar();
    app.settle();
    let declared = ui::window_toolbar::declared().expect("the window declares its slots");
    assert_eq!(location(&declared), Some("Library"));

    app.harness().press_window_slot(action(SETTINGS));
    app.settle();
    assert_eq!(app.surface_mut::<BooksApp>().view, View::Settings);
    let declared = ui::window_toolbar::declared().expect("the window declares its slots");
    assert_eq!(location(&declared), Some("Settings"));
}

#[test]
fn opening_settings_and_showing_the_library_are_published_actions() {
    let mut app = TestApp::<crate::EbookShell>::new().expect("Books opens");
    app.settle();
    app.call_on(
        "ebook.library/derived/settings",
        "settings",
        serde_json::json!({}),
    )
    .expect("the settings open");
    app.settle();
    assert_eq!(app.surface_mut::<BooksApp>().view, View::Settings);

    app.call("show-library", serde_json::json!({}))
        .expect("the library shows");
    app.settle();
    assert_eq!(app.surface_mut::<BooksApp>().view, View::Library);
}

#[test]
fn the_bars_back_and_settings_run_the_published_actions() {
    let mut app = sample();
    let mut doc = library_doc();
    frame(&mut app, &mut doc);
    app.reading_settings.open();
    press(&mut app, &mut doc, &ToolbarEvent::Back);
    assert_eq!(app.view, View::Library);
    assert!(
        !app.reading_settings.is_open(),
        "showing the library closes what the reader had open"
    );

    press(&mut app, &mut doc, &action(SETTINGS));
    assert_eq!(app.view, View::Settings);
    assert_eq!(app.settings_scroll, 0.0, "the settings open at their top");
}
