//! Controls of different kinds dropped into one `Row` — the shape of every
//! toolbar and form action row — share a single centre line and meet the
//! fine-pointer minimum target, under every theme the shell ships and at 2×
//! text. Measured through the headless host the desktop's inspection tooling
//! uses, so the rects here are the rects a screen reader or `qos-control`
//! would be handed.
//!
//! This is the F2 (core controls) alignment covenant from the 2026-09-16 UI
//! alignment review: a control whose centre drifts from its row-mates by more
//! than half a pixel, or that shrinks below the 32 px target, fails here
//! before it reaches an app.
//!
//! Linked into the `it` umbrella test binary (see `tests/it.rs`), which also
//! carries the test-wide lint allowances.

use std::collections::HashMap;

use ui::State;
use ui::headless::FrameHarness;
use ui::presence::PathSegment;
use ui::theme::Theme;
use ui::widget::{Button, Checkbox, Container, Dropdown, Row, TextInput, Toggle, Widget};
use vello::kurbo::Rect;

/// Fine-pointer minimum target (HIG §9.6).
const MINIMUM_TARGET: f64 = 32.0;
/// Half a logical pixel: anything larger reads as a visible step.
const CENTRE_TOLERANCE: f64 = 0.5;
/// Slot given to each fluid (percent-width) control so the row measures
/// vertical alignment rather than flex negotiation: `Dropdown` asks for
/// `percent(100%)` and refuses to shrink, which alone would push every later
/// item out of the row (recorded as an open F4 finding in the F2 report).
const FLUID_SLOT_WIDTH: f32 = 220.0;

/// `test_id` of every control in the fixture row, in row order.
const CONTROL_IDS: [&str; 5] = ["save", "toggle", "checkbox", "dropdown", "text_input"];

fn slot(control: Box<dyn Widget>) -> Box<dyn Widget> {
    Box::new(Container::new().with_width(FLUID_SLOT_WIDTH).with_child(control))
}

fn fixture_row() -> Box<dyn Widget> {
    Box::new(
        Row::new()
            .no_wrap()
            .push(Box::new(Button::new("save", "Save", None)))
            .push(Box::new(Toggle::new("wifi", "wifi")))
            .push(Box::new(Checkbox::new("agree", "agree")))
            .push(slot(Box::new(Dropdown::new(
                vec!["Low".into(), "High".into()],
                "level",
            ))))
            .push(slot(Box::new(TextInput::new(vec![PathSegment::Field(
                "name".into(),
            )])))),
    )
}

/// The realistic toolbar shape: fixed controls beside one fluid text field
/// laid directly in the row, with no slot around it. This is the row in which
/// the `Toggle` used to be squeezed to zero width (it had the default shrink
/// factor and no measured minimum, so taffy took the whole overflow from it).
const TOOLBAR_IDS: [&str; 4] = ["save", "toggle", "checkbox", "text_input"];

fn toolbar_row() -> Box<dyn Widget> {
    Box::new(
        Row::new()
            .no_wrap()
            .push(Box::new(Button::new("save", "Save", None)))
            .push(Box::new(Toggle::new("wifi", "wifi")))
            .push(Box::new(Checkbox::new("agree", "agree")))
            .push(Box::new(TextInput::new(vec![PathSegment::Field(
                "query".into(),
            )]))),
    )
}

fn control_rects(targets: &HashMap<String, Rect>) -> Vec<(&'static str, Rect)> {
    rects_for(&CONTROL_IDS, targets)
}

fn rects_for(ids: &[&'static str], targets: &HashMap<String, Rect>) -> Vec<(&'static str, Rect)> {
    ids.iter()
        .map(|id| {
            let rect = targets.get(*id).copied().unwrap_or_else(|| {
                let mut keys: Vec<_> = targets.keys().cloned().collect();
                keys.sort();
                panic!("control {id:?} is not addressable; targets: {keys:?}")
            });
            (*id, rect)
        })
        .collect()
}

fn assert_row_is_aligned(label: &str, harness: &FrameHarness) {
    assert_rects_aligned(label, &control_rects(&harness.targets()));
}

fn assert_rects_aligned(label: &str, rects: &[(&'static str, Rect)]) {
    let centre = rects[0].1.center().y;
    for (id, rect) in rects {
        assert!(
            rect.width() > 0.0 && rect.height() > 0.0,
            "{label}: {id} has no geometry: {rect:?}"
        );
        assert!(
            (rect.center().y - centre).abs() <= CENTRE_TOLERANCE,
            "{label}: {id} centre-y {} drifts from the row's {centre} (rect {rect:?})",
            rect.center().y
        );
        assert!(
            rect.height() + 1e-6 >= MINIMUM_TARGET,
            "{label}: {id} is {} px tall, under the {MINIMUM_TARGET} px target",
            rect.height()
        );
        assert!(
            rect.width() + 1e-6 >= MINIMUM_TARGET,
            "{label}: {id} is {} px wide, under the {MINIMUM_TARGET} px target",
            rect.width()
        );
    }
    // Row order is preserved and nothing overlaps its neighbour.
    for pair in rects.windows(2) {
        let (left, right) = (pair[0], pair[1]);
        assert!(
            left.1.x1 <= right.1.x0 + 1e-6,
            "{label}: {} (x1 {}) overlaps {} (x0 {})",
            left.0,
            left.1.x1,
            right.0,
            right.1.x0
        );
    }
}

#[test]
fn mixed_controls_in_one_row_share_a_centre_line_in_every_theme() {
    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        let harness = FrameHarness::new_with_theme(State::new(), fixture_row(), (1000, 200), theme);
        assert_row_is_aligned(name, &harness);
    }
}

#[test]
fn mixed_controls_keep_one_centre_line_at_two_x_text() {
    let harness = FrameHarness::new_with_theme(
        State::new(),
        fixture_row(),
        (1200, 240),
        Theme::dark().with_text_scale(2.0),
    );
    assert_row_is_aligned("dark @ 2× text", &harness);
}

/// Fixed controls beside a fluid field laid directly in the row: the field
/// takes the remaining width and every fixed control keeps its target size
/// and the shared centre line. Fails before the Toggle pinned `flex_shrink: 0`
/// (its rect came back `x0 == x1`).
#[test]
fn fixed_controls_keep_their_size_beside_a_fluid_field() {
    for (name, theme) in [
        ("dark", Theme::dark()),
        ("dark @ 2× text", Theme::dark().with_text_scale(2.0)),
    ] {
        let viewport = (900, 200);
        let harness = FrameHarness::new_with_theme(State::new(), toolbar_row(), viewport, theme);
        let rects = rects_for(&TOOLBAR_IDS, &harness.targets());
        assert_rects_aligned(name, &rects);
        let (_, toggle) = rects[1];
        assert!(
            (toggle.width() - 44.0).abs() < 1e-6,
            "{name}: the toggle must keep its 44 px pill width, got {}",
            toggle.width()
        );
        let (_, field) = rects[3];
        assert!(
            field.x1 <= f64::from(viewport.0) + 1e-6,
            "{name}: the fluid field must end inside the viewport, got x1 {}",
            field.x1
        );
        assert!(
            field.width() > 200.0,
            "{name}: the fluid field should take the remaining width, got {}",
            field.width()
        );
    }
}

#[test]
fn mixed_controls_keep_one_centre_line_after_a_resize() {
    let mut harness = FrameHarness::new(State::new(), fixture_row(), (1000, 200));
    harness.resize((1400, 320)).expect("resize settles");
    assert_row_is_aligned("dark after resize", &harness);
}
