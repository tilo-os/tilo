//! An app shown by the display-less driver as a window of its own wears the row of its parts the
//! native runner shows: its parts, the window's search and History, and on a desktop that leaves
//! the frame to the app, the window's own controls. A person's press, a reader's action and an
//! agent's call on a part each reach the app.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixture: a failure is the assertion"
)]

use std::cell::RefCell;
use std::rc::Rc;

use crate::headless::Driver;
use crate::part_row::WindowAction;
use tilo_ui_base::widget::Widget;
use crate::widget::tabs::TOOLBAR_ROW_HEIGHT;
use crate::window_toolbar::{SearchOpening, ToolbarAction, ToolbarEvent, WindowToolbar};

const MUTE: &str = "window-toolbar-action:call-mute";

/// An app whose one control is a part in the window's bar: a call's microphone, named for what a
/// press does, as a call declares it.
struct CallApp {
    content: crate::widget::Column,
    muted: bool,
    heard: Rc<RefCell<Vec<ToolbarEvent>>>,
}

impl tilo_ui_base::widget::Decorator for CallApp {
    type Inner = crate::widget::Column;

    fn inner(&self) -> &Self::Inner {
        &self.content
    }

    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.content
    }

    fn sync(&mut self, state: &tilo_ui_base::widget::AppModel, path: &tilo_ui_base::widget::ModelPath) {
        for event in crate::window_toolbar::take_events() {
            if event
                == (ToolbarEvent::Action {
                    id: "call-mute".into(),
                })
            {
                self.muted = !self.muted;
            }
            self.heard.borrow_mut().push(event);
        }
        let name = if self.muted { "Unmute" } else { "Mute" };
        crate::window_toolbar::declare(Some(WindowToolbar {
            actions: vec![ToolbarAction::new("call-mute", "mic", name)],
            location: Some(crate::window_toolbar::ToolbarLocation {
                label: "Call".into(),
                ..crate::window_toolbar::ToolbarLocation::default()
            }),
            ..WindowToolbar::default()
        }));
        self.content.sync(state, path);
    }
}

fn call_app() -> (Driver, Rc<RefCell<Vec<ToolbarEvent>>>) {
    let heard = Rc::new(RefCell::new(Vec::new()));
    let app = CallApp {
        content: crate::widget::Column::new()
            .push(Box::new(crate::widget::Button::new("send", "Send"))),
        muted: true,
        heard: Rc::clone(&heard),
    };
    let mut driver = Driver::for_root(
        crate::State::new(),
        app,
        (900, 600),
        tilo_ui_base::theme::Theme::dark().with_reduce_motion(true),
    );
    driver.present_in_window_bar();
    driver.relayout();
    (driver, heard)
}

/// The bar's part whose target is `target`.
fn part(driver: &Driver, target: &str) -> Option<tilo_ui_base::a11y::AccessNode> {
    fn find(nodes: &[tilo_ui_base::a11y::AccessNode], target: &str) -> Option<tilo_ui_base::a11y::AccessNode> {
        nodes.iter().find_map(|node| {
            if node.action_target.as_deref() == Some(target) {
                Some(node.clone())
            } else {
                find(&node.children, target)
            }
        })
    }
    find(&driver.window_parts(), target)
}

/// Every name the bar's parts carry.
fn names(driver: &Driver) -> Vec<String> {
    fn walk(nodes: &[tilo_ui_base::a11y::AccessNode], out: &mut Vec<String>) {
        for node in nodes {
            out.extend(node.name.clone());
            walk(&node.children, out);
        }
    }
    let mut out = Vec::new();
    walk(&driver.window_parts(), &mut out);
    out
}

fn click(driver: &mut Driver, bounds: tilo_ui_base::a11y::AccessBounds) {
    let (x, y) = ((bounds.x0 + bounds.x1) / 2.0, (bounds.y0 + bounds.y1) / 2.0);
    driver.click_at(x, y);
}

#[test]
fn the_window_shows_the_apps_parts_in_a_row_above_its_frame() {
    let (driver, _) = call_app();
    assert!(driver.presents_window_bar());
    assert!(
        crate::window_toolbar::is_hosted(),
        "the app hears its parts are hosted"
    );
    let frame = Widget::get_rect(driver.root::<CallApp>());
    assert!(
        (frame.y0 - TOOLBAR_ROW_HEIGHT).abs() < f64::EPSILON,
        "the app's frame starts under the row: {frame:?}"
    );
    let mute = part(&driver, MUTE).expect("the microphone stands in the row");
    assert_eq!(mute.name.as_deref(), Some("Unmute"));
    let bounds = mute.bounds.expect("the part says where it stands");
    assert!(bounds.y1 <= TOOLBAR_ROW_HEIGHT && bounds.x1 > bounds.x0);
    assert!(
        part(&driver, "window-search-open").is_some(),
        "the row ends with the window's search"
    );
    let window = driver.a11y_tree_update();
    assert!(
        window
            .nodes
            .iter()
            .any(|(_, node)| node.label() == Some("Unmute")),
        "the window's accessibility tree carries the row"
    );
}

#[test]
fn activating_a_part_reaches_the_app_and_the_row_shows_its_outcome() {
    let (mut driver, heard) = call_app();
    driver
        .perform_window_part(MUTE, &tilo_ui_base::a11y::Action::Activate)
        .expect("the part runs");
    assert_eq!(
        heard.borrow().as_slice(),
        [ToolbarEvent::Action {
            id: "call-mute".into()
        }]
    );
    assert_eq!(
        part(&driver, MUTE).and_then(|mute| mute.name).as_deref(),
        Some("Mute"),
        "the row shows the app's new state"
    );
    assert!(
        driver
            .perform_window_part(
                "window-toolbar-action:missing",
                &tilo_ui_base::a11y::Action::Activate
            )
            .is_err(),
        "a part the app does not declare runs nothing"
    );
    driver
        .press_window_action("Mute")
        .expect("the part is found by what it says");
    assert_eq!(heard.borrow().len(), 2);
}

// A person's press on the part, where the row paints it, is the same press, and never reaches the
// app's own controls under the row; a reader's action on its node is too.
#[test]
fn a_pointer_press_and_a_readers_action_on_a_part_reach_the_app() {
    let (mut driver, heard) = call_app();
    let bounds = part(&driver, MUTE)
        .and_then(|mute| mute.bounds)
        .expect("the part stands in the row");
    click(&mut driver, bounds);
    assert_eq!(
        heard.borrow().as_slice(),
        [ToolbarEvent::Action {
            id: "call-mute".into()
        }]
    );
    assert_eq!(
        part(&driver, MUTE).and_then(|mute| mute.name).as_deref(),
        Some("Mute")
    );
    let node = driver
        .a11y_node_named("Mute")
        .expect("the part is in the window's tree");
    driver
        .perform_a11y_action(node, &tilo_ui_base::a11y::Action::Activate)
        .expect("a reader presses it");
    assert_eq!(heard.borrow().len(), 2, "{:?}", heard.borrow());
}

#[test]
fn a_window_that_draws_its_frame_has_one_bar_with_the_windows_controls() {
    let (mut driver, _) = call_app();
    assert!(!driver.hosts_window_controls(), "a page hosts no controls");
    driver.host_window_controls();
    assert!(driver.hosts_window_controls());
    let frame = Widget::get_rect(driver.root::<CallApp>());
    assert!(
        (frame.y0 - TOOLBAR_ROW_HEIGHT).abs() < f64::EPSILON,
        "the window has one bar: the app starts under it, not under two: {frame:?}"
    );
    for (target, name) in [
        ("window-control-minimize", "Minimize"),
        ("window-control-maximize", "Maximize"),
        ("window-control-close", "Close"),
    ] {
        let control = part(&driver, target).expect("the control is reachable");
        assert_eq!(control.name.as_deref(), Some(name), "{target}");
        assert!(control.state.focusable, "{target} takes focus");
        let bounds = control.bounds.expect("the control says where it stands");
        assert!(
            bounds.y1 <= TOOLBAR_ROW_HEIGHT && bounds.x1 > bounds.x0,
            "{target} stands in the bar: {bounds:?}"
        );
    }
    driver
        .perform_window_part("window-control-maximize", &tilo_ui_base::a11y::Action::Activate)
        .expect("the control runs");
    assert!(driver.window_maximized(), "the window maximizes");
    assert_eq!(
        part(&driver, "window-control-maximize")
            .and_then(|control| control.name)
            .as_deref(),
        Some("Restore"),
        "the maximized window's control restores it"
    );
    let close = part(&driver, "window-control-close")
        .and_then(|control| control.bounds)
        .expect("the close control stands in the bar");
    click(&mut driver, close);
    assert_eq!(
        driver.take_window_actions(),
        [WindowAction::ToggleMaximize, WindowAction::Close],
        "the presses reach the window"
    );
}

// The bar names what the window is on with a label that runs nothing: a reader that presses it is
// told nothing was refused, and no part runs.
#[test]
fn a_press_on_the_rows_own_label_runs_nothing() {
    let (mut driver, heard) = call_app();
    assert!(
        names(&driver).iter().any(|name| name == "Call"),
        "the call is named in the row: {:?}",
        names(&driver)
    );
    let label = driver
        .a11y_node_named("Call")
        .expect("the label is in the window's tree");
    driver
        .perform_a11y_action(label, &tilo_ui_base::a11y::Action::Activate)
        .expect("a press on the row's own label is not refused");
    assert!(
        heard.borrow().is_empty(),
        "no part ran: {:?}",
        heard.borrow()
    );
    assert!(driver.take_window_actions().is_empty());
}

/// An app whose bar holds a menu, as a list's sort menu does: the row's button says what the menu
/// is, and the rows it lists run by their id.
struct SortApp {
    content: crate::widget::Column,
    heard: Rc<RefCell<Vec<ToolbarEvent>>>,
}

impl tilo_ui_base::widget::Decorator for SortApp {
    type Inner = crate::widget::Column;

    fn inner(&self) -> &Self::Inner {
        &self.content
    }

    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.content
    }

    fn sync(&mut self, state: &tilo_ui_base::widget::AppModel, path: &tilo_ui_base::widget::ModelPath) {
        for event in crate::window_toolbar::take_events() {
            self.heard.borrow_mut().push(event);
        }
        crate::window_toolbar::declare(Some(WindowToolbar {
            actions: vec![
                ToolbarAction::new("sort", "arrow-up-down", "Sort")
                    .labelled()
                    .with_items(vec![
                        ToolbarAction::new("sort~row-0", "", "Everything"),
                        ToolbarAction::new("sort~row-1", "", "Title"),
                    ]),
            ],
            ..WindowToolbar::default()
        }));
        self.content.sync(state, path);
    }
}

#[test]
fn a_menu_in_the_bar_opens_on_one_click_stays_open_and_runs_its_rows() {
    let heard = Rc::new(RefCell::new(Vec::new()));
    let app = SortApp {
        content: crate::widget::Column::new()
            .push(Box::new(crate::widget::Button::new("send", "Send"))),
        heard: Rc::clone(&heard),
    };
    let mut driver = Driver::for_root(
        crate::State::new(),
        app,
        (900, 600),
        tilo_ui_base::theme::Theme::dark().with_reduce_motion(true),
    );
    driver.present_in_window_bar();
    let button = part(&driver, "window-toolbar-action:sort")
        .and_then(|button| button.bounds)
        .expect("the sort menu stands in the row");
    click(&mut driver, button);
    assert!(
        names(&driver).iter().any(|name| name == "Title"),
        "one click opens the menu: {:?}",
        names(&driver)
    );
    for _ in 0..4 {
        driver.relayout();
    }
    assert!(
        names(&driver).iter().any(|name| name == "Title"),
        "the menu stays open: {:?}",
        names(&driver)
    );
    assert!(
        heard.borrow().is_empty(),
        "opening the menu runs none of its rows"
    );
    driver
        .press_window_action("Title")
        .expect("the menu's row runs");
    assert_eq!(
        heard.borrow().as_slice(),
        [ToolbarEvent::Action {
            id: "sort~row-1".into()
        }]
    );
}

fn settings_like_list() -> crate::widget::NavList {
    use crate::widget::{NavItem, NavList, NavSection};
    NavList::new(vec![
        NavSection::new(
            "personal",
            "Personal",
            vec![
                NavItem::new("appearance", "Appearance"),
                NavItem::new("sound", "Sound"),
                NavItem::new("time", "Time & date"),
            ],
        )
        .filterable(),
    ])
}

// The window's search is the window's one search entry: Find opens it on the app's list, what is
// typed narrows the list in the next frame, and closing it shows the whole list.
#[test]
fn the_windows_search_finds_in_the_apps_list_in_place() {
    let mut driver = Driver::for_root(
        crate::State::new(),
        settings_like_list(),
        (800, 600),
        tilo_ui_base::theme::Theme::dark().with_reduce_motion(true),
    );
    driver.present_in_window_bar();
    let filter = |driver: &Driver| driver.root::<crate::widget::NavList>().filter().to_owned();
    driver.search_in_window(SearchOpening::Find, "s");
    assert_eq!(driver.window_search_query(), "s");
    assert_eq!(filter(&driver), "s", "the frame narrows the list");
    assert!(
        driver.window_search_count().is_some(),
        "the search counts what the list kept"
    );
    assert!(!driver.window_search_replaces(), "a list offers no Replace");
    assert!(!driver.replace_in_window("x", false));
    driver
        .perform_window_part("window-search-close", &tilo_ui_base::a11y::Action::Activate)
        .expect("the search closes");
    assert_eq!(
        filter(&driver),
        "",
        "closing the search shows the whole list"
    );
}

// Without its bar presented, the window has no parts to run: a call on one says how to show it.
#[test]
fn a_part_of_an_unpresented_bar_is_refused_legibly() {
    let mut driver = Driver::for_root(
        crate::State::new(),
        settings_like_list(),
        (800, 600),
        tilo_ui_base::theme::Theme::dark(),
    );
    assert!(!driver.presents_window_bar());
    let error = driver
        .perform_window_part(MUTE, &tilo_ui_base::a11y::Action::Activate)
        .expect_err("no bar, no part");
    assert!(error.contains("present_in_window_bar"), "{error}");
}

// ---- every part of the bar gets its own place at every width ----

use tilo_ui_base::a11y::AccessNode;
use crate::widget::window_toolbar_row::{
    ACTION_TARGET_PREFIX, LEADING_TARGET_PREFIX, LOCATION_TARGET, OVERFLOW_TARGET,
    SEGMENT_TARGET_PREFIX, SIDEBAR_TARGET, VIEW_TARGET_PREFIX, VIEWS_MENU_TARGET,
};
use crate::window_toolbar::{
    LocationSegment, ToolbarLocation, ToolbarNavigation, ToolbarView, ToolbarViews,
};

const BAR_WIDTHS: [u32; 12] = [
    320, 360, 390, 420, 480, 560, 640, 720, 840, 1024, 1280, 1440,
];

/// An app that is only its name: the calendar's, as a shell window's tab wears it.
struct NamedApp;

impl Widget for NamedApp {
    fn name(&self) -> &str {
        "NamedApp"
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _: &tilo_ui_base::theme::Theme,
        nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        use tilo_ui_base::widget::TaffyTreeExt as _;
        taffy.widget_leaf(taffy::Style::default(), nodes, self)
    }
}

impl crate::widget::AppWidget for NamedApp {
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::App(tilo_ui_base::icons::Icon::Calendar)
    }

    fn app_title(&self) -> &str {
        "Calendar"
    }

    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Named("calendar".into())
    }
}

/// Clear the thread's declared toolbar when the test ends, however it ends.
struct DeclaredReset;

impl Drop for DeclaredReset {
    fn drop(&mut self) {
        crate::window_toolbar::declare(None);
        crate::window_toolbar::set_hosted(false);
    }
}

/// A folder deep in the home, as Files names it: its levels a breadcrumb, its path editable, and
/// Files' actions.
fn deep_folder() -> WindowToolbar {
    let levels = [
        "Home",
        "Documents",
        "Projects",
        "Tilo",
        "designs",
        "app-language",
    ];
    WindowToolbar {
        sidebar: Some(true),
        navigation: Some(ToolbarNavigation {
            back: true,
            forward: false,
        }),
        location: Some(ToolbarLocation {
            label: "app-language".into(),
            value: "~/Documents/Projects/Tilo/designs/app-language".into(),
            editable: true,
            segments: levels
                .iter()
                .map(|label| LocationSegment {
                    label: (*label).into(),
                    value: (*label).into(),
                })
                .collect(),
            ..ToolbarLocation::default()
        }),
        actions: vec![
            ToolbarAction::new("view", "layout-grid", "Show as grid"),
            ToolbarAction::new("new-folder", "folder-plus", "New folder"),
            ToolbarAction::new("up", "arrow-up", "Up").in_menu(),
            ToolbarAction::new("new-file", "file-plus", "New file").in_menu(),
            ToolbarAction::new("terminal", "square-terminal", "Open in Terminal").in_menu(),
            ToolbarAction::new("hidden", "eye", "Show hidden files").in_menu(),
        ],
        ..WindowToolbar::default()
    }
}

/// Files' bar: the workspace as its location, Upload as its main action, New folder after it.
fn files() -> WindowToolbar {
    let mut files = deep_folder();
    files.location = Some(ToolbarLocation {
        label: "Workspace".into(),
        value: "Workspace".into(),
        editable: true,
        ..ToolbarLocation::default()
    });
    files.actions.insert(
        0,
        ToolbarAction::new("upload", "upload", "Upload files").primary(),
    );
    files
}

/// A week across a month, as Calendar names it: Monday, June 29 to Sunday, July 5, 2026.
fn week_across_a_month() -> tilo_ui_base::format::Period {
    let day = |m, d| jiff::civil::Date::new(2026, m, d).ok().unwrap_or_default();
    tilo_ui_base::format::Period::Days(day(6, 29), day(7, 5))
}

/// Calendar's bar: previous and next as a pair, Today, the week as its period, and New event.
fn calendar() -> WindowToolbar {
    WindowToolbar {
        sidebar: Some(true),
        leading: vec![
            ToolbarAction::new("previous", "chevron-left", "Previous week").navigating(),
            ToolbarAction::new("next", "chevron-right", "Next week").navigating(),
            ToolbarAction::new("today", "calendar", "Today"),
        ],
        location: Some(ToolbarLocation::period(week_across_a_month())),
        views: Some(ToolbarViews {
            name: "View".into(),
            choices: ["Month", "Week", "Day", "Agenda", "Tasks"]
                .into_iter()
                .map(|label| ToolbarView {
                    id: label.to_lowercase(),
                    label: label.into(),
                })
                .collect(),
            chosen: "week".into(),
        }),
        actions: vec![
            ToolbarAction::new("new-event", "plus", "New event").primary(),
            ToolbarAction::new("export", "upload", "Export calendar").in_menu(),
        ],
        ..WindowToolbar::default()
    }
}

/// The window showing `toolbar` at `width`, presented with its bar.
fn bar_driver(toolbar: &WindowToolbar, width: u32) -> Driver {
    crate::window_toolbar::declare(Some(toolbar.clone()));
    let mut driver = Driver::for_app(
        crate::State::new(),
        NamedApp,
        (width, 800),
        tilo_ui_base::theme::Theme::dark().with_reduce_motion(true),
    );
    driver.present_in_window_bar();
    driver
}

/// Every target the bar offers, which the overflow menu's entries offer too.
fn offered(nodes: &[AccessNode], out: &mut std::collections::BTreeSet<String>) {
    for node in nodes {
        if let Some(target) = &node.action_target {
            out.insert(target.clone());
        }
        offered(&node.children, out);
    }
}

/// A part the bar shows in a place of its own: a positive box, not one hidden in a menu. A part
/// that runs nothing itself (the period a calendar shows as its place) has no target.
#[derive(Debug)]
struct ShownPart {
    target: Option<String>,
    name: Option<String>,
    role: tilo_ui_base::a11y::Role,
    rect: vello::kurbo::Rect,
}

fn standing(nodes: &[AccessNode], out: &mut Vec<ShownPart>) {
    for node in nodes {
        if node.state.hidden {
            continue;
        }
        if let Some(bounds) = node.bounds
            && bounds.x1 > bounds.x0
            && bounds.y1 > bounds.y0
        {
            out.push(ShownPart {
                target: node.action_target.clone(),
                name: node.name.clone(),
                role: node.role,
                rect: bounds.to_rect(),
            });
        }
        standing(&node.children, out);
    }
}

/// The part that is the bar's place: the editable location, the period a calendar shows there, or
/// the views picker when the bar shows the view's name in the location.
fn location_part<'a>(placed: &'a [ShownPart], toolbar: &WindowToolbar) -> Option<&'a ShownPart> {
    placed
        .iter()
        .find(|part| part.target.as_deref() == Some(LOCATION_TARGET))
        .or_else(|| {
            let location = toolbar.location.as_ref()?;
            placed.iter().find(|part| {
                part.target.is_none()
                    && part.role == tilo_ui_base::a11y::Role::StaticText
                    && part.name.as_deref().is_some_and(|name| {
                        name == location.label.as_str()
                            || location
                                .shorter
                                .iter()
                                .any(|shorter| shorter.as_str() == name)
                    })
            })
        })
        .or_else(|| {
            placed
                .iter()
                .find(|part| part.target.as_deref() == Some(VIEWS_MENU_TARGET))
        })
}

fn overlaps(a: vello::kurbo::Rect, b: vello::kurbo::Rect) -> bool {
    let intersection = a.intersect(b);
    intersection.width() > 0.01 && intersection.height() > 0.01
}

/// The targets `toolbar` declares, each as the bar names it.
fn declared(toolbar: &WindowToolbar) -> Vec<String> {
    let mut declared = Vec::new();
    if toolbar.sidebar.is_some() {
        declared.push(SIDEBAR_TARGET.to_owned());
    }
    if toolbar.navigation.is_some() {
        declared.push(crate::widget::window_toolbar_row::BACK_TARGET.to_owned());
        declared.push(crate::widget::window_toolbar_row::FORWARD_TARGET.to_owned());
    }
    for action in &toolbar.leading {
        declared.push(format!("{LEADING_TARGET_PREFIX}{}", action.id));
    }
    if toolbar.location.is_some() {
        // The views picker takes the location's place where the bar shows the view's name there.
        declared.push(LOCATION_TARGET.to_owned());
    }
    if let Some(views) = &toolbar.views {
        for view in &views.choices {
            declared.push(format!("{VIEW_TARGET_PREFIX}{}", view.id));
        }
    }
    for action in &toolbar.actions {
        declared.push(format!("{ACTION_TARGET_PREFIX}{}", action.id));
    }
    declared
}

/// At every width every declared part is offered, no two parts stand over each other, each stands
/// inside the window and its bar, and the location stays clear: the narrowest bar folds what yields
/// to it into its own menu rather than overlapping it.
#[test]
#[serial_test::serial]
fn every_bar_gives_every_part_its_own_place_at_every_width() {
    let _reset = DeclaredReset;
    let bars: [(&str, fn() -> WindowToolbar); 4] = [
        ("en-US", deep_folder),
        ("en-US", files),
        ("en-US", calendar),
        ("de-DE", calendar),
    ];
    for (locale, make) in bars {
        tilo_ui_base::format::speak_locale(locale);
        let toolbar = make();
        let declared = declared(&toolbar);
        for width in BAR_WIDTHS {
            let driver = bar_driver(&toolbar, width);
            let parts = driver.window_parts();
            let mut offered_targets = std::collections::BTreeSet::new();
            offered(&parts, &mut offered_targets);
            let mut placed = Vec::new();
            standing(&parts, &mut placed);
            let location = location_part(&placed, &toolbar);
            for target in &declared {
                let present = if target.as_str() == LOCATION_TARGET {
                    location.is_some()
                } else {
                    offered_targets.contains(target)
                };
                assert!(
                    present,
                    "{locale} {width} px: {target} is offered nowhere: {offered_targets:?}"
                );
            }
            let Some(location) = location else {
                panic!("{locale} {width} px: the bar names a place: {placed:?}")
            };
            let window = vello::kurbo::Rect::new(0.0, 0.0, f64::from(width), 800.0);
            let band = driver.window_bar_height();
            for part in &placed {
                assert!(
                    part.rect.x0 >= -0.01 && part.rect.x1 <= window.x1 + 0.01,
                    "{locale} {width} px: {part:?} stands inside the window"
                );
                assert!(
                    part.rect.y0 >= -0.01 && part.rect.y1 <= band + 0.5,
                    "{locale} {width} px: {part:?} stands in the bar {band} px tall"
                );
            }
            // The location's own levels share its box; every other part keeps its place.
            let controls: Vec<&ShownPart> = placed
                .iter()
                .filter(|part| {
                    !part
                        .target
                        .as_deref()
                        .is_some_and(|target| target.starts_with(SEGMENT_TARGET_PREFIX))
                })
                .filter(|part| !std::ptr::eq(*part, location))
                .collect();
            for (index, part) in controls.iter().enumerate() {
                assert!(
                    !overlaps(location.rect, part.rect),
                    "{locale} {width} px: {:?} runs under {part:?}",
                    location.rect
                );
                for other in controls.iter().skip(index + 1) {
                    assert!(
                        !overlaps(part.rect, other.rect),
                        "{locale} {width} px: {part:?} over {other:?}"
                    );
                }
            }
        }
    }

    // Files keeps its actions in the row at a wide width: only the menu's own are behind it.
    let files = files();
    let driver = bar_driver(&files, 1280);
    let mut placed = Vec::new();
    standing(&driver.window_parts(), &mut placed);
    for id in ["view", "new-folder"] {
        let target = format!("{ACTION_TARGET_PREFIX}{id}");
        assert!(
            placed
                .iter()
                .any(|part| part.target.as_deref() == Some(target.as_str())),
            "1280 px: {id} stands in the row: {placed:?}"
        );
    }

    // A compact calendar window folds its views into the overflow menu, which names them, and
    // keeps its location and that menu in places of their own.
    let calendar = calendar();
    let driver = bar_driver(&calendar, 320);
    let parts = driver.window_parts();
    let mut offered_targets = std::collections::BTreeSet::new();
    offered(&parts, &mut offered_targets);
    let mut placed = Vec::new();
    standing(&parts, &mut placed);
    let location_stands = location_part(&placed, &calendar).is_some();
    let menu_stands = placed.iter().any(|part| {
        part.target.as_deref() == Some(OVERFLOW_TARGET)
            || part.target.as_deref() == Some(VIEWS_MENU_TARGET)
    });
    assert!(
        location_stands && menu_stands,
        "320 px: the location and the menu its parts fold into stand: {placed:?}"
    );
    for view in ["month", "week", "day", "agenda", "tasks"] {
        let target = format!("{VIEW_TARGET_PREFIX}{view}");
        let stands = placed
            .iter()
            .any(|part| part.target.as_deref() == Some(target.as_str()));
        assert!(
            !stands && offered_targets.contains(&target),
            "320 px: {view} folds into the overflow menu that offers it: {placed:?}"
        );
    }
}
