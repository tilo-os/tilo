//! Controls of different kinds dropped into one `Row` — the shape of every
//! toolbar and form action row — share a single centre line and meet the
//! fine-pointer minimum target, under every theme the shell ships and at 2×
//! text. Measured through the headless host the desktop's inspection tooling
//! uses, so the rects here are the rects a screen reader or `tilo-control`
//! would be handed.
//!
//! A control whose centre drifts from its row-mates by more than half a
//! pixel, or that shrinks below the 32 px target, fails here before it
//! reaches an app.
//!
//! Linked into the `it` umbrella test binary (see `tests/it.rs`), which also
//! carries the test-wide lint allowances.

use std::collections::HashMap;

use tilo_ui::State;
use tilo_ui::headless::FrameHarness;
use tilo_ui::presence::PathSegment;
use tilo_ui::theme::Theme;
use tilo_ui::widget::toggle_group::Segment;
use tilo_ui::widget::{
    Button, Checkbox, Chip, Container, Dropdown, NumberInput, Row, SearchField, TextInput, Toggle,
    ToggleGroup, Widget,
};
use vello::kurbo::Rect;

/// Fine-pointer minimum target (HIG §9.6).
const MINIMUM_TARGET: f64 = 32.0;
/// Half a logical pixel: anything larger reads as a visible step.
const CENTRE_TOLERANCE: f64 = 0.5;
/// Slot given to each fluid (percent-width) control so the row measures
/// vertical alignment rather than flex negotiation: `Dropdown` asks for
/// `percent(100%)` and refuses to shrink, which alone would push every later
/// item out of the row.
const FLUID_SLOT_WIDTH: f32 = 220.0;

/// `test_id` of every control in the fixture row, in row order.
const CONTROL_IDS: [&str; 5] = ["save", "toggle", "checkbox", "dropdown", "text_input"];

fn slot(control: Box<dyn Widget>) -> Box<dyn Widget> {
    Box::new(
        Container::new()
            .with_width(FLUID_SLOT_WIDTH)
            .with_child(control),
    )
}

fn fixture_row() -> Box<dyn Widget> {
    Box::new(
        Row::new()
            .no_wrap()
            .push(Box::new(Button::new("save", "Save")))
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
            .push(Box::new(Button::new("save", "Save")))
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
fn mixed_controls_share_one_centre_line_across_themes_scales_and_resize() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = tilo_ui::motion::testing::settled();
    // Case `mixed_controls_in_one_row_share_a_centre_line_in_every_theme`.
    {
        for (name, theme) in [
            ("dark", Theme::dark()),
            ("light", Theme::light()),
            ("high-contrast", Theme::high_contrast()),
        ] {
            let harness =
                FrameHarness::new_with_theme(State::new(), fixture_row(), (1000, 200), theme);
            assert_row_is_aligned(name, &harness);
        }
    }

    // Case `mixed_controls_keep_one_centre_line_at_two_x_text`.
    {
        let harness = FrameHarness::new_with_theme(
            State::new(),
            fixture_row(),
            (1200, 240),
            Theme::dark().with_text_scale(2.0),
        );
        assert_row_is_aligned("dark @ 2× text", &harness);
    }

    // Case `every_control_kind_in_one_row_shares_the_centre_line_at_one_and_two_x`.
    {
        for (name, theme, viewport) in [
            ("dark", Theme::dark(), (1200, 200)),
            ("light", Theme::light(), (1200, 200)),
            (
                "dark @ 2× text",
                Theme::dark().with_text_scale(2.0),
                (1800, 260),
            ),
        ] {
            let harness = FrameHarness::new_with_theme(State::new(), dense_row(), viewport, theme);
            let rects = rects_for(&DENSE_IDS, &harness.targets());
            let centre = rects[0].1.center().y;
            for (id, rect) in &rects {
                assert!(
                    rect.width() > 0.0 && rect.height() > 0.0,
                    "{name}: {id} has no geometry: {rect:?}"
                );
                assert!(
                    (rect.center().y - centre).abs() <= CENTRE_TOLERANCE,
                    "{name}: {id} centre-y {} drifts from the row's {centre} (rect {rect:?})",
                    rect.center().y
                );
                if *id != "Row[0]/Chip" {
                    assert!(
                        rect.height() + 1e-6 >= MINIMUM_TARGET
                            && rect.width() + 1e-6 >= MINIMUM_TARGET,
                        "{name}: {id} {}×{} is under the {MINIMUM_TARGET} px target",
                        rect.width(),
                        rect.height()
                    );
                }
            }
            for pair in rects.windows(2) {
                let (left, right) = (pair[0], pair[1]);
                assert!(
                    left.1.x1 <= right.1.x0 + 1e-6,
                    "{name}: {} (x1 {}) overlaps {} (x0 {})",
                    left.0,
                    left.1.x1,
                    right.0,
                    right.1.x0
                );
            }
            let (_, field) = rects[6];
            assert!(
                field.x1 <= f64::from(viewport.0) + 1e-6,
                "{name}: the fluid field must end inside the viewport, got x1 {}",
                field.x1
            );
        }
    }

    // Case `mixed_controls_keep_one_centre_line_after_a_resize`.
    {
        let mut harness = FrameHarness::new(State::new(), fixture_row(), (1000, 200));
        harness.resize((1400, 320)).expect("resize settles");
        assert_row_is_aligned("dark after resize", &harness);
    }
}

#[test]
fn fixed_controls_keep_their_size_and_labels_beside_fluid_fields() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = tilo_ui::motion::testing::settled();
    // Case `fixed_controls_keep_their_size_beside_a_fluid_field`.
    // Fixed controls beside a fluid field laid directly in the row: the field
    // takes the remaining width and every fixed control keeps its target size
    // and the shared centre line. Fails before the Toggle pinned `flex_shrink: 0`
    // (its rect came back `x0 == x1`).
    {
        for (name, theme) in [
            ("dark", Theme::dark()),
            ("dark @ 2× text", Theme::dark().with_text_scale(2.0)),
        ] {
            let viewport = (900, 200);
            let harness =
                FrameHarness::new_with_theme(State::new(), toolbar_row(), viewport, theme);
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

    // Case `default_buttons_are_regular_controls_that_grow_with_their_label`.
    // A default `Button::new` is a Regular control: 32 px tall at 1×, as wide
    // as its label (at least twice its height). At 2× text the label's line
    // no longer fits, so the face follows the line, and both buttons in the
    // row agree on the new height — measured on the production layout path.
    {
        let at = |scale: f32| {
            let harness = FrameHarness::new_with_theme(
                State::new(),
                dense_row(),
                (1800, 260),
                Theme::dark().with_text_scale(scale),
            );
            let rects = rects_for(&["save", "delete"], &harness.targets());
            (rects[0].1, rects[1].1)
        };
        let (save, delete) = at(1.0);
        assert_eq!(save.height(), 32.0, "1× default: {save:?}");
        assert_eq!(delete.height(), 32.0, "1× destructive: {delete:?}");
        assert!(save.width() >= 64.0 && delete.width() >= 64.0);

        let (save, delete) = at(2.0);
        let scaled_line = 13.0 * 2.0 * 1.3;
        assert!(
            save.height() > scaled_line && save.height() > 32.0,
            "2× default height {} must exceed the {scaled_line} px label line",
            save.height()
        );
        assert_eq!(
            save.height(),
            delete.height(),
            "both default buttons share one height"
        );
    }
}

/// Round two (2026-09-17): the control kinds the first fixture did not hold —
/// an outlined danger button, a chip, a number stepper, a segmented group and
/// a search field — beside the default button and a fluid field. A chip is a
/// 28 px selector by design (HIG §9.6), so this row checks the centre line and
/// overlap for every item and the 32 px target for everything but the chip,
/// whose pointer target is its grown `hit_rect` (covered in `chip::tests`).
const DENSE_IDS: [&str; 7] = [
    "save",
    "delete",
    "Row[0]/Chip",
    "count",
    "view",
    "search",
    "text_input",
];

fn dense_row() -> Box<dyn Widget> {
    Box::new(
        Row::new()
            .no_wrap()
            .push(Box::new(Button::new("save", "Save")))
            .push(Box::new(
                Button::new("delete", "Delete").role(tilo_ui::widget::ButtonRole::Destructive),
            ))
            .push(Box::new(Chip::new("Filter").selectable()))
            .push(Box::new(NumberInput::new(3.0).with_id("count")))
            .push(Box::new(
                ToggleGroup::new(vec![Segment::new("List"), Segment::new("Grid")], 0)
                    .with_id("view"),
            ))
            .push(Box::new(SearchField::new().with_id("search")))
            .push(Box::new(TextInput::new(vec![PathSegment::Field(
                "query".into(),
            )]))),
    )
}
