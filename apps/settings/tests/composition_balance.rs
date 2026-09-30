use settings::SettingsApp;
use settings::categories::Category;
use ui::headless::Framed;
use ui::widget::Widget;

use vello::kurbo::Rect;

const W: f32 = 1200.0;
const H: f32 = 760.0;

fn lay_out(app: SettingsApp) -> Framed<SettingsApp> {
    lay_out_at(app, W, H)
}

/// Run the production frame — sync, layout, draw — on any widget at an
/// explicit viewport. Shared with the sibling
/// geometry modules in this test binary (`alignment_review`).
pub(crate) fn lay_out_at<T: Widget + 'static>(widget: T, w: f32, h: f32) -> Framed<T> {
    Framed::new(
        ui::State::new(),
        widget,
        ui::headless::viewport((w, h)),
        ui::Theme::dark(),
    )
}

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

/// Compact headings, the region preview and finite region layout.
#[test]
fn compact_heading_and_region_layout_stay_balanced() {
    // Case `first_account_action_and_region_field_follow_compact_heading`.
    {
        let config = crate::support::ScratchConfig::new();
        for (category, label, limit) in [
            (Category::ExternalAccounts, "Add account", 370.0),
            (Category::RegionFormats, "Currency", 220.0),
        ] {
            let app = config.app().with_category(category);
            let app = lay_out(app);
            let mut targets = Vec::new();
            named(&*app, label, &mut targets);
            let first = targets.first().expect("visible first control");
            assert!(
                first.y1 < limit,
                "{category:?}: first control {first:?} below {limit}"
            );
            let mut headings = Vec::new();
            named(&*app, &category.label(), &mut headings);
            assert!(headings.iter().any(|r| r.y1 < first.y0));
        }
    }

    // Case `region_preview_follows_every_format_control`.
    {
        let config = crate::support::ScratchConfig::new();
        let app = config.app().with_category(Category::RegionFormats);
        let app = lay_out(app);
        let mut last = Vec::new();
        named(&*app, "First day of week", &mut last);
        let mut preview = Vec::new();
        named(&*app, "Preview", &mut preview);
        assert!(
            last[0].y1 < preview[0].y0,
            "last {:?}, preview {:?}",
            last[0],
            preview[0]
        );
    }

    // Case `region_layout_keeps_every_painted_widget_finite`.
    {
        let config = crate::support::ScratchConfig::new();
        let app = config.app().with_category(Category::RegionFormats);
        let app = lay_out(app);
        assert_finite_rects(&*app, "root");
    }
}
