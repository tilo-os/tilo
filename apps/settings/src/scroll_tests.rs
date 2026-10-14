//! A pane keeps where the person scrolled it while they change its
//! controls, in the hosted window whose controls are bound to the
//! settings document.

use crate::{SettingsShell, categories::Category, settings_app::SettingsApp};
use ui::framework::testing::TestApp;
use ui::vello::kurbo::Rect;
use ui::widget::Widget;

/// The content pane's scroller (the right-most `Scrollable`): its offset.
fn content_offset(root: &dyn Widget) -> f64 {
    fn collect<'a>(w: &'a dyn Widget, out: &mut Vec<&'a dyn Widget>) {
        if w.name() == "Scrollable" {
            out.push(w);
        }
        for c in w.children() {
            collect(c, out);
        }
    }
    let mut all = Vec::new();
    collect(root, &mut all);
    let content = all
        .into_iter()
        .max_by(|a, b| a.get_rect().x0.total_cmp(&b.get_rect().x0))
        .expect("a content scroller");
    let child = content.children()[0];
    content.get_rect().y0 - child.get_rect().y0
}

fn controls(w: &dyn Widget, viewport: Rect, out: &mut Vec<(String, Rect)>) {
    if matches!(w.name(), "Toggle" | "Dropdown" | "Slider" | "ToggleGroup") {
        let r = w.get_rect();
        if r.y0 > viewport.y0 + 80.0 && r.y1 < viewport.y1 - 40.0 && r.x0 > viewport.x0 {
            out.push((w.name().to_string(), r));
        }
    }
    for c in w.children() {
        controls(c, viewport, out);
    }
}

fn settle(app: &mut TestApp<SettingsShell>) {
    for _ in 0..30 {
        app.settle();
        let _ = app.harness().poll_frame();
    }
}

/// The person scrolls a pane down at the device's size and changes its
/// toggles, choices and sliders: each change is applied through the
/// settings document and the pane rebuilt, and the pane stays where it was
/// scrolled instead of jumping back to the top.
#[test]
fn changing_a_control_keeps_the_pane_scrolled() {
    ui::i18n::I18nManager::init("en-US");
    for category in [Category::Power, Category::Pointing, Category::Displays] {
        let mut app = TestApp::<SettingsShell>::new().expect("Settings mounts");
        app.harness().resize((1472, 920)).unwrap();
        app.call(
            "open",
            serde_json::json!({ "anchor": crate::effects::pane_row_id(category) }),
        )
        .expect("the pane opens");
        settle(&mut app);
        app.harness()
            .send(&ui::synthetic_input::cursor_moved(1000.0, 500.0));
        app.harness().scroll(0.0, -4.0);
        settle(&mut app);
        let scrolled = content_offset(app.harness().root_widget());
        assert!(
            scrolled > 1.0,
            "{category:?}: the pane scrolled ({scrolled})"
        );
        let mut found = Vec::new();
        controls(
            app.harness().root_widget(),
            Rect::new(400.0, 0.0, 1472.0, 920.0),
            &mut found,
        );
        assert!(!found.is_empty(), "{category:?} shows a control to change");
        for (name, rect) in found.into_iter().take(3) {
            let at = content_offset(app.harness().root_widget());
            app.harness().click_at(rect.center().x, rect.center().y);
            settle(&mut app);
            if name == "Dropdown" {
                app.harness().key("ArrowDown");
                app.harness().key("Enter");
                settle(&mut app);
            }
            let after = content_offset(app.harness().root_widget());
            assert_eq!(
                app.with_widget::<SettingsApp, _>(|s| s.selected()),
                Some(category)
            );
            assert!(
                after >= at - 0.5,
                "{category:?}: changing a {name} moved the pane back from {at} to {after}"
            );
        }
    }
}
