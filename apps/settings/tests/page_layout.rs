//! Every Settings page, staged as its capture is, at the laptop's own window
//! (2944×1840 at scale 2 is 1472×920 logical pixels) and at the small
//! windows a strip gives it. The pages are composed from shared rows; a row
//! whose controls wrap past its own bounds, or two controls drawn over each
//! other, is what made pages read as broken. These checks read the
//! production widget tree after the real layout pass the window runs.

use settings::SettingsShell;
use settings::categories::Category;
use ui::framework::testing::TestApp;
use ui::widget::Widget;
use vello::kurbo::Rect;

/// The laptop's panel at its scale, a short strip window, a phone-width
/// column and the smallest window a strip tile leaves.
const SIZES: [(u32, u32); 4] = [(1472, 920), (640, 400), (440, 820), (400, 600)];

/// Containers that lay their parts out side by side or one above the other
/// (flex rows and columns, a form's sections, its rows and their bodies):
/// what they hold stays inside them and never lies on a sibling.
fn is_flex(name: &str) -> bool {
    matches!(
        name,
        "Row" | "Column" | "FormSection" | "Field" | "FieldBody"
    )
}

fn drawn(rect: Rect) -> bool {
    rect.width() > 0.5 && rect.height() > 0.5
}

fn describe(widget: &dyn Widget) -> String {
    let label = widget
        .accessibility()
        .and_then(|node| node.name)
        .unwrap_or_default();
    format!("{} {:?} {:?}", widget.name(), label, widget.get_rect())
}

fn walk(widget: &dyn Widget, width: f64, problems: &mut Vec<String>) {
    let rect = widget.get_rect();
    let children = widget.children();
    // What is drawn: a container's rect also spans its content's extent
    // (`flow_rects`), so the window edge is checked on the leaves, which
    // are the text, controls and pictures a person sees.
    if children.is_empty() && drawn(rect) && rect.x1 > width + 1.0 {
        problems.push(format!(
            "past the window's right edge: {}",
            describe(widget)
        ));
    }
    if is_flex(widget.name()) && drawn(rect) {
        let outer = rect.inflate(1.0, 1.0);
        let placed: Vec<_> = children
            .iter()
            .filter(|child| drawn(child.get_rect()))
            .collect();
        for child in &placed {
            let inner = child.get_rect();
            if outer.intersect(inner) != inner {
                problems.push(format!(
                    "outside its {}: {} in {:?}",
                    widget.name(),
                    describe(**child),
                    rect
                ));
            }
        }
        for (index, a) in placed.iter().enumerate() {
            for b in placed.iter().skip(index.saturating_add(1)) {
                let overlap = a.get_rect().intersect(b.get_rect());
                if overlap.width() > 1.0 && overlap.height() > 1.0 {
                    problems.push(format!(
                        "drawn over each other: {} and {}",
                        describe(**a),
                        describe(**b)
                    ));
                }
            }
        }
    }
    for child in children {
        walk(child, width, problems);
    }
}

#[test]
fn every_page_keeps_its_controls_inside_their_rows_at_the_laptop_size_and_in_small_windows() {
    let mut failures = Vec::new();
    for &(width, height) in &SIZES {
        for &category in Category::ALL {
            let slug = category.slug();
            let mut app = TestApp::<SettingsShell>::capture_at(Some(slug), (width, height))
                .unwrap_or_else(|error| panic!("{slug} at {width}x{height} opens: {error:?}"));
            app.settle();
            let problems = app
                .surface::<settings::SettingsApp, _>(|settings| {
                    let mut problems = Vec::new();
                    walk(&*settings, f64::from(width), &mut problems);
                    problems
                })
                .expect("the settings surface is mounted");
            if !problems.is_empty() {
                failures.push(format!(
                    "{slug} at {width}x{height}:\n  {}",
                    problems.join("\n  ")
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
