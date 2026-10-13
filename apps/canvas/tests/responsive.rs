//! Controls must fit the logical window, including a small display at 2× scale.
#![allow(clippy::unwrap_used, reason = "assertions over real editor controls")]
mod support;
use canvas::App;
use ui::prelude::TestApp;

#[test]
fn short_scaled_windows_keep_project_choices_and_brush_controls_reachable() {
    for (width, height, scale) in [(800, 534, 2.0), (640, 480, 1.0), (868, 772, 1.0)] {
        let mut app = TestApp::<App>::new().unwrap();
        app.harness().resize((width, height)).unwrap();
        app.harness().set_input_scale(scale).unwrap();
        app.harness().click_named("canvas-welcome-close").unwrap();
        app.call("new-artwork", serde_json::json!({"width":128,"height":128}))
            .unwrap();
        for i in 0..6 {
            app.call("add-layer", serde_json::json!({"id":format!("test-layer-{i}"),"name":format!("Long foreground layer with editable objects {i}")})).unwrap();
        }
        open_inspector(&mut app);
        let shots = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/look/canvas-small-windows");
        app.harness()
            .render_png(&shots.join(format!("{width}x{height}-initial.png")));
        for key in [
            "canvas-workspace-auto",
            "canvas-workspace-vector",
            "canvas-workspace-photo",
            "canvas-workspace-pixel",
        ] {
            let snapshot = app.harness().inspection();
            let label = ui::i18n::I18nManager::lookup_plain(key);
            let bounds = support::named_bounds(&snapshot.root, &label)
                .unwrap_or_else(|| panic!("{label} unavailable at {width}x{height}"));
            assert!(
                bounds.x0 >= 0.0 && bounds.x1 <= f64::from(width) && bounds.y1 <= f64::from(height),
                "{label} clipped at {width}×{height}: {bounds:?}"
            );
            app.harness().click_at(bounds.center().x, bounds.center().y);
        }
        let dock = app.harness().targets()["canvas-dock"];
        assert!(
            dock.y0 > f64::from(height) * 0.35 && dock.y1 < f64::from(height) - 100.0,
            "both panels receive usable height: {dock:?}"
        );
        // Use real wheel input in the inspector; the dock stays fixed while
        // its sibling viewport reveals the brush size control.
        let mut reachable = false;
        for _ in 0..12 {
            if let Some(bounds) = app.harness().targets().get("canvas-brush-size").copied()
                && bounds.y0 >= 0.0
                && bounds.y1 <= dock.y0
            {
                app.harness().click_at(bounds.center().x, bounds.center().y);
                reachable = true;
                break;
            }
            app.harness().send(&ui::synthetic_input::cursor_moved(
                (f64::from(width) - 130.0) * scale,
                (dock.y0 - 30.0) * scale,
            ));
            app.harness().scroll(0.0, -2.0);
        }
        assert!(
            reachable,
            "brush size is scrollable above the dock at {width}×{height}"
        );
        app.harness().click_named("nav-shell-inspector").unwrap();
        assert!(
            app.harness()
                .settle_for_capture(tilo_test_env::hang_bound())
        );
        let canvas = app.harness().targets()["canvas"];
        assert!(
            canvas.width() >= 250.0 && canvas.height() >= 200.0,
            "usable drawing area: {canvas:?}"
        );
        app.harness().click_at(canvas.center().x, canvas.center().y);
        assert_eq!(app.state().marks.len(), 1);
        assert_eq!(app.state().marks[0].style.brush.as_deref(), Some("pixel"));
        let art = app.state();
        let canvas::model::Shape::Stroke { size, .. } = &art.marks[0].shape else {
            panic!("a painted brush stroke");
        };
        assert!(
            *size > 1.0,
            "the revealed slider changes the brush used by a real stroke: {size}"
        );
    }
}

#[test]
fn the_layer_footer_can_be_scrolled_into_a_short_window_and_edited() {
    let mut app = TestApp::<App>::new().unwrap();
    app.harness().resize((800, 534)).unwrap();
    app.harness().click_named("canvas-welcome-close").unwrap();
    for i in 0..6 {
        app.call(
            "add-layer",
            serde_json::json!({"id":format!("test-layer-{i}"),"name":format!("Layer {i}")}),
        )
        .unwrap();
    }
    open_inspector(&mut app);
    let dock = app.harness().targets()["canvas-dock"];
    let mut reached = false;
    for _ in 0..8 {
        let targets = app.harness().targets();
        let tree = targets["canvas-layer-tree"];
        let opacity = targets["canvas-layer-opacity"];
        assert!(
            tree.height() >= f64::from(5.0 * ui::tokens::Metrics::BASE.panel_row_height)
                && tree.y1 <= opacity.y0,
            "layer rows must end before their controls: tree {tree:?}, opacity {opacity:?}"
        );
        let target = app.harness().targets()["canvas-layer-folder"];
        if target.y0 >= dock.y1 && target.y1 <= 534.0 {
            app.harness().click_at(target.center().x, target.center().y);
            app.harness().type_text("Foreground");
            app.harness().key("Enter");
            reached = true;
            break;
        }
        app.harness()
            .send(&ui::synthetic_input::cursor_moved(670.0, 520.0));
        app.harness().scroll(0.0, -1.0);
    }
    assert!(reached, "the selected layer's folder field can be revealed");
    assert!(
        app.state()
            .layers
            .iter()
            .any(|layer| layer.group == "Foreground")
    );
    assert!(app.undo());
    assert!(
        app.state()
            .layers
            .iter()
            .all(|layer| layer.group.is_empty())
    );
}

fn open_inspector(app: &mut TestApp<App>) {
    let settled = app
        .harness()
        .settle_for_capture(tilo_test_env::hang_bound());
    assert!(
        settled,
        "unsettled frame requests: {:?}",
        app.harness().frame_requesters()
    );
    if !app.harness().targets().contains_key("canvas-dock") {
        app.harness().click_named("nav-shell-inspector").unwrap();
    }
    let settled = app
        .harness()
        .settle_for_capture(tilo_test_env::hang_bound());
    assert!(
        settled,
        "unsettled frame requests: {:?}",
        app.harness().frame_requesters()
    );
}
