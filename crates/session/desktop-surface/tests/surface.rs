//! The desktop surface inside the toolkit's real frame pipeline: navigation
//! commands move the model and the live widgets together.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed lookup is the assertion"
)]

use std::cell::Cell;
use std::rc::Rc;

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
        let title = app.strip_prefix("so.tilo.")?.to_owned();
        Some(Tab {
            id,
            title: title.clone(),
            tile: TileContent::LocalApp {
                app_id: app.into(),
                label: title,
                note: String::new(),
            },
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
                unavailable: None,
            })
            .chain([tilo_desktop_search::providers::AppEntry {
                id: "so.tilo.imager".into(),
                title: "Imager".into(),
                category: Some("System".into()),
                description: None,
                unavailable: Some("Linux and macOS only".into()),
            }])
            .collect()
    }
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
    }
}

fn mount() -> (ui::headless::Driver, DesktopHandle, Rc<Cell<usize>>) {
    let mut minter = IdMinter::new(7);
    let tabs = vec![tab(&mut minter, "Notes"), tab(&mut minter, "Scratch")];
    let tree = seed_tree(&mut minter, "Desk", tabs);
    let built = Rc::new(Cell::new(0));
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Notices(Rc::clone(&built))));
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::Driver::new_with_controlled_inputs(
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

fn dispatch(driver: &mut ui::headless::Driver, handle: &DesktopHandle, wire: serde_json::Value) {
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
    let idle = |driver: &ui::headless::Driver| {
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
    for app in ["so.tilo.a", "so.tilo.b", "so.tilo.c", "so.tilo.d"] {
        dispatch(
            &mut driver,
            &handle,
            serde_json::json!({"kind":"launch","generation":"g","app":app}),
        );
    }
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
    let mut driver = ui::headless::Driver::new_with_controlled_inputs(
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
    let mut state = ui::automerge::AutoCommit::new();
    ui::widget::Widget::update(&mut surface, &mut state, &ui::automerge::ROOT);
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
    let mut driver = ui::headless::Driver::new_with_controlled_inputs(
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
    fn permission_hint(&self) -> Option<ui::widget::AppKind> {
        None
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
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = ui::kurbo::Rect::from_origin_size(
            (
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
            ),
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
    }

    fn draw(
        &mut self,
        _: &mut ui::vello::Scene,
        _: ui::kurbo::Affine,
        _: &mut ui::TextRenderer,
        _: &ui::Theme,
        _: &ui::presence::PresenceCtx,
    ) {
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

fn mount_apps(titles: &[&str]) -> (ui::headless::Driver, DesktopHandle, Routes) {
    let mut minter = IdMinter::new(7);
    let tabs = titles.iter().map(|title| tab(&mut minter, title)).collect();
    let tree = seed_tree(&mut minter, "Desk", tabs);
    let routes = Routes::default();
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Apps(Rc::clone(&routes))));
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::Driver::new_with_controlled_inputs(
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
    driver: &mut ui::headless::Driver,
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

/// Send one page wheel event as winit's web backend reports it: negated,
/// in physical pixels.
fn page_wheel(driver: &mut ui::headless::Driver, delta_x: f64, delta_y: f64) {
    driver.send(&ui::winit::event::WindowEvent::MouseWheel {
        device_id: ui::winit::event::DeviceId::dummy(),
        delta: ui::winit::event::MouseScrollDelta::PixelDelta(
            ui::winit::dpi::PhysicalPosition::new(-delta_x, -delta_y),
        ),
        phase: ui::winit::event::TouchPhase::Moved,
    });
    driver.relayout();
}

fn pan_offset(handle: &DesktopHandle) -> f64 {
    handle.snapshot("g", vec![]).desktop_pan.unwrap().offset
}

/// Longer than the surface's swipe gap, so the next swipe is a new gesture.
fn pause() {
    std::thread::sleep(std::time::Duration::from_millis(260));
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

    let first_window_x = |driver: &ui::headless::Driver| {
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

    // A flick past the end stops the strip there, for the whole gesture.
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
fn the_tasks_view_opens_over_the_workspace_and_the_others_say_why_not() {
    let (mut driver, handle, _) = mount_apps(&["A"]);
    let mut tree = handle.read(Clone::clone);
    tree.workspaces[0]
        .tasks
        .push(tilo_desktop_model::WorkspaceTask {
            id: "t1".into(),
            title: "Review the draft".into(),
            created_by: "tester".into(),
            assigned_agent: None,
            route: tilo_desktop_model::WorkspaceTaskRoute::Manual,
            status: tilo_desktop_model::WorkspaceTaskStatus::InProgress,
            resource: None,
            created_at: 1,
            updated_at: 2,
            audit: vec![],
        });
    handle.replace_tree(tree);
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

    let saved = handle.revision();
    try_dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"workspace-activity","generation":"g","workspace":workspace,"view":"tasks"}),
    )
    .unwrap();
    let json = serde_json::to_value(handle.snapshot("g", vec![])).unwrap();
    assert_eq!(json["workspaceActivity"]["view"], "tasks");
    assert_eq!(json["workspaceActivity"]["workspace"], workspace);
    assert_eq!(
        json["workspaceActivity"]["tasks"][0]["title"],
        "Review the draft"
    );
    assert_eq!(
        json["workspaceActivity"]["tasks"][0]["statusLabel"],
        "In progress"
    );
    assert_eq!(
        handle.revision(),
        saved,
        "opening a view is not a tree change"
    );

    try_dispatch(
        &mut driver,
        &handle,
        serde_json::json!({"kind":"close-workspace-activity","generation":"g","workspace":workspace}),
    )
    .unwrap();
    let json = serde_json::to_value(handle.snapshot("g", vec![])).unwrap();
    assert!(json["workspaceActivity"].is_null());
    assert_eq!(json["workspaceActivityCloseReason"], "dismissed");
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
fn a_run_of_sideways_wheel_clicks_focuses_and_reveals_the_next_window() {
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
    let mut driver = ui::headless::Driver::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    driver.relayout();
    let entries = handle.snapshot("g", vec![]).entries;
    driver.send(&ui::winit::event::WindowEvent::CursorMoved {
        device_id: ui::winit::event::DeviceId::dummy(),
        position: ui::winit::dpi::PhysicalPosition::new(600.0, 400.0),
    });
    // A mouse tilt wheel held sideways: one run of line-delta clicks.
    for dx in [0.3, 1.0, 2.0, 1.0, 0.5] {
        driver.send(&ui::winit::event::WindowEvent::MouseWheel {
            device_id: ui::winit::event::DeviceId::dummy(),
            delta: ui::winit::event::MouseScrollDelta::LineDelta(dx, 0.0),
            phase: ui::winit::event::TouchPhase::Moved,
        });
    }
    driver.relayout();
    assert_eq!(
        handle.snapshot("g", vec![]).active.unwrap(),
        entries[1].route,
        "one run of clicks, one window along the strip"
    );
    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_millis(250));
        driver.send(&ui::winit::event::WindowEvent::MouseWheel {
            device_id: ui::winit::event::DeviceId::dummy(),
            delta: ui::winit::event::MouseScrollDelta::LineDelta(1.0, 0.0),
            phase: ui::winit::event::TouchPhase::Moved,
        });
        driver.relayout();
    }
    assert_eq!(
        handle.snapshot("g", vec![]).active.unwrap(),
        entries[3].route,
        "stops at the last window"
    );
    let mut rects = Vec::new();
    groups(&driver.inspection().root, &mut rects);
    rects.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let [x, _, width, _] = rects[3];
    assert!(
        x >= 0.0 && x + width <= 1200.5,
        "the focused window is in view: {rects:?}"
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
    assert_eq!(first.name.as_deref(), Some("Open Writer"));
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
fn search_lists_an_app_this_desktop_cannot_run_with_why_and_opens_nothing() {
    use ui::a11y::Action;
    let (mut driver, handle, _) = mount();
    let before = labels(&handle.snapshot("g", vec![]));
    handle.toggle_search();
    driver.relayout();
    driver
        .perform_semantic_named(
            "command_palette_input",
            Some("command-palette:query"),
            &Action::SetValue("imager".into()),
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
        .expect("the unavailable app is listed");
    assert_eq!(first.name.as_deref(), Some("Imager"));
    assert!(
        format!("{first:?}").contains("Linux and macOS only"),
        "the row says why: {first:?}"
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
        labels(&handle.snapshot("g", vec![])),
        before,
        "nothing opened"
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
    let mut driver = ui::headless::Driver::new_with_controlled_inputs(
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
