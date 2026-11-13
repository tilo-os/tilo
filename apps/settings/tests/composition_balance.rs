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

#[test]
fn compact_heading_and_region_layout_stay_balanced() {
    // Each page is laid out once and only read: every check below takes the
    // window's widget tree by shared reference.
    let mut accounts = crate::support::window_on(Category::ExternalAccounts, (W, H));
    let mut region = crate::support::window_on(Category::RegionFormats, (W, H));
    for (app, category, label, limit) in [
        (
            &mut accounts,
            Category::ExternalAccounts,
            "Add Email",
            370.0,
        ),
        (&mut region, Category::RegionFormats, "Language", 220.0),
    ] {
        let mut targets = Vec::new();
        named(app.harness().root_widget(), label, &mut targets);
        let first = targets.first().expect("visible first control");
        assert!(
            first.y1 < limit,
            "{category:?}: first control {first:?} below {limit}"
        );
    }

    let root = region.harness().root_widget();
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

    assert_finite_rects(root, "root");
}
