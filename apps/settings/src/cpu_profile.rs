//! Content-free CPU observations of mounted production Settings interactions.
//! Scope durations nest and are not additive. No latency budget is asserted.

use crate::{SettingsShell, categories::Category};
use ui::framework::testing::TestApp;

use ui::synthetic_input::cursor_moved;

fn sidebar_toggle(app: &mut TestApp<SettingsShell>) -> ui::vello::kurbo::Rect {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find_map(|(_, node)| {
            (node.role() == ui::accesskit::Role::Button && node.label() == Some("Hide sidebar"))
                .then(|| node.bounds())
                .flatten()
        })
        .map(|rect| ui::vello::kurbo::Rect::new(rect.x0, rect.y0, rect.x1, rect.y1))
        .expect("the mounted shared sidebar toggle is accessible")
}

fn dirty_leaves(widget: &dyn ui::Widget, out: &mut Vec<serde_json::Value>) {
    let children = widget.children();
    if children.is_empty() && widget.layout_needs_measure() {
        out.push(serde_json::json!({
            "widget": widget.concrete_type_name(), "test_id": widget.test_id(),
        }));
    }
    for child in children {
        dirty_leaves(child, out);
    }
}

fn measure(
    app: &mut TestApp<SettingsShell>,
    category: Category,
    operation: &str,
    sample: usize,
    act: impl FnOnce(&mut TestApp<SettingsShell>),
) {
    app.harness().clear_profile_samples();
    let ((elapsed, drain_elapsed, frames), phases) = ui::tilo_resource::profiling::observe(|| {
        let started = std::time::Instant::now();
        act(app);
        let elapsed = started.elapsed().as_nanos();
        let drain = std::time::Instant::now();
        let frames = app.harness().take_profile_samples();
        (elapsed, drain.elapsed().as_nanos(), frames)
    });
    assert!(
        !frames.is_empty(),
        "the measured interaction paints real frames"
    );
    assert!(frames.iter().any(|frame| frame.draw_us > 0));
    assert_eq!(
        app.harness().native_input_snapshot().unwrap()["selected_category"],
        category.slug(),
        "the mounted canonical pane is the measured subject"
    );
    assert!(
        phases
            .iter()
            .any(|scope| scope.phase == "framework.publication")
    );
    {
        let layout = app.harness().last_relayout();
        if category == Category::MouseKeyboard && operation == "pointer-hover" && sample >= 2 {
            assert_eq!(
                layout.marked_leaves, 0,
                "the collapsed keyboard inspection is an empty container, not a changing intrinsic-size leaf"
            );
        }
        let mut leaves = Vec::new();
        dirty_leaves(app.harness().root_widget(), &mut leaves);
        eprintln!(
            "settings-cpu-interaction {}",
            serde_json::json!({
                "architecture": std::env::consts::ARCH,
                "profile": if cfg!(debug_assertions) { "test-debug" } else { "optimized" },
                "pane": category.slug(), "operation": operation, "sample": sample,
                "warmup": sample < 2,
                "last_layout_rebuilt": layout.rebuilt,
                "last_layout_measured": layout.measured,
                "last_layout_marked_dirty": layout.marked_dirty,
                "last_layout_marked_leaves": layout.marked_leaves,
                "event_elapsed_ns": elapsed, "profile_drain_elapsed_ns": drain_elapsed,
                "phases": phases, "frames": frames, "dirty_leaves_after_draw": leaves,
                "includes_cpu_layout_and_scene_draw": true,
                "includes_gpu_or_display": false,
                "execution_host": "mounted headless framework; no GPU or device presentation",
            })
        );
    }
}

#[test]
fn mounted_settings_cpu_interactions_observe_canonical_panes_and_shared_controls() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<SettingsShell>::new().expect("Settings mounts normally");
    app.harness().resize((1472, 920)).unwrap();
    let address = crate::effects::open_pane_address(Category::Displays);
    app.harness()
        .invoke_action(&address, &serde_json::Map::new(), false, None)
        .expect("initial ordinary pane selection")
        .completed()
        .expect("selection completes");
    app.harness().begin_profiling();
    for sample in 0..9 {
        for category in [
            Category::Appearance,
            Category::Network,
            Category::MouseKeyboard,
            Category::Displays,
        ] {
            let bounds = app
                .harness()
                .a11y_nodes()
                .into_iter()
                .find_map(|(_, node)| {
                    (node.role() == ui::accesskit::Role::TreeItem
                        && node.label() == Some(category.label().as_str()))
                    .then(|| node.bounds())
                    .flatten()
                })
                .expect("the actual visible navigation row is accessible");
            assert!(bounds.y0 >= 0.0 && bounds.y1 <= 920.0);
            measure(
                &mut app,
                category,
                "pointer-pane-navigation",
                sample,
                |app| {
                    app.harness().click_at(
                        f64::midpoint(bounds.x0, bounds.x1),
                        f64::midpoint(bounds.y0, bounds.y1),
                    );
                },
            );
            let toggle = sidebar_toggle(&mut app);
            measure(&mut app, category, "pointer-hover", sample, |app| {
                app.harness().send(&cursor_moved(1190.0, 890.0));
                app.harness()
                    .send(&cursor_moved(toggle.center().x, toggle.center().y));
            });
            measure(&mut app, category, "sidebar-control-click", sample, |app| {
                app.harness().click_at(toggle.center().x, toggle.center().y);
            });
            assert_eq!(
                app.harness().native_input_snapshot().unwrap()["sidebar_open"],
                false
            );
            app.harness().key("Escape");
            // Restore through the ordinary shared control; inspection does not
            // manufacture open state or bypass the production layout path.
            let bounds = app
                .harness()
                .a11y_nodes()
                .into_iter()
                .find_map(|(_, node)| {
                    (node.role() == ui::accesskit::Role::Button
                        && node.label() == Some("Show sidebar"))
                    .then(|| node.bounds())
                    .flatten()
                })
                .expect("the ordinary sidebar reopen control stays visible");
            app.harness().click_at(
                f64::midpoint(bounds.x0, bounds.x1),
                f64::midpoint(bounds.y0, bounds.y1),
            );
            assert_eq!(
                app.harness().native_input_snapshot().unwrap()["sidebar_open"],
                true
            );
        }
    }
}
