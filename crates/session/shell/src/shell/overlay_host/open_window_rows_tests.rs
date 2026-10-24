//! Searching for an app that has a window open: the window and the app are
//! separate rows, and Shift on the window opens a new window of its app.

use std::cell::RefCell;
use std::rc::Rc;

use automerge::AutoCommit;
use ui::widget::Container;
use ui::widget::command_palette::ActivationModifier;
use vello::kurbo::Rect;

use super::*;
use crate::shell::chrome_state::{ChromeState, OpenTarget};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};
use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth, WorkspaceTree};

thread_local! {
    /// Where each start of the app was asked to open.
    static STARTS: RefCell<Vec<OpenTarget>> = const { RefCell::new(Vec::new()) };
}

fn alacritty_row() -> SearchResult {
    SearchResult::nullary(
        ResourceKind::Application,
        "test-host-apps",
        "Alacritty",
        "Alacritty",
        None,
        40,
        |_, _, chrome: &crate::shell::chrome_state::Chrome| {
            let target = chrome.borrow().open_target;
            STARTS.with(|starts| starts.borrow_mut().push(target));
        },
    )
}

struct HostApps;

impl SearchProvider for HostApps {
    fn id(&self) -> &'static str {
        "test-host-apps"
    }
    fn name(&self) -> &'static str {
        "Linux apps"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Application]
    }
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if "alacritty".contains(&ctx.needle.to_lowercase()) {
            vec![alacritty_row()]
        } else {
            Vec::new()
        }
    }
    fn opens_app(&self, result: &SearchResult, app_id: &str) -> bool {
        result.id == app_id
    }
    fn app_row(&self, app_id: &str) -> Option<SearchResult> {
        (app_id == "Alacritty").then(alacritty_row)
    }
    fn row_icon(&self, _: &SearchResult) -> Option<ui::icons::Icon> {
        Some(ui::icons::Icon::SquareTerminal)
    }
}

/// One Alacritty window, showing `~/src`, in the workspace in front.
fn host() -> ShellOverlayHost {
    STARTS.with(|starts| starts.borrow_mut().clear());
    let mut tree = WorkspaceTree::bootstrap_default();
    tree.desktop.workspaces[0].strips[0].window_groups = vec![WindowGroup {
        id: 700,
        tabs: vec![Tab {
            id: 7,
            title: "~/src".into(),
            tile: TileContent::Foreign {
                app_id: "Alacritty".into(),
                surface_id: 7,
            },
            app_title: None,
        }],
        active_tab: 7,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }];
    let strip = tree.desktop.workspaces[0].strips[0].id;
    tree.desktop.active_group.insert(strip, 700);
    tree.seed_doc_from_cache();
    let chrome = ChromeState::new(tree);
    let mut host =
        ShellOverlayHost::with_fixture_content(Box::new(Container::new())).with_chrome(chrome);
    host.rect = Rect::new(0.0, 0.0, 1600.0, 900.0);
    host.search_registry.register(Rc::new(HostApps));
    host.search_registry.register(Rc::new(
        crate::shell::search::providers::DesktopWindowsProvider::new(),
    ));
    host
}

fn search(host: &mut ShellOverlayHost, doc: &AutoCommit, query: &str) {
    host.open_palette_with_scope(crate::shell::chrome_state::SearchScope::All);
    host.palette.set_query(query);
    host.refresh_search_results(doc, &automerge::ROOT);
}

fn select(host: &mut ShellOverlayHost, pick: impl Fn(&SearchResult) -> bool) {
    let at = host
        .search_results
        .iter()
        .position(pick)
        .expect("the row is listed");
    let row = host
        .palette
        .visible_entries()
        .iter()
        .position(|entry| entry.id == format!("result:{at}"))
        .expect("the row is shown");
    host.palette.set_cursor(row);
}

fn is_window(row: &SearchResult) -> bool {
    row.provider_id == tilo_desktop::search::WINDOWS_PROVIDER_ID && row.kind == ResourceKind::Tab
}

fn starts() -> Vec<OpenTarget> {
    STARTS.with(|starts| starts.borrow().clone())
}

#[test]
fn the_app_and_its_open_window_are_separate_rows_and_the_app_starts_it() {
    let mut host = host();
    let mut doc = AutoCommit::new();
    search(&mut host, &doc, "alacritty");
    assert!(host.search_results.iter().any(is_window));
    assert!(
        host.search_results
            .iter()
            .any(|row| row.provider_id == "test-host-apps")
    );

    select(&mut host, |row| row.provider_id == "test-host-apps");
    host.activate_selected_result_with(
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
        None,
    );
    assert_eq!(starts().len(), 1);
    drop(host);
}

#[test]
fn enter_on_the_window_switches_to_it_and_starts_nothing() {
    let mut host = host();
    let mut doc = AutoCommit::new();
    search(&mut host, &doc, "alacritty");
    select(&mut host, is_window);
    host.activate_selected_result_with(
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
        None,
    );
    assert!(starts().is_empty());
    drop(host);
}

#[test]
fn shift_enter_on_the_window_opens_a_new_window_of_its_app_here() {
    let mut host = host();
    let mut doc = AutoCommit::new();
    search(&mut host, &doc, "src");
    let entry_label = host
        .palette
        .visible_entries()
        .iter()
        .find(|entry| entry.label == "~/src")
        .and_then(|entry| {
            entry
                .alternates
                .iter()
                .find(|alternate| alternate.modifier == ActivationModifier::Shift)
                .map(|alternate| alternate.label.clone())
        });
    assert_eq!(entry_label.as_deref(), Some("New window"));

    select(&mut host, is_window);
    host.activate_selected_result_with(
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
        Some(ActivationModifier::Shift),
    );
    assert_eq!(starts(), [OpenTarget::Here(700)]);
    drop(host);
}

#[test]
fn the_window_wears_its_apps_icon() {
    let mut host = host();
    let doc = AutoCommit::new();
    search(&mut host, &doc, "alacritty");
    let icon_of = |label: &str| {
        host.palette
            .visible_entries()
            .iter()
            .find(|entry| entry.label == label)
            .and_then(|entry| entry.icon)
    };
    assert_eq!(icon_of("~/src"), Some(ui::icons::Icon::SquareTerminal));
    assert_eq!(icon_of("~/src"), icon_of("Alacritty"));
}
