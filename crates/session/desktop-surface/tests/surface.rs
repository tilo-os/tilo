//! The desktop surface inside the toolkit's real frame pipeline: navigation
//! commands move the model and the live widgets together.

#![allow(
    clippy::unwrap_used,
    reason = "test fixtures: a failed lookup is the assertion"
)]

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use tilo_desktop_model::{IdMinter, Tab, TileContent, WindowWidth, Workspace};
use tilo_desktop_surface::navigation::{Command, Route};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource, seed_tree};
use ui::widget::TileHost;

struct Notices(Rc<Cell<usize>>);

impl TileSource for Notices {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        self.0.set(self.0.get().saturating_add(1));
        TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn launch(&mut self, _: &Workspace, id: u64, app: &str) -> Option<Tab> {
        if app.starts_with(NODE_APP) {
            // Opened on the node; its window arrives from there.
            NODE_LAUNCHES.with(|launches| launches.borrow_mut().push(app.into()));
            return None;
        }
        let title = app.strip_prefix("so.tilo.")?.to_owned();
        Some(Tab {
            id,
            title: title.clone(),
            tile: TileContent::LocalApp {
                app_id: app.into(),
                label: title,
                note: String::new(),
            },
            app_title: None,
        })
    }

    fn search_applications(&self) -> Vec<tilo_desktop_search::providers::AppEntry> {
        ["Sheets", "Writer"]
            .into_iter()
            .map(|title| tilo_desktop_search::providers::AppEntry {
                id: format!("so.tilo.{}", title.to_lowercase()),
                title: title.into(),
                category: Some("Office".into()),
                description: None,
            })
            .chain([tilo_desktop_search::providers::AppEntry {
                id: format!("{NODE_APP}app.so.tilo.terminal"),
                title: "Terminal".into(),
                category: Some("System".into()),
                description: Some("Runs on the node".into()),
            }])
            .collect()
    }
}

/// Launch prefix of an app a node runs for this desktop.
const NODE_APP: &str = "node:";

thread_local! {
    /// The node apps this thread's desktops were asked to open.
    static NODE_LAUNCHES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn tab(minter: &mut IdMinter, title: &str) -> Tab {
    Tab {
        id: minter.mint(),
        title: title.into(),
        tile: TileContent::LocalApp {
            app_id: format!("so.tilo.{}", title.to_lowercase()),
            label: title.into(),
            note: String::new(),
        },
        app_title: None,
    }
}

fn mount() -> (ui::headless::FrameHarness, DesktopHandle, Rc<Cell<usize>>) {
    mount_with(&["Notes", "Scratch"])
}

/// A desk of one window per title, each of the app named like it.
fn mount_with(titles: &[&str]) -> (ui::headless::FrameHarness, DesktopHandle, Rc<Cell<usize>>) {
    let mut minter = IdMinter::new(7);
    let tabs = titles.iter().map(|title| tab(&mut minter, title)).collect();
    let tree = seed_tree(&mut minter, "Desk", tabs);
    let built = Rc::new(Cell::new(0));
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Notices(Rc::clone(&built))));
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    driver.set_caller_palette_enabled(false);
    driver.relayout();
    (driver, handle, built)
}

fn dispatch(
    driver: &mut ui::headless::FrameHarness,
    handle: &DesktopHandle,
    wire: serde_json::Value,
) {
    let command: Command = serde_json::from_value(wire).unwrap();
    handle.dispatch(&command, "g").unwrap();
    driver.relayout();
}

fn widgets(node: &ui::headless::InspectionNode, out: &mut Vec<String>) {
    out.push(node.widget.clone());
    for child in &node.children {
        widgets(child, out);
    }
}

#[test]
fn the_seeded_desktop_presents_each_tab_once_as_real_window_groups() {
    let (mut driver, handle, built) = mount();
    driver.relayout();
    assert_eq!(built.get(), 2, "tiles are built once and retained");
    let mut names = Vec::new();
    widgets(&driver.inspection().root, &mut names);
    assert!(names.iter().any(|n| n == "GroupRow"), "{names:?}");
    assert_eq!(
        names.iter().filter(|n| *n == "WindowGroup").count(),
        2,
        "{names:?}"
    );

    let snapshot = handle.snapshot("g", vec![]);
    assert_eq!(snapshot.entries.len(), 2);
    assert_eq!(
        snapshot.active.as_ref().unwrap(),
        &snapshot.entries[0].route
    );
}

#[test]
fn navigation_selects_resizes_and_closes_through_the_model() {
    let (mut driver, handle, _) = mount();
    let routes: Vec<Route> = handle
        .snapshot("g", vec![])
        .entries
        .into_iter()
        .map(|entry| entry.route)
        .collect();
    let before = handle.revision();

    dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"select","generation":"g","route":routes[1]}),
    );
    assert_eq!(handle.snapshot("g", vec![]).active.unwrap(), routes[1]);
    assert!(handle.revision() > before);

    dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"width","generation":"g","route":routes[1],"permille":400}),
    );
    let group: u64 = routes[1].group.parse().unwrap();
    let width = handle.read(|tree| {
        tree.workspaces[0].strips[0]
            .window_groups
            .iter()
            .find(|g| g.id == group)
            .unwrap()
            .width
    });
    assert_eq!(width, WindowWidth::Fraction(400));
    assert_eq!(handle.snapshot("g", vec![]).width_permille, Some(400));

    dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"close","generation":"g","route":routes[0]}),
    );
    let snapshot = handle.snapshot("g", vec![]);
    assert_eq!(snapshot.entries.len(), 1);
    assert_eq!(snapshot.entries[0].route, routes[1]);

    let stale: Command = serde_json::from_value(
        serde_json::json!({"kind":"select","generation":"g","route":routes[0]}),
    )
    .unwrap();
    assert_eq!(
        handle.dispatch(&stale, "g").unwrap_err(),
        "Window unavailable"
    );
}

#[test]
fn a_replaced_tree_keeps_surviving_tabs_and_builds_only_new_ones() {
    let (mut driver, handle, built) = mount();
    assert_eq!(built.get(), 2);
    let mut tree = handle.read(Clone::clone);
    let strip = &mut tree.workspaces[0].strips[0];
    let removed = strip.window_groups.remove(0);
    let mut minter = IdMinter::new(9);
    let added = tab(&mut minter, "Writer");
    let added_id = added.id;
    strip.window_groups[0].tabs.push(added);
    strip.window_groups[0].active_tab = added_id;
    let before = handle.revision();
    handle.replace_tree(tree);
    driver.relayout();
    assert!(handle.revision() > before);
    assert_eq!(
        built.get(),
        3,
        "only the new tab is built; the kept one moves"
    );

    let snapshot = handle.snapshot("g", vec![]);
    assert_eq!(snapshot.entries.len(), 2);
    let removed_tab = removed.tabs[0].id.to_string();
    assert!(
        snapshot
            .entries
            .iter()
            .all(|entry| entry.route.tab != removed_tab)
    );
    let mut names = Vec::new();
    widgets(&driver.inspection().root, &mut names);
    assert_eq!(
        names.iter().filter(|n| *n == "WindowGroup").count(),
        1,
        "{names:?}"
    );

    let dispatched: Command = serde_json::from_value(
        serde_json::json!({"kind":"select","generation":"g","route":snapshot.entries[0].route}),
    )
    .unwrap();
    handle.dispatch(&dispatched, "g").unwrap();
    driver.relayout();
    assert_eq!(
        built.get(),
        3,
        "navigation after a replace reuses the rebuilt row"
    );
}

#[test]
fn an_idle_desktop_asks_its_host_for_no_frame() {
    let (mut driver, handle, _) = mount();
    let idle = |driver: &ui::headless::FrameHarness| {
        let schedule = driver.frame_schedule();
        !schedule.queued_work
            && !schedule.needs_animation_frame
            && schedule.next_wake_deadline.is_none()
    };
    assert!(idle(&driver), "{:?}", driver.frame_schedule());

    let routes = handle.snapshot("g", vec![]).entries;
    dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"select","generation":"g","route":routes[1].route}),
    );
    assert!(
        idle(&driver),
        "settles after a change: {:?}",
        driver.frame_schedule()
    );
}

#[test]
fn launch_opens_a_focused_window_beside_the_active_one_and_builds_it_once() {
    let (mut driver, handle, built) = mount();
    let before = handle.snapshot("g", vec![]).entries;
    dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"launch","generation":"g","app":"so.tilo.sheets"}),
    );
    assert_eq!(built.get(), 3, "the launched tab is built once");
    let snapshot = handle.snapshot("g", vec![]);
    assert_eq!(snapshot.entries.len(), 3);
    assert_eq!(snapshot.entries[0].route, before[0].route);
    assert_eq!(
        snapshot.entries[1].label, "sheets",
        "opens after the focused window"
    );
    assert_eq!(
        snapshot.active.as_ref().unwrap(),
        &snapshot.entries[1].route
    );
    let mut names = Vec::new();
    widgets(&driver.inspection().root, &mut names);
    assert_eq!(
        names.iter().filter(|n| *n == "WindowGroup").count(),
        3,
        "{names:?}"
    );

    dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"launch","generation":"g","app":"elsewhere"}),
    );
    assert_eq!(
        handle.snapshot("g", vec![]).entries.len(),
        3,
        "an app the host cannot open opens nothing"
    );
    assert_eq!(built.get(), 3);
}

#[test]
fn a_launched_window_is_on_screen_in_the_frame_that_opens_it() {
    fn groups(node: &ui::headless::InspectionNode, out: &mut Vec<[f64; 4]>) {
        if node.widget == "WindowGroup" {
            out.push(node.rect);
        }
        for child in &node.children {
            groups(child, out);
        }
    }
    let (mut driver, handle, _) = mount();
    let launch = |driver: &mut ui::headless::FrameHarness, app: &str| {
        on_clock(FRAME, || {
            dispatch(
                driver,
                &handle,
                serde_json::json!({"kind":"launch","generation":"g","app":app}),
            );
        });
    };
    // Launches a person makes come seconds apart: each earlier one has
    // come to rest, sharing the screen with its neighbour, by the next.
    for app in ["so.tilo.a", "so.tilo.b", "so.tilo.c"] {
        launch(&mut driver, app);
        settle(&mut driver);
    }
    launch(&mut driver, "so.tilo.d");
    let mut rects = Vec::new();
    groups(&driver.inspection().root, &mut rects);
    rects.sort_by(|a, b| a[0].total_cmp(&b[0]));
    // Notes, a, b, c, d, Scratch: each launch opens after the focused window.
    assert_eq!(rects.len(), 6);
    let [x, _, width, _] = rects[4];
    assert!(
        x >= 0.0 && x + width <= 1200.5,
        "the newest window is in view: {rects:?}"
    );
}

#[test]
fn only_the_focused_window_is_built_before_the_first_frame() {
    let mut minter = IdMinter::new(7);
    let tabs = (0..5)
        .map(|i| tab(&mut minter, &format!("App{i}")))
        .collect();
    let tree = seed_tree(&mut minter, "Desk", tabs);
    let built = Rc::new(Cell::new(0));
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Notices(Rc::clone(&built))));
    assert_eq!(built.get(), 1, "the focused window only");
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    for _ in 0..5 {
        driver.relayout();
    }
    assert_eq!(built.get(), 5, "the rest follow in later frames, each once");
    let schedule = driver.frame_schedule();
    assert!(!schedule.needs_animation_frame, "{schedule:?}");
}

#[test]
fn a_replaced_tree_builds_tabs_that_were_still_deferred() {
    let mut minter = IdMinter::new(7);
    let tabs = vec![tab(&mut minter, "Notes"), tab(&mut minter, "Scratch")];
    let tree = seed_tree(&mut minter, "Desk", tabs);
    let built = Rc::new(Cell::new(0));
    let (mut surface, handle) = DesktopSurface::new(tree, 7, Box::new(Notices(Rc::clone(&built))));
    assert_eq!(built.get(), 1);
    handle.replace_tree(handle.read(Clone::clone));
    let state = ui::automerge::AutoCommit::new();
    ui::widget::Widget::update(&mut surface, &state, &ui::automerge::ROOT);
    assert_eq!(
        built.get(),
        2,
        "the deferred tab is built, not carried over as a placeholder"
    );
}

#[test]
fn a_restored_strip_opens_on_its_focused_window() {
    fn groups(node: &ui::headless::InspectionNode, out: &mut Vec<[f64; 4]>) {
        if node.widget == "WindowGroup" {
            out.push(node.rect);
        }
        for child in &node.children {
            groups(child, out);
        }
    }
    let mut minter = IdMinter::new(7);
    let tabs = (0..6)
        .map(|i| tab(&mut minter, &format!("App{i}")))
        .collect();
    let mut tree = seed_tree(&mut minter, "Desk", tabs);
    let strip = &tree.workspaces[0].strips[0];
    let (strip_id, last) = (strip.id, strip.window_groups[5].id);
    tree.set_active_group(strip_id, last);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Notices(Rc::new(Cell::new(0)))));
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    driver.relayout();
    let mut rects = Vec::new();
    groups(&driver.inspection().root, &mut rects);
    rects.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let [x, _, width, _] = rects[5];
    assert!(
        x >= 0.0 && x + width <= 1200.5,
        "the focused window is in view: {rects:?}"
    );
}

#[test]
fn the_handle_reports_where_each_shown_tab_is_drawn() {
    let (mut driver, handle, _) = mount();
    driver.relayout();
    let shown = handle.shown_tabs();
    assert_eq!(shown.len(), 2, "one active tab per window: {shown:?}");
    let tabs: Vec<_> = handle.read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .map(|group| group.active_tab)
            .collect()
    });
    for (tab, rect) in &shown {
        assert!(tabs.contains(tab), "{tab} is a window's active tab");
        assert!(rect.width() > 0.0 && rect.height() > 0.0, "{rect:?}");
    }
    let (left, right) = (shown[0].1, shown[1].1);
    assert!(
        left.x1 <= right.x0 + 0.5,
        "the windows sit side by side: {left:?} {right:?}"
    );
}

/// An in-process app for a tab: `Pages` routes between two screens,
/// `Long` scrolls a tall page, anything else is a notice.
struct Hosted(ui::widget::Element);

impl ui::widget::Decorator for Hosted {
    type Inner = dyn ui::widget::Widget;
    fn inner(&self) -> &Self::Inner {
        self.0.as_ref()
    }
    fn inner_mut(&mut self) -> &mut Self::Inner {
        self.0.as_mut()
    }
}

impl ui::widget::AppWidget for Hosted {
    fn uses_host_record(&self) -> bool {
        false
    }
    fn app_kind(&self) -> ui::widget::AppKind {
        ui::widget::AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Hosted"
    }
    fn app_icon(&self) -> ui::widget::IconDescriptor {
        ui::widget::IconDescriptor::Glyph("H")
    }
}

struct Apps(Rc<std::cell::RefCell<Option<ui::widget::RouteHandle>>>);

impl TileSource for Apps {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        let content: ui::widget::Element = match tab.title.as_str() {
            "Pages" => {
                let router = ui::widget::Router::new(
                    "one",
                    ["one", "two"],
                    &ui::Theme::dark(),
                    |_, _, _| Ok(Box::new(ui::widget::Spacer::new()) as ui::widget::Element),
                )
                .unwrap();
                *self.0.borrow_mut() = Some(router.handle());
                Box::new(router)
            }
            "Long" => Box::new(
                ui::widget::Scrollable::new_vertical()
                    .with_height(600.0)
                    .with_child(Box::new(ui::widget::Spacer::fixed(5000.0))),
            ),
            "Greedy" => Box::new(TakesEveryWheel::default()),
            caret if caret.starts_with("Caret") => Box::new(CaretProbe::new(caret)),
            _ => {
                return TileHost::new(ui::widget::TileContent::Notice {
                    title: tab.title.clone(),
                    detail: String::new(),
                });
            }
        };
        TileHost::new(ui::widget::TileContent::App(Box::new(Hosted(content))))
    }
}

/// An app that takes every wheel it is handed, wherever the pointer is, as
/// a chat's message list follows any wheel that reaches it.
#[derive(Default)]
struct TakesEveryWheel {
    rect: ui::kurbo::Rect,
}

impl ui::widget::Widget for TakesEveryWheel {
    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        _: &ui::Theme,
        _: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> ui::taffy::NodeId {
        taffy
            .new_leaf(ui::taffy::Style {
                flex_grow: 1.0,
                ..Default::default()
            })
            .unwrap()
    }

    fn apply_layout(
        &mut self,
        taffy: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: ui::kurbo::Point,
    ) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        if matches!(event, ui::winit::event::WindowEvent::MouseWheel { .. }) {
            *ctx.handled = true;
        }
    }

    fn name(&self) -> &'static str {
        "TakesEveryWheel"
    }

    fn get_rect(&self) -> ui::kurbo::Rect {
        self.rect
    }
}

/// An editor whose caret stays where it is without keyboard focus: its
/// presence key is its title, its caret at 7.
struct CaretProbe {
    key: ui::presence::PresenceKey,
    rect: ui::kurbo::Rect,
}

impl CaretProbe {
    fn new(title: &str) -> Self {
        Self {
            key: ui::presence::PresenceKey::Tab { id: title.into() },
            rect: ui::kurbo::Rect::ZERO,
        }
    }
}

impl ui::widget::Widget for CaretProbe {
    ui::provides!(dyn ui::presence::capability::PublishesPresence);

    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        _: &ui::Theme,
        _: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> ui::taffy::NodeId {
        taffy
            .new_leaf(ui::taffy::Style {
                flex_grow: 1.0,
                ..Default::default()
            })
            .unwrap()
    }

    fn apply_layout(
        &mut self,
        taffy: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: ui::kurbo::Point,
    ) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn name(&self) -> &'static str {
        "CaretProbe"
    }

    fn get_rect(&self) -> ui::kurbo::Rect {
        self.rect
    }
}

impl ui::presence::capability::PublishesPresence for CaretProbe {
    fn presence_key(&self) -> Option<ui::presence::PresenceKey> {
        Some(self.key.clone())
    }

    fn presence_payload(
        &self,
    ) -> Option<(
        ui::presence::PresenceKey,
        ui::collaboration::PresencePayload,
    )> {
        Some((
            self.key.clone(),
            ui::collaboration::PresencePayload::Caret { pos: 7 },
        ))
    }
}

type Routes = Rc<std::cell::RefCell<Option<ui::widget::RouteHandle>>>;

fn mount_apps(titles: &[&str]) -> (ui::headless::FrameHarness, DesktopHandle, Routes) {
    let mut minter = IdMinter::new(7);
    let tabs = titles.iter().map(|title| tab(&mut minter, title)).collect();
    let tree = seed_tree(&mut minter, "Desk", tabs);
    let routes = Routes::default();
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Apps(Rc::clone(&routes))));
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    for _ in 0..titles.len() {
        driver.relayout();
    }
    (driver, handle, routes)
}

fn try_dispatch(
    driver: &mut ui::headless::FrameHarness,
    handle: &DesktopHandle,
    wire: serde_json::Value,
) -> Result<(), String> {
    let command: Command = serde_json::from_value(wire).unwrap();
    handle.dispatch(&command, "g")?;
    driver.relayout();
    Ok(())
}

#[test]
fn pan_moves_the_shown_strip_without_changing_the_saved_tree() {
    let (mut driver, handle, _) = mount_apps(&["A", "B", "C", "D", "E"]);
    let snapshot = handle.snapshot("g", vec![]);
    let pan = snapshot
        .desktop_pan
        .clone()
        .expect("a laid-out strip reports pan");
    assert_eq!(Some(&pan.strip), snapshot.active_strip.as_ref());
    assert!(pan.max > 0.0, "five windows overflow 1200 px: {pan:?}");
    assert!(pan.viewport > 0.0 && pan.content >= pan.viewport, "{pan:?}");
    assert!(pan.offset.abs() < 0.5, "starts at home: {pan:?}");

    let (saved, presented) = (handle.revision(), handle.navigation_revision());
    let target = pan.max / 2.0;
    try_dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"pan","generation":"g","workspace":pan.workspace,"strip":pan.strip,"offset":target}),
    )
    .unwrap();
    let moved = handle.snapshot("g", vec![]).desktop_pan.unwrap();
    assert!((moved.offset - target).abs() < 0.5, "{moved:?}");
    assert_eq!(handle.revision(), saved, "a pan is not a tree change");
    assert!(
        handle.navigation_revision() > presented,
        "but it republishes"
    );

    try_dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"pan","generation":"g","workspace":pan.workspace,"strip":pan.strip,"offset":1.0e9}),
    )
    .unwrap();
    let end = handle.snapshot("g", vec![]).desktop_pan.unwrap();
    assert!(
        (end.offset - end.max).abs() < 0.5,
        "clamped to the end: {end:?}"
    );

    let wrong_strip = try_dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"pan","generation":"g","workspace":pan.workspace,"strip":"1","offset":0.0}),
    );
    assert_eq!(wrong_strip.unwrap_err(), "Strip is no longer active");
}

/// Page deltaX of one Mac trackpad swipe: a small lead-in while the
/// fingers start, then the OS momentum decaying after they lift.
fn trackpad_swipe() -> Vec<f64> {
    let mut deltas = vec![1.0, 2.0, 4.0, 7.0, 11.0, 14.0, 16.0, 15.0];
    let mut v = 15.0_f64;
    for _ in 0..32 {
        v *= 0.9;
        deltas.push((v * 4.0).round() / 4.0);
    }
    deltas
}

thread_local! {
    /// The desktop's monotonic clock in a gesture test, once it has started.
    static CLOCK: Cell<Option<ui::time::Instant>> = const { Cell::new(None) };
}

/// One trackpad report, or one notch of a spun wheel, after the last.
const EVENT: Duration = Duration::from_millis(8);
/// One frame of a 60 Hz display after the last.
const FRAME: Duration = Duration::from_millis(16);

/// Run `step` with the desktop's clock moved on by `after`. Gestures and the
/// pauses between them last exactly as long as the test says, however fast
/// or loaded the machine running it is.
fn on_clock<T>(after: Duration, step: impl FnOnce() -> T) -> T {
    let at = CLOCK
        .get()
        .unwrap_or_else(ui::time::now)
        .checked_add(after)
        .unwrap();
    CLOCK.set(Some(at));
    tilo_clock::testing::with_monotonic_time(at, step)
}

/// Send one page wheel event as winit's web backend reports it: negated,
/// in physical pixels.
fn page_wheel(driver: &mut ui::headless::FrameHarness, delta_x: f64, delta_y: f64) {
    on_clock(EVENT, || {
        driver.send(&ui::winit::event::WindowEvent::MouseWheel {
            device_id: ui::winit::event::DeviceId::dummy(),
            delta: ui::winit::event::MouseScrollDelta::PixelDelta(
                ui::winit::dpi::PhysicalPosition::new(-delta_x, -delta_y),
            ),
            phase: ui::winit::event::TouchPhase::Moved,
        });
        driver.relayout();
    });
}

/// One notch of a mouse wheel spun sideways, `lines` to the left.
fn wheel_notch(driver: &mut ui::headless::FrameHarness, lines: f32) {
    on_clock(EVENT, || {
        driver.send(&ui::synthetic_input::mouse_wheel(lines, 0.0));
    });
}

/// Lay the desktop out at the clock's current reading.
fn relayout(driver: &mut ui::headless::FrameHarness) {
    on_clock(Duration::ZERO, || driver.relayout());
}

/// Frames until the wheel has paused longer than a swipe, so the next
/// input is a new gesture, and a last frame long after, once whatever the
/// gesture started has run its course.
fn settle(driver: &mut ui::headless::FrameHarness) {
    for _ in 0..30 {
        on_clock(FRAME, || driver.relayout());
    }
    on_clock(Duration::from_secs(5), || driver.relayout());
    on_clock(FRAME, || driver.relayout());
}

/// Frames until nothing on the desktop is moving.
fn settle_still(driver: &mut ui::headless::FrameHarness) {
    let settled = (0..1_000).any(|_| {
        on_clock(FRAME, || driver.relayout());
        !driver.frame_schedule().needs_animation_frame
    });
    assert!(
        settled,
        "the desktop is still moving after a thousand frames"
    );
}

fn pan_offset(handle: &DesktopHandle) -> f64 {
    handle.snapshot("g", vec![]).desktop_pan.unwrap().offset
}

/// Longer than the surface's swipe gap, so the next swipe is a new gesture.
fn pause() {
    on_clock(Duration::from_millis(260), || ());
}

#[test]
fn a_trackpad_swipe_pans_the_strip_under_the_fingers_through_its_momentum() {
    let (mut driver, handle, _) = mount_apps(&["A", "B", "C", "D", "E"]);
    let focus = handle.snapshot("g", vec![]).active;
    let max = handle.snapshot("g", vec![]).desktop_pan.unwrap().max;
    let swipe = trackpad_swipe();
    let total: f64 = swipe.iter().sum();
    assert!(
        total < max,
        "one swipe stays inside the strip: {total} of {max}"
    );

    let first_window_x = |driver: &ui::headless::FrameHarness| {
        find(&driver.inspection().root, "WindowGroup").unwrap().rect[0]
    };
    let drawn = first_window_x(&driver);
    let mut offsets = vec![pan_offset(&handle)];
    for &dx in &swipe {
        page_wheel(&mut driver, dx, 0.2);
        offsets.push(pan_offset(&handle));
    }
    assert!(
        (drawn - first_window_x(&driver) - total).abs() < 0.01,
        "the windows are drawn where the strip panned to"
    );
    for (step, (pair, dx)) in offsets.windows(2).zip(&swipe).enumerate() {
        assert!(
            (pair[1] - pair[0] - dx).abs() < 0.01,
            "event {step} moves the strip 1:1 ({dx} px): {offsets:?}"
        );
    }
    assert_eq!(
        handle.snapshot("g", vec![]).active,
        focus,
        "panning is not choosing a window"
    );

    // Fingers reversing inside one gesture pan straight back.
    let before = pan_offset(&handle);
    for _ in 0..5 {
        page_wheel(&mut driver, -10.0, 0.0);
    }
    assert!((pan_offset(&handle) - (before - 50.0)).abs() < 0.01);

    // A flick past the end keeps the strip at it for the whole gesture,
    // pulling against the rubber band (which the pan reports as the end),
    // and once let go the strip settles there.
    pause();
    for _ in 0..200 {
        page_wheel(&mut driver, 40.0, 0.0);
    }
    assert!(
        (pan_offset(&handle) - max).abs() < 0.5,
        "hard stop at the end"
    );
    for _ in 0..10 {
        page_wheel(&mut driver, 40.0, 0.0);
    }
    assert!((pan_offset(&handle) - max).abs() < 0.5);
    pause();
    settle_still(&mut driver);
    for &dx in &swipe {
        page_wheel(&mut driver, -dx, 0.0);
    }
    assert!(
        (pan_offset(&handle) - (max - total)).abs() < 0.01,
        "and swiping back follows the fingers again"
    );

    // Vertical scrolling is never the strip's.
    pause();
    let before = pan_offset(&handle);
    for _ in 0..10 {
        page_wheel(&mut driver, 1.0, 30.0);
    }
    assert!((pan_offset(&handle) - before).abs() < 0.01);
}

/// A browser page presents on animation frames, and a trackpad's wheel
/// events outpace them: each event pans the strip at once, and the next
/// frame moves the laid-out windows there without solving any size.
#[test]
fn wheel_pans_wait_for_the_next_frame_which_only_moves_the_windows() {
    let (mut driver, handle, _) = mount_apps(&["A", "B", "C", "D", "E"]);
    driver.set_coalesce_input_frames(true);
    let first_window_x = |driver: &ui::headless::FrameHarness| {
        find(&driver.inspection().root, "WindowGroup").unwrap().rect[0]
    };
    let drawn = first_window_x(&driver);
    let start = pan_offset(&handle);
    let frames = driver.relayout_frames();
    let ((), wheels) = tilo_resource::profiling::observe(|| {
        for _ in 0..3 {
            driver.send_host_event(&pixel_wheel(-10.0, 0.0));
        }
    });
    assert_eq!(driver.relayout_frames(), frames, "a wheel lays nothing out");
    assert_eq!(
        phase_calls(&wheels, "ui.host_tick"),
        0,
        "a strip pan is view-only: its tick is the frame's"
    );
    assert!(driver.frame_schedule().queued_work, "it asks for a frame");

    let (polled, frame) = tilo_resource::profiling::observe(|| driver.poll_frame().unwrap());
    assert!(polled);
    assert_eq!(
        phase_calls(&frame, "ui.host_tick"),
        1,
        "the frame ticks once"
    );
    assert_eq!(
        driver.relayout_frames(),
        frames + 1,
        "one frame for all three"
    );
    let stats = driver.last_relayout();
    assert!(
        !stats.rebuilt && !stats.measured,
        "a pan only re-applies positions: {stats:?}, asked by {:?}",
        driver.layout_measure_requesters()
    );
    assert!(
        (pan_offset(&handle) - start - 30.0).abs() < 0.01,
        "each wheel panned the strip 1:1"
    );
    assert!(
        (drawn - first_window_x(&driver) - 30.0).abs() < 0.01,
        "the windows are laid out where the strip panned to"
    );
    assert!(
        !driver.frame_schedule().queued_work,
        "the frame took the wheels' work"
    );
}

/// A page hosting the desktop shows the toolkit's tooltips as the installed
/// shell does: the pointer resting on a window bar's magnifier asks for a
/// frame when its dwell runs out, and that frame paints what the control
/// searches; pressing it takes the tooltip away.
#[test]
fn resting_on_the_window_bars_magnifier_shows_what_it_searches() {
    let _settled = ui::motion::testing::settled();
    let (mut driver, _handle, _) = mount_apps(&["A"]);
    driver.set_platform_conventions(ui::platform::PlatformConventions::new(
        ui::platform::Platform::Linux,
    ));
    driver.relayout();
    let magnifier = find(&driver.inspection().root, "WindowSearch")
        .expect("the window's bar shows its search control")
        .rect;
    let (x, y) = (
        (magnifier[0] + magnifier[2] / 2.0),
        (magnifier[1] + magnifier[3] / 2.0),
    );
    driver.send_host_event(&ui::synthetic_input::cursor_moved(x, y));
    let _ = driver.poll_frame();
    assert!(
        driver.drawn_tooltip().is_none(),
        "no tooltip before the pointer has rested"
    );
    let reveal = driver
        .frame_schedule()
        .next_wake_deadline
        .expect("the host is woken when the dwell runs out");
    let shown = tilo_clock::testing::with_monotonic_time(reveal, || {
        driver.poll_frame().expect("the frame lays out");
        driver
            .drawn_tooltip()
            .map(|(text, chip)| (text.to_owned(), chip))
    });
    let (text, chip) = shown.expect("the frame paints the tooltip");
    assert_eq!(text, "Search in this window (Ctrl+F)");
    assert!(chip.width() > 0.0 && chip.height() > 0.0);
    let read = find(&driver.inspection().root, "Tooltip")
        .and_then(|node| node.semantics.as_ref())
        .and_then(|node| node.name.clone());
    assert_eq!(
        read.as_deref(),
        Some("Search in this window (Ctrl+F)"),
        "a reader is told the tooltip too"
    );
    driver.send_host_event(&ui::synthetic_input::mouse_press());
    let _ = driver.poll_frame();
    assert!(
        driver.drawn_tooltip().is_none(),
        "acting on the control takes its tooltip away"
    );
}

/// The same hover on the live clock, with the tooltip's arrival animated,
/// driven as a page drives its frames: a frame when the schedule asks for
/// one (animation, queued work, a due deadline), otherwise asleep until its
/// deadline; widgets merely marked dirty bring none, as on the page. Its
/// frames come slower than the arrival lasts, as on a busy or
/// software-rendered GPU: the arrival's first frame draws it not yet
/// visible and the arrival ends before the next frame, which still has to
/// come and show the tooltip at rest. Past the dwell only the schedule's
/// demand brings a frame, so no unrelated deadline (the bar's clock) can
/// stand in for it.
#[test]
fn a_page_shows_the_tooltip_driving_frames_by_its_schedule() {
    let (mut driver, _handle, _) = mount_apps(&["A"]);
    driver.set_platform_conventions(ui::platform::PlatformConventions::new(
        ui::platform::Platform::Linux,
    ));
    driver.relayout();
    let magnifier = find(&driver.inspection().root, "WindowSearch")
        .expect("the window's bar shows its search control")
        .rect;
    let (x, y) = (
        magnifier[0] + magnifier[2] / 2.0,
        magnifier[1] + magnifier[3] / 2.0,
    );
    driver.send_host_event(&ui::synthetic_input::cursor_moved(x, y));
    let _ = driver.poll_frame();
    let revealed = ui::time::now() + ui::widget::tooltip::HOVER_DWELL;
    let end = ui::time::now() + Duration::from_secs(5);
    let mut frames = Vec::new();
    while ui::time::now() < end && driver.drawn_tooltip().is_none() {
        let schedule = driver.frame_schedule();
        let now = ui::time::now();
        let past_dwell = frames.iter().any(|&(at, _)| at >= revealed);
        let due = !past_dwell && schedule.next_wake_deadline.is_some_and(|at| at <= now);
        if schedule.needs_animation_frame || schedule.queued_work || due {
            frames.push((now, driver.poll_frame().expect("the frame lays out")));
            std::thread::sleep(Duration::from_millis(600));
        } else if let Some(at) = schedule.next_wake_deadline.filter(|_| !past_dwell) {
            std::thread::sleep(at.saturating_duration_since(now));
        } else {
            panic!(
                "nothing asks for a frame after {} frames: the tooltip stays unshown",
                frames.len()
            );
        }
    }
    assert_eq!(
        driver.drawn_tooltip().map(|(text, _)| text),
        Some("Search in this window (Ctrl+F)"),
        "after {} frames",
        frames.len()
    );
}

fn pixel_wheel(dx: f64, dy: f64) -> ui::winit::event::WindowEvent {
    ui::winit::event::WindowEvent::MouseWheel {
        device_id: ui::winit::event::DeviceId::dummy(),
        delta: ui::winit::event::MouseScrollDelta::PixelDelta(
            ui::winit::dpi::PhysicalPosition::new(dx, dy),
        ),
        phase: ui::winit::event::TouchPhase::Moved,
    }
}

fn phase_calls(rows: &[tilo_resource::profiling::Sample], phase: &str) -> usize {
    rows.iter()
        .find(|row| row.phase == phase)
        .map_or(0, |row| row.calls)
}

/// Only a wheel the strip pans with skips its own tick: one that reaches the
/// window under the pointer may dispatch there, so it ticks at once.
#[test]
fn a_wheel_the_strip_does_not_pan_with_ticks_at_once() {
    let (mut driver, handle, _) = mount_apps(&["A", "B", "C", "D", "E"]);
    driver.set_coalesce_input_frames(true);
    // Over the first window's content, not the wallpaper before it.
    driver.send_host_event(&ui::synthetic_input::cursor_moved(400.0, 400.0));
    driver.relayout();
    let start = pan_offset(&handle);
    let ((), rows) = tilo_resource::profiling::observe(|| {
        driver.send_host_event(&pixel_wheel(0.0, -10.0));
    });
    assert!(
        (pan_offset(&handle) - start).abs() < 0.01,
        "a vertical wheel pans nothing"
    );
    assert_eq!(phase_calls(&rows, "ui.host_tick"), 1, "{rows:?}");
}

#[test]
fn a_trackpad_swipe_the_strip_cannot_follow_is_the_apps() {
    let (mut driver, handle, _) = mount_apps(&["A", "B", "C", "D", "E"]);
    assert!(pan_offset(&handle).abs() < 0.5, "at home");
    // Backwards from home the strip cannot move, so the gesture is the
    // app's, even once it reverses toward later windows.
    for _ in 0..10 {
        page_wheel(&mut driver, -12.0, 0.0);
    }
    for _ in 0..10 {
        page_wheel(&mut driver, 12.0, 0.0);
    }
    assert!(pan_offset(&handle).abs() < 0.5);

    let (mut driver, handle, _) = mount_apps(&["A"]);
    let max = handle.snapshot("g", vec![]).desktop_pan.unwrap().max;
    assert!(max < 0.5, "one window fits: {max}");
    for &dx in &trackpad_swipe() {
        page_wheel(&mut driver, dx, 0.0);
    }
    assert!(
        pan_offset(&handle).abs() < 0.5,
        "a lone window's app scrolls sideways"
    );
}

#[test]
fn scroll_moves_the_focused_windows_primary_region() {
    let (mut driver, handle, _) = mount_apps(&["Long", "B"]);
    let scroll = handle
        .snapshot("g", vec![])
        .scroll
        .expect("the focused app exposes its primary region");
    assert!(scroll.max > 0.0 && scroll.offset.abs() < 0.5, "{scroll:?}");
    for (action, expect) in [
        (serde_json::json!({"kind":"line","value":1}), 48.0),
        (serde_json::json!({"kind":"end"}), scroll.max),
        (serde_json::json!({"kind":"absolute","value":100.0}), 100.0),
        (serde_json::json!({"kind":"start"}), 0.0),
    ] {
        try_dispatch(
            &mut driver,
            &handle,
            serde_json::json!({"kind":"scroll","generation":"g","region":scroll.region,"action":action}),
        )
        .unwrap();
        let now = handle.snapshot("g", vec![]).scroll.unwrap();
        assert!((now.offset - expect).abs() < 0.5, "{action}: {now:?}");
    }
    let saved = handle.revision();
    try_dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"scroll","generation":"g","region":scroll.region,"action":{"kind":"page","value":1}}),
    )
    .unwrap();
    let paged = handle.snapshot("g", vec![]).scroll.unwrap();
    assert!(paged.offset > 0.0, "{paged:?}");
    assert_eq!(handle.revision(), saved, "a scroll is not a tree change");

    let other = handle.snapshot("g", vec![]).entries[1].route.clone();
    dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"select","generation":"g","route":other}),
    );
    assert!(
        handle.snapshot("g", vec![]).scroll.is_none(),
        "a notice has no scroll region"
    );
    let stale = try_dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"scroll","generation":"g","region":scroll.region,"action":{"kind":"start"}}),
    );
    assert_eq!(stale.unwrap_err(), "Scroll target is no longer active");
}

#[test]
fn history_moves_the_windows_own_navigation_stack() {
    let (mut driver, handle, routes) = mount_apps(&["Pages"]);
    let snapshot = handle.snapshot("g", vec![]);
    let route = snapshot.active.clone().unwrap();
    let history = snapshot.history.unwrap();
    assert!(!history.can_back && !history.can_forward, "{history:?}");
    let refused = try_dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"history","generation":"g","route":route,"target":"back"}),
    );
    assert_eq!(refused.unwrap_err(), "Nothing to go back to in this window");

    let pages = routes.borrow().clone().unwrap();
    assert!(pages.push("two").unwrap());
    driver.relayout();
    let history = handle.snapshot("g", vec![]).history.unwrap();
    assert!(history.can_back && !history.can_forward, "{history:?}");

    try_dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"history","generation":"g","route":route,"target":"back"}),
    )
    .unwrap();
    assert_eq!(pages.current(), "one");
    let history = handle.snapshot("g", vec![]).history.unwrap();
    assert!(!history.can_back && history.can_forward, "{history:?}");

    try_dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"history","generation":"g","route":route,"target":"forward"}),
    )
    .unwrap();
    assert_eq!(pages.current(), "two");

    let unknown = try_dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"history","generation":"g","route":route,"target":"sideways"}),
    );
    assert_eq!(unknown.unwrap_err(), "Unknown history target");
}

#[test]
fn an_activity_view_without_its_history_says_why_not() {
    let (mut driver, handle, _) = mount_apps(&["A"]);
    driver.relayout();
    let workspace = handle.snapshot("g", vec![]).active_workspace;

    for (view, reason) in [
        ("catch-up", "Catch up needs retained record history"),
        ("explore", "Explore needs retained record history"),
        ("replay", "Replay needs retained desktop history"),
    ] {
        let refused = try_dispatch(
            &mut driver,
            &handle,
            serde_json::json!({"kind":"workspace-activity","generation":"g","workspace":workspace,"view":view}),
        );
        assert!(refused.unwrap_err().starts_with(reason), "{view}");
    }
    // Tasks are an app, not a view of the workspace's activity.
    assert!(
        serde_json::from_value::<Command>(serde_json::json!({
            "kind": "workspace-activity",
            "generation": "g",
            "workspace": workspace,
            "view": "tasks",
        }))
        .is_err()
    );
}

#[test]
fn a_tab_the_host_asks_to_rebuild_gets_its_content_again() {
    let (mut driver, handle, built) = mount();
    driver.relayout();
    assert_eq!(built.get(), 2);
    let notes = handle.read(|tree| tree.workspaces[0].strips[0].window_groups[0].tabs[0].id);
    handle.rebuild(notes);
    driver.relayout();
    assert_eq!(built.get(), 3, "only the named tab is built again");
    driver.relayout();
    assert_eq!(built.get(), 3, "and only once");
}

#[test]
fn a_sideways_wheel_spin_pans_the_strip_and_focus_follows_to_where_it_stopped() {
    fn groups(node: &ui::headless::InspectionNode, out: &mut Vec<[f64; 4]>) {
        if node.widget == "WindowGroup" {
            out.push(node.rect);
        }
        for child in &node.children {
            groups(child, out);
        }
    }
    let mut minter = IdMinter::new(7);
    let tabs = (0..4)
        .map(|i| tab(&mut minter, &format!("App{i}")))
        .collect();
    let tree = seed_tree(&mut minter, "Desk", tabs);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Notices(Rc::new(Cell::new(0)))));
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    driver.relayout();
    let entries = handle.snapshot("g", vec![]).entries;
    let first = handle.snapshot("g", vec![]).active.unwrap();
    let shown = |driver: &ui::headless::FrameHarness, index: usize| {
        let mut rects = Vec::new();
        groups(&driver.inspection().root, &mut rects);
        rects.sort_by(|a, b| a[0].total_cmp(&b[0]));
        let [x, _, width, _] = rects[index];
        x.max(0.0) < (x + width).min(1200.0) - width / 2.0
    };
    on_clock(Duration::ZERO, || {
        driver.send(&ui::synthetic_input::cursor_moved(600.0, 400.0));
    });
    // A sideways wheel spun to the right as Firefox reports it: six lines a
    // notch, which winit reports with negative x.
    let before = pan_offset(&handle);
    for _ in 0..4 {
        wheel_notch(&mut driver, -6.0);
    }
    relayout(&mut driver);
    assert!(
        (pan_offset(&handle) - before - 960.0).abs() < 0.5,
        "four notches pan the strip four notches' travel"
    );
    assert_eq!(
        handle.snapshot("g", vec![]).active.unwrap(),
        first,
        "the strip moves under focus while the wheel spins"
    );
    let spun = pan_offset(&handle);
    settle(&mut driver);
    let followed = handle.snapshot("g", vec![]).active.unwrap();
    let index = entries.iter().position(|e| e.route == followed).unwrap();
    assert!(index > 0, "focus follows the strip once the wheel stops");
    assert!(
        shown(&driver, index),
        "to a window mostly in view, window {index}"
    );
    assert!(
        (pan_offset(&handle) - spun).abs() < 0.5,
        "and the strip stays where the wheel left it"
    );

    // A long spin stops at the strip's end and keeps the rest of the spin.
    for _ in 0..40 {
        wheel_notch(&mut driver, -6.0);
    }
    relayout(&mut driver);
    let pan = handle.snapshot("g", vec![]).desktop_pan.unwrap();
    assert!((pan.offset - pan.max).abs() < 0.5, "hard stop at the end");
    settle(&mut driver);
    assert_eq!(
        handle.snapshot("g", vec![]).active.unwrap(),
        entries[3].route,
        "focus follows to the last window"
    );
    assert!(shown(&driver, 3));
}

#[test]
fn a_slow_spin_pans_exactly_its_notches_and_the_strip_stays_where_it_left_it() {
    let mut minter = IdMinter::new(7);
    let tabs = (0..4)
        .map(|i| tab(&mut minter, &format!("App{i}")))
        .collect();
    let tree = seed_tree(&mut minter, "Desk", tabs);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Notices(Rc::new(Cell::new(0)))));
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    driver.relayout();
    let last = handle.snapshot("g", vec![]).entries[3].route.clone();
    on_clock(Duration::ZERO, || {
        driver.send(&ui::synthetic_input::cursor_moved(600.0, 400.0));
    });
    for _ in 0..40 {
        wheel_notch(&mut driver, -6.0);
    }
    relayout(&mut driver);
    settle(&mut driver);

    // A spin whose notches arrive slowly, one or two at a time with pauses
    // between them, as a busy page gets them: each pause ends a gesture and
    // focus follows, but the strip still pans exactly the notches' travel.
    let from = pan_offset(&handle);
    for lines in [1.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 1.0] {
        wheel_notch(&mut driver, lines);
        relayout(&mut driver);
        settle(&mut driver);
    }
    assert!(
        (pan_offset(&handle) - (from - 20.0 * 40.0)).abs() < 0.5,
        "twenty slow notches pan twenty lines: {} from {from}",
        pan_offset(&handle)
    );

    // Where the spin left the strip, windows are cut at both edges. A later
    // change to the strip's geometry, here the last window a pixel wider,
    // keeps it there rather than re-framing it onto a window's edge.
    let held = pan_offset(&handle);
    on_clock(Duration::ZERO, || {
        dispatch(
            &mut driver,
            &handle,
            serde_json::json!({"kind":"width","generation":"g","route":last,"permille":668}),
        );
    });
    settle(&mut driver);
    assert!(
        (pan_offset(&handle) - held).abs() < 0.5,
        "a window's width changing keeps the strip where the wheel left it: {} from {held}",
        pan_offset(&handle)
    );
}

fn find<'a>(
    node: &'a ui::headless::InspectionNode,
    widget: &str,
) -> Option<&'a ui::headless::InspectionNode> {
    if node.widget == widget {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, widget))
}

fn labels(snapshot: &tilo_desktop_surface::navigation::Snapshot) -> Vec<String> {
    snapshot.entries.iter().map(|e| e.label.clone()).collect()
}

#[test]
fn the_search_shortcut_then_an_app_name_and_enter_opens_the_app() {
    let (mut driver, handle, _) = mount();
    driver.set_platform_conventions(ui::platform::PlatformConventions::new(
        ui::platform::Platform::Linux,
    ));
    driver.set_modifiers(&["ctrl".into()]);
    driver.key("k");
    driver.set_modifiers(&[]);
    assert!(
        handle.search_open(),
        "Ctrl+K opens search where Ctrl is the command key"
    );
    assert!(find(&driver.inspection().root, "CommandPalette").is_some());
    driver.type_text("sheets");
    driver.key("Enter");
    driver.relayout();
    assert!(!handle.search_open(), "choosing a row closes search");
    let snapshot = handle.snapshot("g", vec![]);
    assert!(
        labels(&snapshot).contains(&"sheets".into()),
        "{:?}",
        labels(&snapshot)
    );
    let active = snapshot.active.unwrap();
    assert_eq!(
        snapshot
            .entries
            .iter()
            .find(|e| e.route == active)
            .unwrap()
            .label,
        "sheets",
        "the launched app is focused"
    );
    assert!(find(&driver.inspection().root, "CommandPalette").is_none());
}

#[test]
fn search_from_navigation_focuses_an_open_window_and_escape_dismisses() {
    let (mut driver, handle, _) = mount();
    dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"global-search","generation":"g"}),
    );
    assert!(handle.search_open());
    driver.key("Escape");
    driver.relayout();
    assert!(!handle.search_open(), "Escape dismisses");
    assert_eq!(
        handle.snapshot("g", vec![]).entries.len(),
        2,
        "nothing opened"
    );

    dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"commands","generation":"g","query":"scratch"}),
    );
    assert!(handle.search_open());
    driver.key("Enter");
    driver.relayout();
    let snapshot = handle.snapshot("g", vec![]);
    assert_eq!(
        snapshot.entries.len(),
        2,
        "a window hit opens no new window"
    );
    let active = snapshot.active.unwrap();
    assert_eq!(
        snapshot
            .entries
            .iter()
            .find(|e| e.route == active)
            .unwrap()
            .label,
        "Scratch"
    );
}

/// The open palette's result rows.
fn result_rows(driver: &ui::headless::FrameHarness) -> Vec<ui::a11y::AccessNode> {
    let palette = find(&driver.inspection().root, "CommandPalette")
        .unwrap()
        .semantics
        .clone()
        .unwrap();
    palette.children.get(1).unwrap().children.clone()
}

/// Choose the app row named `name` in open search.
fn choose_app(driver: &mut ui::headless::FrameHarness, name: &str) {
    let row = result_rows(driver)
        .into_iter()
        .find(|row| row.name.as_deref() == Some(name))
        .unwrap();
    driver
        .perform_semantic_named(
            "command_palette_input",
            row.action_target.as_deref(),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    driver.relayout();
}

#[test]
fn an_app_chosen_from_search_while_its_window_is_open_is_focused_not_opened_twice() {
    let (mut driver, handle, _) = mount();
    handle.open_search(Some("sheets".into()));
    driver.relayout();
    choose_app(&mut driver, "Sheets");
    assert!(handle.focus_app("so.tilo.notes"));
    driver.relayout();
    handle.open_search(Some("sheets".into()));
    driver.relayout();
    choose_app(&mut driver, "Sheets");
    let snapshot = handle.snapshot("g", vec![]);
    assert_eq!(
        labels(&snapshot),
        ["Notes", "sheets", "Scratch"],
        "no second Sheets window"
    );
    let active = snapshot.active.unwrap();
    assert_eq!(
        snapshot
            .entries
            .iter()
            .find(|e| e.route == active)
            .unwrap()
            .label,
        "sheets",
        "the open Sheets window is focused"
    );
}

#[test]
fn windows_titled_alike_say_which_one_each_is() {
    let (mut driver, handle, _) = mount_with(&["Chat", "Chat", "Notes"]);
    handle.open_search(Some("chat".into()));
    driver.relayout();
    let chats: Vec<String> = result_rows(&driver)
        .into_iter()
        .filter(|row| row.name.as_deref() == Some("Chat"))
        .filter_map(|row| row.description)
        // Without the bidirectional isolation marks around placeables.
        .map(|facts| facts.replace(['\u{2068}', '\u{2069}'], ""))
        .collect();
    assert_eq!(chats.len(), 2, "{chats:?}");
    let (first, second) = (chats.first().unwrap(), chats.get(1).unwrap());
    assert!(
        first.ends_with("1 of 2") && second.ends_with("2 of 2"),
        "{chats:?}"
    );
    assert!(
        chats.iter().all(|place| !place.starts_with("Window")),
        "the Windows header already says what a row is: {chats:?}"
    );
}

#[test]
fn the_search_shortcut_closes_open_search_and_a_host_can_toggle_it() {
    let (mut driver, handle, _) = mount();
    handle.toggle_search();
    driver.relayout();
    assert!(handle.search_open());
    driver.set_modifiers(&["super".into()]);
    driver.key("k");
    driver.set_modifiers(&[]);
    driver.relayout();
    assert!(!handle.search_open());
    handle.toggle_search();
    driver.relayout();
    assert!(handle.search_open());
    handle.toggle_search();
    driver.relayout();
    assert!(!handle.search_open());
}

#[test]
fn a_host_command_from_search_runs_against_the_focused_window() {
    let (mut driver, handle, _) = mount();
    handle.open_search(Some("close window".into()));
    driver.relayout();
    driver.key("Enter");
    driver.relayout();
    assert_eq!(
        handle.snapshot("g", vec![]).entries.len(),
        1,
        "the focused window closed"
    );
}

#[test]
fn assistive_technology_and_agents_search_through_the_palette_semantics() {
    use ui::a11y::Action;
    let (mut driver, handle, _) = mount();
    handle.toggle_search();
    driver.relayout();
    let palette = find(&driver.inspection().root, "CommandPalette")
        .expect("the open palette is in the semantic tree")
        .semantics
        .clone()
        .unwrap();
    assert!(
        palette
            .children
            .iter()
            .any(|c| c.action_target.as_deref() == Some("command-palette:query"))
    );
    driver
        .perform_semantic_named(
            "command_palette_input",
            Some("command-palette:query"),
            &Action::SetValue("writer".into()),
        )
        .unwrap();
    driver.relayout();
    let palette = find(&driver.inspection().root, "CommandPalette")
        .unwrap()
        .semantics
        .clone()
        .unwrap();
    let results = &palette.children[1].children;
    let first = results.first().expect("a result for the typed query");
    assert_eq!(first.name.as_deref(), Some("Writer"));
    driver
        .perform_semantic_named(
            "command_palette_input",
            first.action_target.as_deref(),
            &Action::Activate,
        )
        .unwrap();
    driver.relayout();
    assert!(!handle.search_open());
    assert!(labels(&handle.snapshot("g", vec![])).contains(&"writer".into()));
}

#[test]
fn search_opens_an_app_the_node_runs_for_this_desktop() {
    use ui::a11y::Action;
    let (mut driver, handle, _) = mount();
    handle.toggle_search();
    driver.relayout();
    driver
        .perform_semantic_named(
            "command_palette_input",
            Some("command-palette:query"),
            &Action::SetValue("terminal".into()),
        )
        .unwrap();
    driver.relayout();
    let palette = find(&driver.inspection().root, "CommandPalette")
        .unwrap()
        .semantics
        .clone()
        .unwrap();
    let first = palette.children[1]
        .children
        .first()
        .expect("the node's app is listed");
    assert_eq!(first.name.as_deref(), Some("Terminal"), "it opens");
    assert!(
        !format!("{first:?}").contains(" only"),
        "the row offers it: {first:?}"
    );
    driver
        .perform_semantic_named(
            "command_palette_input",
            first.action_target.as_deref(),
            &Action::Activate,
        )
        .unwrap();
    driver.relayout();
    assert_eq!(
        NODE_LAUNCHES.with(|launches| launches.borrow().clone()),
        vec![format!("{NODE_APP}app.so.tilo.terminal")],
        "the desktop asked the node to open it"
    );
}

/// Each built tab and the window a panic hook would name while building it.
type ScopedTabs = Rc<std::cell::RefCell<Vec<(u64, Option<u64>)>>>;

/// Records the window a panic hook would name while each tab is built.
struct Scoped(ScopedTabs);

impl TileSource for Scoped {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        self.0
            .borrow_mut()
            .push((tab.id, ui::widget::panic_scope::current()));
        TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}

#[test]
fn a_window_is_built_under_its_tab_for_a_panic_hook() {
    let mut minter = IdMinter::new(7);
    let tabs = vec![tab(&mut minter, "Notes"), tab(&mut minter, "Maps")];
    let tree = seed_tree(&mut minter, "Desk", tabs);
    let built = Rc::new(std::cell::RefCell::new(Vec::new()));
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Scoped(Rc::clone(&built))));
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    driver.relayout();
    let built = built.borrow();
    assert_eq!(built.len(), 2, "{built:?}");
    for (tab, running) in built.iter() {
        assert_eq!(*running, Some(*tab));
    }
    assert_eq!(ui::widget::panic_scope::current(), None);
}

#[test]
fn the_focused_windows_caret_is_published_even_when_keyboard_focus_is_elsewhere() {
    let (mut driver, handle, _) = mount_apps(&["Caret A", "Caret B"]);
    driver.relayout();
    let caret = |title: &str| {
        (
            ui::presence::PresenceKey::Tab { id: title.into() },
            ui::collaboration::PresencePayload::Caret { pos: 7 },
        )
    };
    let routes: Vec<Route> = handle
        .snapshot("g", vec![])
        .entries
        .into_iter()
        .map(|entry| entry.route)
        .collect();
    assert_eq!(handle.snapshot("g", vec![]).active.unwrap(), routes[0]);
    // Nothing holds keyboard focus: the focused window's editor caret.
    assert_eq!(handle.published_selection(None), Some(caret("Caret A")));
    // Keyboard focus left behind in the other window loses to it.
    assert_eq!(
        handle.published_selection(Some(caret("Caret B"))),
        Some(caret("Caret A"))
    );
    dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"select","generation":"g","route":routes[1]}),
    );
    assert_eq!(handle.published_selection(None), Some(caret("Caret B")));
    assert_eq!(
        handle.published_selection(Some(caret("Caret B"))),
        Some(caret("Caret B")),
        "keyboard focus inside the focused window is published as is"
    );
}

#[test]
fn published_selection_prefers_keyboard_focus_inside_the_window() {
    use tilo_desktop_surface::published_selection;
    use ui::collaboration::PresencePayload;
    let key = |id: &str| ui::presence::PresenceKey::Tab { id: id.into() };
    let window = vec![(key("writer"), PresencePayload::Caret { pos: 40 })];
    let stray = Some((key("elsewhere"), PresencePayload::Caret { pos: 0 }));
    assert_eq!(published_selection(stray, &window), window.first().cloned());
    let own = Some((key("writer"), PresencePayload::Caret { pos: 41 }));
    assert_eq!(published_selection(own.clone(), &window), own);
    assert_eq!(published_selection(own.clone(), &[]), own);
    assert_eq!(published_selection(None, &[]), None);
}

#[test]
fn a_windows_new_tab_control_asks_which_app_and_opens_it_there() {
    // Mounted as its own type, so the test can find the control to press.
    // A window of one tab shows it as its title and offers no new tab, so
    // the focused window holds two.
    let mut minter = IdMinter::new(7);
    let tabs = vec![tab(&mut minter, "Notes")];
    let mut tree = seed_tree(&mut minter, "Desk", tabs);
    let scratch = tab(&mut minter, "Scratch");
    tree.workspaces[0].strips[0].window_groups[0]
        .tabs
        .push(scratch);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Notices(Rc::new(Cell::new(0)))));
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1200, 800),
        ui::Theme::dark(),
    );
    driver.set_caller_palette_enabled(false);
    driver.relayout();
    let before = handle.snapshot("g", vec![]);
    let group = before.active.clone().unwrap().group;
    let windows = |snapshot: &tilo_desktop_surface::navigation::Snapshot| {
        snapshot
            .entries
            .iter()
            .map(|e| e.route.group.clone())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let plus = driver
        .root::<DesktopSurface>()
        .new_tab_rect(group.parse().unwrap())
        .expect("the focused window offers a new tab");
    driver.click_at(plus.center().x, plus.center().y);
    driver.relayout();
    assert!(handle.search_open(), "the control opens search");
    assert_eq!(
        labels(&handle.snapshot("g", vec![])),
        labels(&before),
        "nothing opens before an application is chosen"
    );
    driver.type_text("sheets");
    driver.key("Enter");
    driver.relayout();
    assert!(!handle.search_open());
    let after = handle.snapshot("g", vec![]);
    let active = after.active.clone().unwrap();
    let opened = after.entries.iter().find(|e| e.route == active).unwrap();
    assert_eq!(opened.label, "sheets", "the chosen app is focused");
    assert_eq!(active.group, group, "it opens as a tab of that window");
    assert_eq!(windows(&after), windows(&before), "no window is added");

    // Search opened otherwise opens an application as its own window.
    handle.toggle_search();
    driver.relayout();
    driver.type_text("writer");
    driver.key("Enter");
    driver.relayout();
    let last = handle.snapshot("g", vec![]);
    assert_ne!(last.active.as_ref().unwrap().group, group);
    assert_eq!(windows(&last).len(), windows(&before).len() + 1);
}

/// Three windows each 70% of the strip: the second runs off the right edge.
fn mount_wide() -> (ui::headless::FrameHarness, DesktopHandle) {
    let mut minter = IdMinter::new(7);
    let tabs = (0..3)
        .map(|i| tab(&mut minter, &format!("Wide{i}")))
        .collect();
    let mut tree = seed_tree(&mut minter, "Desk", tabs);
    let strip = tree
        .workspaces
        .first_mut()
        .and_then(|workspace| workspace.strips.first_mut())
        .unwrap();
    for group in &mut strip.window_groups {
        group.width = WindowWidth::Fraction(700);
    }
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Notices(Rc::new(Cell::new(0)))));
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    for _ in 0..3 {
        driver.relayout();
    }
    (driver, handle)
}

/// Where tab `index` of the shown strip is drawn.
fn shown_rect(handle: &DesktopHandle, index: usize) -> ui::kurbo::Rect {
    let mut shown = handle.shown_tabs();
    shown.sort_by(|a, b| a.1.x0.total_cmp(&b.1.x0));
    shown.get(index).unwrap().1
}

#[test]
fn a_click_in_a_window_partly_off_screen_brings_it_into_view() {
    let (mut driver, handle) = mount_wide();
    let second = shown_rect(&handle, 1);
    assert!(
        second.x0 < 1200.0 && second.x1 > 1200.0,
        "the second window runs off the right edge: {second:?}"
    );
    driver.click_at(f64::midpoint(second.x0, 1200.0), second.center().y);
    settle_still(&mut driver);
    let second = shown_rect(&handle, 1);
    assert!(
        second.x0 >= -0.5 && second.x1 <= 1200.5,
        "the clicked window is wholly in view: {second:?}"
    );
    let snapshot = handle.snapshot("g", vec![]);
    assert_eq!(
        snapshot.active.as_ref(),
        Some(&snapshot.entries[1].route),
        "and focused"
    );
}

/// Every accessible name under the first node of `widget`, synthetic
/// children included.
fn names_under(root: &ui::headless::InspectionNode, widget: &str) -> Vec<String> {
    fn semantic(node: &ui::a11y::AccessNode, out: &mut Vec<String>) {
        out.extend(node.name.clone());
        for child in &node.children {
            semantic(child, out);
        }
    }
    fn walk(node: &ui::headless::InspectionNode, out: &mut Vec<String>) {
        if let Some(semantics) = &node.semantics {
            semantic(semantics, out);
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    if let Some(node) = find(root, widget) {
        walk(node, &mut out);
    }
    out
}

/// A maximized window keeps its bar, so a reader is told of it as of a
/// restored one: its title and controls, and its search.
#[test]
fn a_maximized_windows_bar_and_search_are_in_the_accessibility_tree() {
    let (mut driver, handle, _) = mount_apps(&["A"]);
    let route = handle.snapshot("g", vec![]).entries[0].route.clone();
    settle_still(&mut driver);
    let restored = driver.inspection();
    let search = names_under(&restored.root, "WindowSearch");
    assert!(!search.is_empty(), "a restored window's search is read");
    dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"maximize","generation":"g","route":route,"maximized":true}),
    );
    settle_still(&mut driver);
    let inspection = driver.inspection();
    let bar = names_under(&inspection.root, "TabBar");
    for name in ["A", "Close A", "Restore", "Close"] {
        assert!(
            bar.iter().any(|read| read == name),
            "the maximized window's bar reads {name:?}: {bar:?}"
        );
    }
    assert_eq!(
        names_under(&inspection.root, "WindowSearch"),
        search,
        "the maximized window's search is read as when restored"
    );
}

#[test]
fn a_window_restored_from_maximized_is_in_view() {
    let (mut driver, handle) = mount_wide();
    let snapshot = handle.snapshot("g", vec![]);
    let route = snapshot.entries[1].route.clone();
    let pan = snapshot.desktop_pan.unwrap();
    dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"maximize","generation":"g","route":route,"maximized":true}),
    );
    settle_still(&mut driver);
    // The strip is back home while the window fills the display, which
    // leaves the restored window partly off screen unless it is revealed.
    // (Where the strip already stays home under a maximized window, the
    // pan has nothing left to do.)
    let _ = try_dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"pan","generation":"g","workspace":pan.workspace,"strip":pan.strip,"offset":0.0}),
    );
    dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"maximize","generation":"g","route":route,"maximized":false}),
    );
    settle_still(&mut driver);
    let restored = shown_rect(&handle, 1);
    assert!(
        restored.x0 >= -0.5 && restored.x1 <= 1200.5,
        "the restored window's controls are on screen: {restored:?}"
    );
}

/// One member's desk in a room whose layout is personal: a tree of its own
/// (its own ids, its own strip) showing a Notes and a Chat window, on a
/// screen `width` wide.
fn personal_desk(seed: u16, width: u32) -> (ui::headless::FrameHarness, DesktopHandle) {
    let mut minter = IdMinter::new(seed);
    let tabs = ["Notes", "Chat"]
        .iter()
        .map(|title| tab(&mut minter, title))
        .collect();
    let tree = seed_tree(&mut minter, "Main", tabs);
    let (surface, handle) =
        DesktopSurface::new(tree, seed, Box::new(Notices(Rc::new(Cell::new(0)))));
    handle.set_viewport_width(f64::from(width));
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (width, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    for _ in 0..3 {
        driver.relayout();
    }
    (driver, handle)
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "surface coordinates are a few thousand logical pixels"
)]
fn rect_f32(rect: ui::kurbo::Rect) -> [f32; 4] {
    [
        rect.x0 as f32,
        rect.y0 as f32,
        rect.x1 as f32,
        rect.y1 as f32,
    ]
}

/// What a desk shows that a member's pointer can be relative to, as the
/// browser runtime lays it out: each shown window by the record it shows
/// (here, its app's room record) and the shown strip.
fn pointer_layout(handle: &DesktopHandle) -> tilo_relay_protocol::live::PointerLayout {
    let shown = handle.shown_tabs();
    let windows = handle.read(|tree| {
        shown
            .iter()
            .filter_map(|(id, rect)| {
                let tab = tree
                    .workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.strips)
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .find(|tab| tab.id == *id)?;
                let TileContent::LocalApp { app_id, .. } = &tab.tile else {
                    return None;
                };
                let record = tilo_relay_protocol::live::LiveRecord {
                    app: app_id.clone(),
                    record: String::new(),
                };
                Some((record, rect_f32(*rect)))
            })
            .collect()
    });
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a strip's pan is a few thousand logical pixels"
    )]
    let strip = handle
        .shown_strip()
        .map(|(id, area, pan)| tilo_relay_protocol::live::ShownStrip {
            id: id.0,
            area: rect_f32(area),
            scroll_x: pan as f32,
        });
    tilo_relay_protocol::live::PointerLayout { windows, strip }
}

fn close_to(a: [f32; 2], b: [f32; 2]) -> bool {
    (a[0] - b[0]).abs() < 0.01 && (a[1] - b[1]).abs() < 0.01
}

/// Two visitors of a room whose layout is personal, on screens of other
/// widths: their strips are their own, with ids of their own, so a pointer
/// over one's empty desktop says where it is beside the nearest window, and
/// lands as far beside that window on the other's desk. Said in the strip's
/// coordinates, it never landed anywhere.
#[test]
fn a_pointer_over_the_empty_desktop_crosses_personal_layouts_of_other_widths() {
    use tilo_relay_protocol::live::LivePointer;
    let (_wide_driver, wide) = personal_desk(7, 1440);
    let (_narrow_driver, narrow) = personal_desk(9, 1024);
    let (wide_layout, narrow_layout) = (pointer_layout(&wide), pointer_layout(&narrow));
    let (wide_strip, narrow_strip) = (wide_layout.strip.unwrap(), narrow_layout.strip.unwrap());
    assert_ne!(
        wide_strip.id, narrow_strip.id,
        "each desk's strip is its own"
    );
    let notes = |layout: &tilo_relay_protocol::live::PointerLayout| {
        layout
            .windows
            .iter()
            .find(|(record, _)| record.app == "so.tilo.notes")
            .map(|(_, rect)| *rect)
            .expect("the desk shows its notes")
    };
    let (wide_notes, narrow_notes) = (notes(&wide_layout), notes(&narrow_layout));
    assert!(
        wide_notes[2] - wide_notes[0] > narrow_notes[2] - narrow_notes[0],
        "the notes window is wider on the wider screen: {wide_notes:?} {narrow_notes:?}"
    );

    // In the empty desktop left of the notes window, a third of the way down.
    let gutter = (wide_notes[0] - wide_strip.area[0]) / 2.0;
    assert!(
        gutter > 1.0,
        "the strip keeps a gutter before its first window"
    );
    let third = wide_notes[1] + (wide_notes[3] - wide_notes[1]) / 3.0;
    let pointer = wide_layout
        .anchor([wide_notes[0] - gutter, third])
        .expect("the empty desktop names a window beside it");
    assert!(
        matches!(&pointer, LivePointer::Beside { record, .. } if record.app == "so.tilo.notes"),
        "beside the notes window: {pointer:?}"
    );
    let landed = narrow_layout
        .place(&pointer)
        .expect("the other desk shows the notes");
    let expected = [
        narrow_notes[0] - gutter,
        narrow_notes[1] + (narrow_notes[3] - narrow_notes[1]) / 3.0,
    ];
    assert!(
        close_to(landed, expected),
        "as far left of the notes, a third of the way down: {landed:?} {expected:?}"
    );

    // Over the window itself, the same spot of it.
    let inside = wide_layout
        .anchor([wide_notes[0] + 40.0, wide_notes[1] + 30.0])
        .expect("over the notes");
    let landed = narrow_layout.place(&inside).expect("the notes are shown");
    assert!(close_to(
        landed,
        [narrow_notes[0] + 40.0, narrow_notes[1] + 30.0]
    ));

    // What a strip of one's own says reaches nobody else.
    let on_strip = LivePointer::Strip {
        strip: wide_strip.id,
        at: [10.0, 10.0],
    };
    assert_eq!(narrow_layout.place(&on_strip), None);
}

/// On a phone the search palette fits the screen: centred, within its
/// edges, and as wide as the screen allows rather than its own width.
#[test]
fn search_fits_a_phone_screen() {
    let mut minter = IdMinter::new(7);
    let tabs = vec![tab(&mut minter, "Notes")];
    let tree = seed_tree(&mut minter, "Desk", tabs);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Notices(Rc::new(Cell::new(0)))));
    handle.set_viewport_width(390.0);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (390, 844),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    driver.set_caller_palette_enabled(false);
    driver.relayout();
    handle.toggle_search();
    driver.relayout();
    driver.relayout();
    let [x, _, width, _] = find(&driver.inspection().root, "CommandPalette")
        .expect("search is open")
        .rect;
    assert!(
        x >= 0.0 && x + width <= 390.0,
        "the palette at {x}..{} runs off a 390 px screen",
        x + width
    );
    assert!(
        ((x + width / 2.0) - 195.0).abs() < 1.0,
        "and is centred: {x}, {width}"
    );
}

/// A wheel goes to the window under the pointer: a window beside it whose
/// app takes any wheel it is handed does not get it first, so the scroll
/// view under the pointer scrolls. The browser desktop sends the wheel as
/// it does, coalesced into its next frame.
#[test]
fn a_wheel_scrolls_the_window_under_the_pointer_not_the_one_before_it() {
    let (mut driver, handle, _) = mount_apps(&["Greedy", "Long"]);
    settle(&mut driver);
    driver.set_coalesce_input_frames(true);
    let tab = handle.read(|tree| tree.workspaces[0].strips[0].window_groups[1].active_tab);
    let rect = handle
        .shown_tabs()
        .iter()
        .find(|(id, _)| *id == tab)
        .map(|(_, rect)| *rect)
        .expect("the scrolling window is shown");
    let at = rect.center();
    driver.send(&ui::winit::event::WindowEvent::CursorMoved {
        device_id: ui::winit::event::DeviceId::dummy(),
        position: (at.x, at.y).into(),
    });
    relayout(&mut driver);
    let top = |driver: &ui::headless::FrameHarness| {
        find(&driver.inspection().root, "Scrollable")
            .map(|node| node.children[0].rect[1])
            .expect("the scroll view")
    };
    let before = top(&driver);
    for _ in 0..4 {
        on_clock(EVENT, || {
            driver.send_host_event(&pixel_wheel(0.0, -120.0));
            driver.poll_frame().unwrap();
        });
    }
    settle(&mut driver);
    let after = top(&driver);
    assert!(
        after < before - 1.0,
        "the page under the pointer moved up: its top {before} -> {after}"
    );
}

/// An app whose capture offers to open Writer, as Camera offers Photos.
#[derive(Default)]
struct OpensWriter {
    rect: ui::kurbo::Rect,
}

impl ui::widget::Widget for OpensWriter {
    ui::provides!(dyn ui::resources::PublishesResources);

    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        _: &ui::Theme,
        _: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> ui::taffy::NodeId {
        taffy
            .new_leaf(ui::taffy::Style {
                flex_grow: 1.0,
                ..Default::default()
            })
            .unwrap()
    }

    fn apply_layout(
        &mut self,
        taffy: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: ui::kurbo::Point,
    ) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn name(&self) -> &'static str {
        "OpensWriter"
    }

    fn get_rect(&self) -> ui::kurbo::Rect {
        self.rect
    }
}

impl ui::resources::PublishesResources for OpensWriter {
    fn resources(&self, _: &ui::automerge::AutoCommit) -> Vec<ui::tilo_actions::Resource> {
        vec![
            ui::tilo_actions::Resource::new("camera.capture", "camera.capture", "Capture")
                .with_action(
                    ui::tilo_actions::remote::ApplicationLaunch::app("so.tilo.writer").spec(
                        "open-writer",
                        "Open Writer",
                        "Open Writer",
                    ),
                ),
        ]
    }
}

/// A desk whose Camera window offers to open Writer; any `so.tilo.` app
/// launches as a window of its own.
struct OpeningApps;

impl TileSource for OpeningApps {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        if tab.title == "Camera" {
            return TileHost::new(ui::widget::TileContent::App(Box::new(Hosted(Box::new(
                OpensWriter::default(),
            )))));
        }
        TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn launch(&mut self, workspace: &Workspace, id: u64, app: &str) -> Option<Tab> {
        Notices(Rc::new(Cell::new(0))).launch(workspace, id, app)
    }
}

/// The page desktop: one frame hosting its windows in process, serving them
/// its own session for the workspace `room`.
fn serving_desk(
    room: &str,
) -> (
    ui::headless::FrameHarness,
    DesktopHandle,
    tilo_desktop_surface::session::Openings,
) {
    let mut minter = IdMinter::new(7);
    let camera = tab(&mut minter, "Camera");
    let tree = seed_tree(&mut minter, "Desk", vec![camera]);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(OpeningApps));
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    driver.set_caller_palette_enabled(false);
    let (session, openings) = tilo_desktop_surface::session::serve(room, std::sync::Arc::default());
    driver.set_session_door(
        ui::RemoteActions::with_host(
            session,
            ui::tilo_actions::remote::Caller::local_person("tester").in_room(room),
        ),
        Some(room.into()),
    );
    driver.relayout();
    (driver, handle, openings)
}

/// Camera's "Open Photos" in the browser desktop: a window asks the page's
/// session to open another app, and the desktop opens it as a window of its
/// own beside the one that asked. What the desktop refuses to open is said
/// over the window that asked, never dropped.
#[test]
fn a_window_opens_another_app_through_the_desktop_s_session() {
    let (mut driver, handle, openings) = serving_desk("room");
    driver
        .invoke_published("camera.capture", "open-writer", &serde_json::Map::new())
        .expect("the session takes the open");
    assert_eq!(
        handle.snapshot("g", vec![]).entries.len(),
        1,
        "not yet opened"
    );
    assert!(openings.open(&handle, "g", |_, _| Ok(())));
    driver.idle();
    driver.relayout();
    let snapshot = handle.snapshot("g", vec![]);
    assert_eq!(snapshot.entries.len(), 2, "{:?}", snapshot.entries);
    assert_eq!(snapshot.entries[1].label, "writer");
    assert_eq!(
        snapshot.active.as_ref().unwrap(),
        &snapshot.entries[1].route,
        "the opened app is in front"
    );
    assert_eq!(driver.refusal_shown(), None);

    let (mut driver, handle, openings) = serving_desk("room");
    driver
        .invoke_published("camera.capture", "open-writer", &serde_json::Map::new())
        .expect("the session takes the open");
    openings.open(&handle, "g", |_, _| {
        Err(ui::tilo_actions::remote::ActionHostError::NotPermitted)
    });
    driver.idle();
    assert_eq!(handle.snapshot("g", vec![]).entries.len(), 1);
    assert!(
        driver.refusal_shown().is_some(),
        "the refusal is said over the window that asked"
    );
}

/// An app whose page fills its window and scrolls a document that can be
/// long or empty, as Speed Test's and every writing surface's does.
struct FillingPage(ui::widget::Scrollable);

impl ui::widget::Decorator for FillingPage {
    type Inner = ui::widget::Scrollable;

    fn inner(&self) -> &ui::widget::Scrollable {
        &self.0
    }

    fn inner_mut(&mut self) -> &mut ui::widget::Scrollable {
        &mut self.0
    }
}

impl ui::widget::AppWidget for FillingPage {
    fn app_kind(&self) -> ui::widget::AppKind {
        ui::widget::AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Filling"
    }
    fn app_icon(&self) -> ui::widget::IconDescriptor {
        ui::widget::IconDescriptor::Glyph("F")
    }
}

/// Every window but the one named `Filling`, which shows `text`.
struct FillingTiles(ui::LocalState<String>);

impl TileSource for FillingTiles {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        if tab.title != "Filling" {
            return TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            });
        }
        let page = ui::widget::Scrollable::new_vertical()
            .fill_parent_height()
            .with_content_min_viewport_height()
            .with_child(Box::new(
                ui::widget::TextInput::from_state(&self.0)
                    .multiline(true)
                    .with_id("filling-editor"),
            ));
        TileHost::new(ui::widget::TileContent::App(Box::new(FillingPage(page))))
    }

    fn launch(&mut self, _: &Workspace, _: u64, _: &str) -> Option<Tab> {
        None
    }
}

/// Where each window's app is drawn: its tile.
fn windows(driver: &ui::headless::FrameHarness) -> Vec<ui::vello::kurbo::Rect> {
    fn walk(node: &ui::headless::InspectionNode, out: &mut Vec<ui::vello::kurbo::Rect>) {
        if node.widget == "TileHost" {
            let [x, y, w, h] = node.rect;
            out.push(ui::vello::kurbo::Rect::new(x, y, x + w, y + h));
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut tiles = Vec::new();
    walk(&driver.inspection().root, &mut tiles);
    tiles
}

/// A window whose page fills it never makes the strip taller than its band,
/// however long its document, and fills the window again once the document
/// is empty. The row of windows and each window's tile took their content's
/// height as their minimum, so a long document raised every window past the
/// screen. A page that fills at least its viewport then kept that height as
/// its floor, and with the page and its content adjusting a pass apart they
/// traded the old and new heights without settling, so the desktop stopped
/// drawing.
#[test]
fn a_filling_page_keeps_its_strip_to_the_band_and_settles() {
    let text = ui::LocalState::new("A line of the document.\n".repeat(120));
    let mut minter = IdMinter::new(7);
    let tabs = ["Notes", "Filling", "Scratch"]
        .iter()
        .map(|title| tab(&mut minter, title))
        .collect();
    let tree = seed_tree(&mut minter, "Desk", tabs);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(FillingTiles(text.clone())));
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    driver.set_caller_palette_enabled(false);
    driver
        .relayout_at((1200, 800))
        .expect("the desktop with a long document settles");
    let long = *driver.targets().get("filling-editor").unwrap();
    let tiles = windows(&driver);
    assert_eq!(tiles.len(), 3, "{tiles:?}");
    for tile in &tiles {
        assert!(
            tile.y1 <= 800.0,
            "no window reaches past the band for a long document: {tile:?} ({long:?})"
        );
    }

    text.set(String::new());
    for _ in 0..3 {
        driver
            .relayout_at((1200, 800))
            .expect("the desktop settles once the document is empty");
    }
    let short = *driver.targets().get("filling-editor").unwrap();
    let tiles = windows(&driver);
    for tile in &tiles {
        assert!(
            tile.y1 <= 800.0,
            "the height the long document filled is no floor once it is gone: {tile:?}"
        );
    }
    let filling = tiles
        .iter()
        .find(|tile| tile.contains(short.origin()))
        .expect("the editor is in a window");
    assert!(
        (short.height() - filling.height()).abs() < 1.0,
        "the empty page fills its window: {short:?} in {filling:?}"
    );
}

/// Windows on records: each record launch opens its app on that record,
/// and the record's window, once open, is the one shown again.
struct RecordWindows {
    opened: Rc<std::cell::RefCell<Vec<(u64, String)>>>,
}

impl TileSource for RecordWindows {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn launch(&mut self, _: &Workspace, id: u64, app: &str) -> Option<Tab> {
        let (app, record) = tilo_desktop_surface::session::launched_record(app)?;
        self.opened.borrow_mut().push((id, record.to_owned()));
        Some(Tab {
            id,
            title: record.into(),
            tile: TileContent::LocalApp {
                app_id: app.into(),
                label: record.into(),
                note: String::new(),
            },
            app_title: None,
        })
    }

    fn showing(&self, app: &str) -> Option<u64> {
        let (_, record) = tilo_desktop_surface::session::launched_record(app)?;
        self.opened
            .borrow()
            .iter()
            .find(|(_, opened)| opened == record)
            .map(|(tab, _)| *tab)
    }
}

/// A shared window's message opens the resource it shares: the page's
/// session opens the resource's app on its record, and asked again shows
/// that window instead of a second one. A resource in another workspace
/// is not this desktop's to open.
#[test]
fn a_window_opens_a_resource_on_its_record_through_the_desktop_s_session() {
    use ui::tilo_actions::remote::{
        APPLICATION_OPEN_ACTION, ActionHost as _, Caller, InvokeRequest, application_record_target,
    };
    let mut minter = IdMinter::new(7);
    let chat = tab(&mut minter, "Chat");
    let tree = seed_tree(&mut minter, "Desk", vec![chat]);
    let opened = Rc::default();
    let (surface, handle) = DesktopSurface::new(
        tree,
        7,
        Box::new(RecordWindows {
            opened: Rc::clone(&opened),
        }),
    );
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1200, 800),
        ui::Theme::dark(),
    );
    let (session, openings) =
        tilo_desktop_surface::session::serve("room", std::sync::Arc::default());
    let caller = Caller::local_person("tester").in_room("room");
    let open = |room: &str| -> InvokeRequest {
        serde_json::from_value(serde_json::json!({
            "invocation": ui::tilo_actions::InvocationId::new(),
            "target": application_record_target(room, "so.tilo.notes", "n:plan", "plan", APPLICATION_OPEN_ACTION),
        }))
        .unwrap()
    };
    let refused = session.invoke(&caller, open("elsewhere"));
    drop(refused);
    assert!(
        !openings.open(&handle, "g", |_, _| Ok(())),
        "nothing asked of here"
    );

    for _ in 0..2 {
        drop(session.invoke(&caller, open("room")));
        assert!(openings.open(&handle, "g", |app, _| {
            assert_eq!(app, "so.tilo.notes", "admitted as its app");
            Ok(())
        }));
        driver.relayout();
    }
    assert_eq!(opened.borrow().len(), 1, "one window on the record");
    let snapshot = handle.snapshot("g", vec![]);
    assert_eq!(snapshot.entries.len(), 2, "{:?}", snapshot.entries);
    assert_eq!(snapshot.entries[1].label, "n:plan");
    assert_eq!(snapshot.active.as_ref(), Some(&snapshot.entries[1].route));
}
