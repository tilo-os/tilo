//! The git client's controls live in the window's bar: the repository, the
//! screens, the list column's toggle, Fetch, Pull and Push, the diff's
//! layout and syntax highlighting, and History.

use ui::framework::testing::TestApp;
use ui::window_toolbar::{ToolbarAction, ToolbarEvent, WindowToolbar};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::{OPEN, SPLIT, SYNTAX, UNIFIED, remote_id};
use crate::app::{GitClientApp, Screen, Scroller};
use crate::panels::RemoteAction;
use crate::settings::DiffLayout;

fn painted(width: f64, screen: Screen) -> GitClientApp {
    let (model, diffs) = crate::sample::model();
    let mut app = GitClientApp::from_model(model, diffs);
    app.screen = screen;
    app.rect = Rect::new(0.0, 0.0, width, 800.0);
    app.drawer.resolve(app.rect);
    app.drawer.snap(app.drawer.is_open());
    paint(&mut app);
    app
}

fn paint(app: &mut GitClientApp) {
    let theme = ui::theme::Theme::dark();
    let mut text = ui::text_render::TextRenderer::new();
    ui::Widget::draw(
        app,
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut text,
        &theme,
        &ui::presence::PresenceCtx::default(),
    );
}

/// Where the window's bar puts the location of `toolbar` in a window
/// `width` wide.
fn location_in_bar(toolbar: WindowToolbar, width: f64) -> Rect {
    use ui::widget::{TabBar, TabDescriptor, Widget};
    let mut bar = TabBar::new("git", vec![TabDescriptor::new(1, "Git")], 1);
    bar.set_window_controls(true);
    bar.set_toolbar(1, Some(toolbar));
    let theme = ui::theme::Theme::dark();
    let mut taffy = taffy::TaffyTree::<()>::new();
    let mut nodes = std::collections::HashMap::new();
    let node = bar.build_layout(&mut taffy, &theme, &mut nodes);
    let root = taffy
        .new_with_children(
            taffy::Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                size: taffy::Size {
                    width: taffy::Dimension::length(width as f32),
                    height: taffy::Dimension::auto(),
                },
                ..taffy::Style::default()
            },
            &[node],
        )
        .expect("the bar's column");
    taffy
        .compute_layout(
            root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(width as f32),
                height: taffy::AvailableSpace::MaxContent,
            },
        )
        .expect("the bar lays out");
    bar.apply_layout(&taffy, node, vello::kurbo::Point::ZERO);
    bar.toolbar_rects().2
}

fn action(id: &str) -> ToolbarEvent {
    ToolbarEvent::Action { id: id.into() }
}

fn find<'a>(toolbar: &'a WindowToolbar, id: &str) -> Option<&'a ToolbarAction> {
    toolbar.actions.iter().find(|action| action.id == id)
}

/// Every name and action target in `node`'s subtree.
fn labels(node: &ui::a11y::AccessNode, out: &mut Vec<String>) {
    out.extend(node.name.clone());
    out.extend(node.action_target.clone());
    for child in &node.children {
        labels(child, out);
    }
}

#[test]
fn the_client_draws_no_header_row_and_its_controls_are_in_the_window_bar() {
    let app = painted(1280.0, Screen::Changes);
    let toolbar = app.window_toolbar();
    assert_eq!(
        toolbar
            .location
            .as_ref()
            .map(|location| location.label.as_str()),
        Some(app.model.name.as_str()),
        "the bar names the repository"
    );
    let views = toolbar
        .views
        .as_ref()
        .expect("the screens are the bar's views");
    let ids: Vec<&str> = views.choices.iter().map(|view| view.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "git.screen.changes",
            "git.screen.history",
            "git.screen.branches"
        ]
    );
    assert_eq!(views.chosen, "git.screen.changes");
    assert_eq!(toolbar.sidebar, Some(true), "the list column's toggle");
    for remote in RemoteAction::ALL {
        let button = find(&toolbar, remote_id(remote)).expect("a remote command is a bar action");
        assert!(
            button.enabled && !button.menu,
            "{remote:?} stands in the row"
        );
    }
    for (id, on) in [
        (UNIFIED, true),
        (SPLIT, false),
        (SYNTAX, true),
        (OPEN, false),
    ] {
        let item = find(&toolbar, id).expect("a diff preference is in the bar's menu");
        assert!(item.menu, "{id} is a menu item");
        assert_eq!(item.on, on, "{id} shows the preference");
    }

    let mut drawn = Vec::new();
    labels(
        &ui::Widget::accessibility(&app).expect("the client's semantics"),
        &mut drawn,
    );
    let mut own: Vec<&str> = vec![
        "git.navigation-toggle",
        "git.screen.changes",
        "git.screen.history",
        "git.screen.branches",
        UNIFIED,
        SPLIT,
        SYNTAX,
        ui::widget::HISTORY_CONTROL_ID,
    ];
    own.extend(RemoteAction::ALL.map(remote_id));
    for own in own {
        assert!(
            !drawn.iter().any(|label| label == own),
            "the client draws no {own} of its own: {drawn:?}"
        );
    }

    let list = app
        .scroll_regions
        .iter()
        .find(|region| region.scroller == Scroller::ChangesList)
        .expect("the changes list is painted");
    assert!(
        (list.rect.y0 - app.rect.y0).abs() < 0.5,
        "the list starts at the top: no header row above it: {:?}",
        list.rect
    );
}

#[test]
fn the_bar_shows_only_what_applies_to_the_screen() {
    let history = painted(1280.0, Screen::History).window_toolbar();
    assert_eq!(history.sidebar, Some(true));
    let changes = painted(1280.0, Screen::Changes).window_toolbar();
    assert!(find(&history, UNIFIED).is_none(), "no diff on History");
    assert!(find(&history, remote_id(RemoteAction::Push)).is_some());

    let branches = painted(1280.0, Screen::Panels).window_toolbar();
    assert_eq!(branches.sidebar, Some(false));
    assert!(
        branches.sidebar_unavailable,
        "Branches has no list column: its toggle holds its place, disabled"
    );
    assert!(!history.sidebar_unavailable && !changes.sidebar_unavailable);
    for width in [1280.0, 420.0] {
        let x = |toolbar: &WindowToolbar| location_in_bar(toolbar.clone(), width).x0;
        assert!(
            (x(&changes) - x(&history)).abs() < 0.5 && (x(&changes) - x(&branches)).abs() < 0.5,
            "{width} wide, the location starts at one x on every screen"
        );
    }
    assert!(find(&branches, SYNTAX).is_none());

    // The row is the same on every screen, its menu included, so the views
    // do not move when the screen changes.
    let row = |toolbar: &WindowToolbar| -> (Vec<String>, bool) {
        (
            toolbar
                .actions
                .iter()
                .filter(|action| !action.menu)
                .map(|action| action.id.clone())
                .collect(),
            toolbar.actions.iter().any(|action| action.menu),
        )
    };
    assert_eq!(row(&changes), row(&history));
    assert_eq!(row(&changes), row(&branches));
    assert!(row(&changes).1, "every screen has a menu");

    assert_eq!(
        GitClientApp::open_empty().window_toolbar(),
        WindowToolbar::default(),
        "with no repository open the bar has nothing of the client's"
    );
}

#[test]
fn the_bar_switches_screens_and_the_diff_preferences() {
    let mut app = painted(1280.0, Screen::Changes);
    app.run_window_bar_event(&action("git.screen.history"));
    assert_eq!(app.screen, Screen::History);
    app.run_window_bar_event(&action("git.screen.branches"));
    assert_eq!(app.screen, Screen::Panels);
    app.run_window_bar_event(&action("git.screen.changes"));
    assert_eq!(app.screen, Screen::Changes);

    app.run_window_bar_event(&action(SPLIT));
    assert_eq!(app.settings.diff_layout, DiffLayout::SideBySide);
    app.run_window_bar_event(&action(SPLIT));
    assert_eq!(
        app.settings.diff_layout,
        DiffLayout::SideBySide,
        "choosing the chosen layout keeps it"
    );
    assert!(find(&app.window_toolbar(), SPLIT).is_some_and(|item| item.on));
    app.run_window_bar_event(&action(UNIFIED));
    assert_eq!(app.settings.diff_layout, DiffLayout::Unified);

    let syntax = app.settings.syntax_highlight;
    app.run_window_bar_event(&action(SYNTAX));
    assert_eq!(app.settings.syntax_highlight, !syntax);
    assert!(find(&app.window_toolbar(), SYNTAX).is_some_and(|item| item.on != syntax));
}

#[test]
fn the_bar_pushes_the_checked_out_branch() {
    let mut app = painted(1280.0, Screen::Panels);
    let ahead = |app: &GitClientApp| {
        app.model
            .branches
            .iter()
            .find(|branch| branch.is_head)
            .map(|branch| branch.ahead)
    };
    assert!(ahead(&app).is_some_and(|ahead| ahead > 0));
    app.run_window_bar_event(&action(remote_id(RemoteAction::Push)));
    assert_eq!(ahead(&app), Some(0));
}

#[test]
fn history_is_the_window_bars_control() {
    let mut app = TestApp::<crate::GitClientShell>::capture(None).expect("the sample opens");
    app.harness().present_in_window_bar();
    app.harness().relayout();
    assert_eq!(
        app.harness().window_history_control(),
        Some(ui::widget::HistoryControl::Closed),
        "the window's bar offers History"
    );
    app.harness().press_window_history().expect("History opens");
    app.settle();
    assert_eq!(
        app.harness().window_history_control(),
        Some(ui::widget::HistoryControl::Open)
    );
}

#[test]
fn the_location_is_the_repository_and_a_typed_path_opens_another() {
    let mut app = painted(1280.0, Screen::History);
    let location = app.window_toolbar().location.expect("a location");
    assert!(location.editable, "the location is a path to edit");
    assert_eq!(location.label, app.model.name);

    let directory = tempfile::tempdir().expect("a temporary directory");
    git2::Repository::init(directory.path()).expect("a repository");
    let path = directory.path().to_string_lossy().into_owned();
    app.run_window_bar_event(&ToolbarEvent::Commit { text: path });
    assert_eq!(
        app.repo_path.as_deref(),
        Some(directory.path()),
        "the typed repository is open"
    );
    let opened = app
        .repo_path
        .as_ref()
        .map(|opened| opened.to_string_lossy().into_owned());
    assert_eq!(
        app.window_toolbar().location.map(|location| location.value),
        opened,
        "an edit starts from the open repository's path"
    );
}
