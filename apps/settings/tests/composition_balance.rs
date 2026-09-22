use std::collections::HashMap;

use settings::SettingsApp;
use settings::categories::Category;
use taffy::{NodeId, TaffyTree};
use ui::text_render::TextRenderer;
use ui::widget::Widget;

use vello::kurbo::{Point, Rect};

const W: f32 = 1200.0;
const H: f32 = 760.0;

fn lay_out(app: &mut SettingsApp) {
    lay_out_at(app, W, H);
}

/// Run the production `sync → build_layout → compute_layout → apply_layout`
/// cycle on any widget at an explicit viewport. Shared with the sibling
/// geometry modules in this test binary (`alignment_review`).
pub(crate) fn lay_out_at(widget: &mut dyn Widget, w: f32, h: f32) {
    let theme = ui::Theme::dark();
    let mut doc = automerge::AutoCommit::new();
    let root = automerge::ROOT;
    widget.sync(&mut doc, &root);
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = widget.build_layout(&mut taffy, &theme, &mut node_map);
    let outer = taffy
        .new_with_children(
            taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::length(w),
                    height: taffy::Dimension::length(h),
                },
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    widget.sync(&mut doc, &root);
    let measure_map = node_map.clone();
    let mut tr = TextRenderer::new();
    taffy
        .compute_layout_with_measure(
            outer,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(w),
                height: taffy::AvailableSpace::Definite(h),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .unwrap();
    widget.apply_layout(&taffy, node, Point::ORIGIN);
    widget.sync(&mut doc, &root);
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

#[test]
fn first_account_action_and_region_field_follow_compact_heading() {
    for (category, label, limit) in [
        (Category::ExternalAccounts, "Add account", 370.0),
        (Category::RegionFormats, "Currency", 220.0),
    ] {
        let mut app = SettingsApp::with_selected(category);
        lay_out(&mut app);
        let mut targets = Vec::new();
        named(&app, label, &mut targets);
        let first = targets.first().expect("visible first control");
        assert!(
            first.y1 < limit,
            "{category:?}: first control {first:?} below {limit}"
        );
        let mut headings = Vec::new();
        named(&app, category.label(), &mut headings);
        assert!(headings.iter().any(|r| r.y1 < first.y0));
    }
}

#[test]
fn region_preview_follows_every_format_control() {
    let mut app = SettingsApp::with_selected(Category::RegionFormats);
    lay_out(&mut app);
    let mut last = Vec::new();
    named(&app, "First day of week", &mut last);
    let mut preview = Vec::new();
    named(&app, "Preview", &mut preview);
    assert!(
        last[0].y1 < preview[0].y0,
        "last {:?}, preview {:?}",
        last[0],
        preview[0]
    );
}

#[test]
fn region_layout_keeps_every_painted_widget_finite() {
    let mut app = SettingsApp::with_selected(Category::RegionFormats);
    lay_out(&mut app);
    assert_finite_rects(&app, "root");
}
