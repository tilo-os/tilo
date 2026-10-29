use settings::categories::Category;
use ui::widget::Widget;

use vello::kurbo::Rect;

const W: f32 = 1200.0;
const H: f32 = 760.0;

fn named(widget: &dyn Widget, label: &str, out: &mut Vec<Rect>) {
    if widget.accessibility().and_then(|n| n.name).as_deref() == Some(label) {
        out.push(widget.get_rect());
    }
    for child in widget.children() {
        named(child, label, out);
    }
}

fn assert_finite_rects(widget: &dyn Widget, path: &str) {
    let rect = widget.get_rect();
    let label = widget.accessibility().and_then(|node| node.name);
    for (index, child) in widget.children().into_iter().enumerate() {
        assert_finite_rects(child, &format!("{path}/{}[{index}]", widget.name()));
    }
    assert!(
        [rect.x0, rect.y0, rect.x1, rect.y1]
            .into_iter()
            .all(f64::is_finite),
        "{path}/{} {:?} has non-finite geometry: {rect:?}",
        widget.name(),
        label
    );
}

/// Compact first controls, the region preview and finite region layout.
#[test]
fn compact_heading_and_region_layout_stay_balanced() {
    // Case `first_account_action_and_region_field_follow_compact_heading`.
    {
        for (category, label, limit) in [
            (Category::ExternalAccounts, "Add Email", 370.0),
            (Category::RegionFormats, "Language", 220.0),
        ] {
            let mut app = crate::support::window_on(category, (W, H));
            let mut targets = Vec::new();
            named(app.harness().root_widget(), label, &mut targets);
            let first = targets.first().expect("visible first control");
            assert!(
                first.y1 < limit,
                "{category:?}: first control {first:?} below {limit}"
            );
        }
    }

    // Case `region_preview_follows_every_format_control`.
    {
        let mut app = crate::support::window_on(Category::RegionFormats, (W, H));
        let root = app.harness().root_widget();
        let mut last = Vec::new();
        named(root, "First day of week", &mut last);
        let mut preview = Vec::new();
        named(root, "Preview", &mut preview);
        assert!(
            last[0].y1 < preview[0].y0,
            "last {:?}, preview {:?}",
            last[0],
            preview[0]
        );
    }

    // Case `region_layout_keeps_every_painted_widget_finite`.
    {
        let mut app = crate::support::window_on(Category::RegionFormats, (W, H));
        assert_finite_rects(app.harness().root_widget(), "root");
    }
}
