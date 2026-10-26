//! The inspector in a narrow window, docked beside the artwork where the
//! artwork keeps its least width and a sheet over it where it would not:
//! either holds its controls and the layers, none of them cut off by the
//! window's edge.

use canvas::App;
use ui::prelude::TestApp;

#[test]
fn the_inspector_drawer_holds_its_controls_inside_the_window() {
    let mut app = TestApp::<App>::capture(Some("landscape")).expect("the scenario stages");
    let width = 600.0;
    app.harness().resize((600, 800)).expect("resizes");
    // The tools' toggle is the window bar's slot now, not a control in a row
    // of the app's own.
    let declared = ui::window_toolbar::declared().expect("the window declares its slots");
    assert!(
        declared
            .actions
            .iter()
            .any(|action| action.id == "canvas.tools"),
        "a narrow window offers the tools' toggle in its bar"
    );
    // At 600 px the artwork keeps its least width beside the inspector, so
    // the inspector docks and stands open; only a closed one is opened.
    if !app.harness().targets().contains_key("canvas-dock") {
        app.harness()
            .press_window_slot(ui::window_toolbar::ToolbarEvent::Action {
                id: "canvas.tools".to_owned(),
            });
    }
    assert!(
        app.harness()
            .settle_for_capture(tilo_test_env::hang_bound()),
        "the inspector settles"
    );
    let targets = app.harness().targets();
    let own: Vec<_> = targets
        .iter()
        .filter(|(name, _)| name.starts_with("canvas-") || name.starts_with("add-layer"))
        .collect();
    assert!(
        own.iter().any(|(name, _)| *name == "canvas-dock"),
        "the open drawer shows the layers and history: {targets:#?}"
    );
    for (name, rect) in own {
        assert!(
            rect.x0 >= 0.0 && rect.x1 <= width,
            "{name} leaves the window: {rect:?}"
        );
    }
}

#[test]
fn blank_phone_welcome_and_header_controls_stay_inside_the_window() {
    let mut app = TestApp::<App>::capture_at(None, (390, 844)).expect("blank artwork");
    assert!(
        app.harness()
            .settle_for_capture(tilo_test_env::hang_bound()),
        "blank window did not settle: {:?}; pending visuals: {}",
        app.harness().frame_schedule(),
        app.harness().root_widget().has_pending_visual_work()
    );
    let targets = app.harness().targets();
    let welcome = [
        "canvas-welcome-new",
        "canvas-welcome-open",
        "canvas-welcome-close",
    ];
    for name in welcome {
        let rect = targets.get(name).expect("the actual welcome control");
        assert!(
            rect.width() > 0.0 && rect.x0 >= 0.0 && rect.x1 <= 390.0,
            "{name} leaves the phone viewport: {rect:?}"
        );
    }
    let add_picture = targets
        .get("canvas-add-picture")
        .expect("adding a picture remains mounted");
    assert!(add_picture.x0 >= 0.0 && add_picture.x1 <= 390.0);
    assert!(
        add_picture.y0 > targets["canvas-welcome-new"].y1,
        "the touch import action is below the welcome, outside its scrolling viewport"
    );
    // The artwork has the window from its top edge: the row that used to hold
    // the title, the menu and the zoom stands in the window bar now, and the
    // controls it held are slots there rather than buttons over the artwork.
    let canvas = targets.get("canvas").expect("the artwork pane");
    assert!(
        canvas.y0 <= 0.5,
        "no row stands above the artwork on a phone: {canvas:?}"
    );
    let declared = ui::window_toolbar::declared().expect("the window declares its slots");
    let ids: Vec<&str> = declared
        .actions
        .iter()
        .flat_map(|action| {
            std::iter::once(action.id.as_str())
                .chain(action.items.iter().map(|item| item.id.as_str()))
        })
        .collect();
    for wanted in [
        "canvas.menu",
        "canvas.add-picture",
        "canvas.zoom-in",
        "canvas.zoom-out",
        "canvas.zoom-fit",
        "canvas.tools",
    ] {
        assert!(ids.contains(&wanted), "{wanted} is a slot in {ids:?}");
    }
    assert!(
        declared.location.is_some(),
        "the artwork's title is the bar's location on a phone too"
    );
}

#[test]
fn phone_tools_live_below_the_artwork_and_survive_rotation() {
    let mut app =
        TestApp::<App>::capture_at(Some("landscape"), (390, 844)).expect("the actual artwork view");
    assert!(
        !app.harness().root_widget().has_pending_visual_work(),
        "capture staging must await the landscape's paint workers before returning"
    );
    for viewport in [(390, 844), (844, 390)] {
        app.harness().resize(viewport).expect("rotation lays out");
        assert!(
            app.harness()
                .settle_for_capture(tilo_test_env::hang_bound())
        );
        let targets = app.harness().targets();
        let tools = targets.get("Tool").expect("the shared tool control");
        assert!(
            tools.width() > tools.height(),
            "tools are a horizontal strip: {tools:?}"
        );
        assert!(
            tools.y0 > f64::from(viewport.1) / 2.0,
            "tools sit below the artwork: {tools:?}"
        );
        let audit = app.harness().native_app_audit();
        assert!(
            audit.is_certified(),
            "the mobile composition keeps every canonical action: {audit:#?}"
        );
    }
}

/// Every accessible name drawn in `root`'s tree.
fn names(root: &dyn ui::widget::Widget, out: &mut Vec<String>) {
    if let Some(name) = root.accessibility().and_then(|node| node.name) {
        out.push(name);
    }
    for child in root.children() {
        names(child, out);
    }
}

/// The welcome offers a new artwork, opening one and the recent ones; it
/// teaches no keys or tools, which the app shows where they are used.
#[test]
fn the_welcome_lists_no_shortcuts() {
    let mut app = TestApp::<App>::capture_at(None, (1280, 800)).expect("blank artwork");
    let targets = app.harness().targets();
    assert!(
        targets.contains_key("canvas-welcome-new"),
        "the welcome is up"
    );
    let mut shown = Vec::new();
    names(app.harness().root_widget(), &mut shown);
    for teaching in ["Press ?", "(B)", "(R, E, L)", "ask one to paint"] {
        assert!(
            shown.iter().all(|name| !name.contains(teaching)),
            "the welcome teaches {teaching:?}: {shown:?}"
        );
    }
}
