use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use taffy::Size;
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::*;
use tilo_ui::kurbo::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::internals::scrollable::test_support::*;
use tilo_ui::widget::internals::scrollable::*;
use tilo_ui::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::MeasureConstraint;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;
/// A place on a canvas that hugs what it holds, up to a cap: the region
/// measures the scroll view at its natural height (a probe solve, in a copy of
/// the tree) and places it no taller than the cap. Words longer than the cap
/// overflow the real box, so its bar keeps its gutter there; in the probe the
/// view is as tall as its words and needs none. Both solves run the same
/// widget, and the gutter one tree decided must not be taken for the other's,
/// or the window settles on no frame at all.
#[test]
fn a_view_measured_at_its_natural_height_and_placed_at_a_cap_settles_with_its_gutter() {
    use tilo_ui_base::{widget::Manual, widget::MeasureConstraint};
    const CAP: f64 = 120.0;
    let mut words = Column::new().with_intrinsic_height();
    for _ in 0..8 {
        words = words.push(Box::new(Text::new(
            "A bulleted answer long enough to wrap over a few lines in a turn's card, \
             so the card is taller than the room the canvas gives it.",
        )));
    }
    let region = Manual::new(|cx| {
        let width = 520.0_f32;
        let natural = cx
            .measure_child(
                0,
                Size {
                    width: tilo_ui_base::widget::MeasureConstraint::Exact(width),
                    height: MeasureConstraint::Intrinsic,
                },
            )?
            .size
            .height;
        let height = f64::from(natural).min(CAP);
        cx.place(
            0,
            vello::kurbo::Rect::from_origin_size((10.0, 10.0), (f64::from(width), height)),
        )?;
        Ok(cx.available())
    })
    .filling()
    .child(Box::new(
        Scrollable::new_vertical().with_child(Box::new(words)),
    ));
    let mut harness = tilo_ui::app::driver::Driver::new_with_theme(
        tilo_ui::State::new(),
        Box::new(region),
        (1280, 800),
        Theme::dark(),
    );
    harness.relayout();
    assert_eq!(harness.take_skip_report(), None, "every frame lays out");
    harness.recompose().expect("a frame after settles too");
    let (gutter, height) = harness
        .with_widget_mut(|scroll: &mut Scrollable| (scroll.reserve_vertical, scroll.rect.height()))
        .unwrap();
    assert!(
        (height - CAP).abs() < 1.0,
        "the view stands at the cap: {height}"
    );
    assert!(
        gutter,
        "the words overflow the cap, so the bar keeps its gutter"
    );
}
