use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use taffy::Size;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::internals::scrollable::test_support::*;
use tilo_ui_app::widget::internals::scrollable::*;
use tilo_ui_app::widget::*;
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
    let mut harness = tilo_ui_app::app::driver::Driver::new_with_theme(
        tilo_ui_app::State::new(),
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

/// A page in a navigation shell, as Clocks shows its timers: the shell's first solve gives the
/// scroll view no width, where the wrapped page is far taller than the window, and the next
/// gives it its place, where the page fits.
#[test]
fn content_that_overflowed_before_the_view_had_its_place_keeps_no_gutter() {
    let page = || -> tilo_ui_base::widget::Element {
        let mut column = Column::new().with_intrinsic_height();
        for _ in 0..6 {
            column = column.push(Box::new(Text::new(
                "A paragraph long enough to wrap onto many lines when the view it \
                 is laid out in has no width yet, and onto one or two once it has.",
            )));
        }
        Box::new(column)
    };
    let shell = tilo_ui_nav::NavShell::new()
        .content_kind(tilo_ui_base::widget::ContentKind::Page)
        .mode(tilo_ui_nav::NavMode::Push)
        .sidebar(Box::new(tilo_ui_app::widget::NavList::new(vec![
            tilo_ui_app::widget::NavSection::untitled(
                "sections",
                ["One", "Two", "Three"]
                    .into_iter()
                    .map(|label| tilo_ui_app::widget::NavItem::new(label.to_owned(), label))
                    .collect(),
            ),
        ])))
        .content(Box::new(
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_child(page()),
        ));
    let mut harness = Driver::new_with_theme(
        tilo_ui_app::State::new(),
        Box::new(shell),
        (900, 760),
        Theme::dark(),
    );
    assert!(harness.take_skip_report().is_none());
    for width in [880, 700, 520] {
        harness
            .resize((width, 760))
            .unwrap_or_else(|error| panic!("{width} px: {error:?}"));
        let gutter = harness
            .with_widget_mut(|scroll: &mut Scrollable| scroll.reserve_vertical)
            .unwrap();
        assert!(!gutter, "{width} px: the page fits, so no gutter is kept");
    }
}
