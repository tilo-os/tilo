//! The inspector in a narrow window: a drawer over the artwork that holds
//! its controls and the layers, none of them cut off by the window's edge.

use canvas::App;
use ui::prelude::TestApp;

#[test]
fn the_inspector_drawer_holds_its_controls_inside_the_window() {
    let mut app = TestApp::<App>::capture(Some("landscape")).expect("the scenario stages");
    let width = 600.0;
    app.harness().resize((600, 800)).expect("resizes");
    let toggle = app
        .harness()
        .targets()
        .into_keys()
        .find(|name| name.ends_with("nav-shell-inspector"))
        .expect("a narrow window offers the inspector toggle");
    app.harness()
        .click_named(&toggle)
        .expect("the toggle opens");
    assert!(
        app.harness()
            .settle_for_capture(tilo_test_env::hang_bound()),
        "the drawer settles"
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
    let header: Vec<_> = targets
        .iter()
        .filter(|(name, rect)| {
            (name.starts_with("canvas-zoom-")
                || name.as_str() == "canvas-menu"
                || name.ends_with("nav-shell-inspector")
                || name.as_str() == "standalone-history-open")
                && rect.height() > 0.0
        })
        .collect();
    assert!(
        !header.is_empty(),
        "the actual header exposes its mounted controls"
    );
    for (index, (name, rect)) in header.iter().enumerate() {
        assert!(rect.x0 >= 0.0 && rect.x1 <= 390.0, "{name}: {rect:?}");
        assert!(
            rect.y1 < targets["canvas-welcome-close"].y0,
            "header and welcome overlap: {name} {rect:?}"
        );
        for (other, other_rect) in &header[index + 1..] {
            assert!(
                rect.intersect(**other_rect).area().total_cmp(&0.0).is_eq(),
                "header controls overlap: {name} {rect:?}; {other} {other_rect:?}"
            );
        }
    }
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
