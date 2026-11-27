use crate::support::map::*;
use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::internals::map::test_support::*;
use tilo_ui_app::widget::internals::map::*;
use tilo_ui_app::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::Event;
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
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;
/// A turn's steps unfold where they are: pressing the quiet line under an answer that hugs its
/// words grows the place by the steps it lists. The canvas measures the place in a copy of the
/// window's tree before it stands it, and the row's body must open in the window's own tree too,
/// not only in the copy, or the line turns open over nothing.
#[test]
fn unfolding_a_hugging_answers_steps_grows_it_by_them() {
    let _frame = tilo_ui_base::motion::pin_frame();
    let _settled = tilo_ui_base::motion::testing::settled();
    // The row as a view describes it (Chat's turn): its steps are its body's views.
    let row =
        <tilo_ui_app::widget::DisclosureRow as tilo_ui_base::component::Described>::from_props(
            "steps",
            tilo_ui_app::widget::DisclosureRowProps {
                title: "Looked up Notes and 5 more steps".into(),
                body: (0..6)
                    .map(|index| -> Element {
                        Box::new(tilo_ui_app::widget::Text::new(&format!(
                            "Looked up Notes {index}"
                        )))
                    })
                    .collect(),
                dense: true,
                ..tilo_ui_app::widget::DisclosureRowProps::default()
            },
        );
    let card = tilo_ui_app::widget::arrange::Stack::stack()
        .gap(tilo_ui_base::tokens::Rhythm::Related)
        .push(Box::new(tilo_ui_app::widget::Text::new(
            "Two notes changed.",
        )))
        .push(Box::new(row));
    let body: Element =
        Box::new(tilo_ui_app::widget::Scrollable::new_vertical().with_child(Box::new(card)));
    let band = tilo_ui_app::widget::internals::canvas_place::PLACE_BAND;
    let place: Element = Box::new(
        <tilo_ui_app::widget::internals::canvas_place::CanvasPlace as tilo_ui_base::component::Described>::from_props(
            "turn",
            tilo_ui_app::widget::internals::canvas_place::PlaceProps {
                name: Some("what changed?".into()),
                body: Some(body),
                ..tilo_ui_app::widget::internals::canvas_place::PlaceProps::default()
            },
        ),
    );
    let map = Map::new().pin(
        "turn",
        PinPlace {
            band,
            ..grown(100.0, 80.0, 420.0, 400.0)
        },
        place,
    );
    let mut host = tilo_ui_app::app::driver::Driver::for_root(
        tilo_ui_app::State::new(),
        map,
        (800, 600),
        theme(),
    );
    host.relayout();
    host.recompose().expect("a frame settles");
    let laid = |host: &mut tilo_ui_app::app::driver::Driver| {
        host.root_widget_mut()
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<Map>())
            .expect("the map")
            .test_laid()
            .borrow()["turn"]
    };
    let folded = laid(&mut host);
    // The person presses the steps' line, the card's last, and the frames the press asks for run
    // as the live window runs them: laid out where something moved, never built again whole.
    host.send(&tilo_ui_base::synthetic_input::cursor_moved(
        folded.x0 + 30.0,
        folded.y1 - 10.0,
    ));
    for state in [ElementState::Pressed, ElementState::Released] {
        host.send(&tilo_ui_base::synthetic_input::mouse_input(
            state,
            winit::event::MouseButton::Left,
        ));
    }
    for _ in 0..4 {
        host.idle();
    }
    let open = laid(&mut host);
    assert_eq!(
        host.with_widget_mut(|row: &mut tilo_ui_app::widget::DisclosureRow| row.is_open()),
        Some(true),
        "the press opened the steps' line"
    );
    assert!(
        open.height() > folded.height() + 6.0 * 14.0,
        "the place grows by the six steps it unfolds: {folded:?} -> {open:?}"
    );
}

/// A turn that fails grows its "Request refused" row where it stands, with no pointer over it:
/// the banner a view adds to a hugging answer arrives, and the place stands it once it has, in
/// the frames the window draws by itself. Before, it stood only once the pointer moved there.
#[test]
fn a_banner_added_to_a_hugging_answer_stands_without_the_pointer() {
    let card = tilo_ui_app::widget::arrange::Stack::stack()
        .gap(tilo_ui_base::tokens::Rhythm::Related)
        .push(Box::new(tilo_ui_app::widget::Text::new(
            "The model's answer could not be used.",
        )));
    let body: Element =
        Box::new(tilo_ui_app::widget::Scrollable::new_vertical().with_child(Box::new(card)));
    let map = Map::new().pin("turn", grown(100.0, 80.0, 420.0, 400.0), body);
    let mut harness = tilo_ui_app::app::driver::Driver::for_root(
        tilo_ui_app::State::new(),
        map,
        (800, 600),
        theme(),
    );
    harness.relayout();
    harness.recompose().expect("a frame settles");
    for _ in 0..4 {
        harness.idle();
    }
    let laid = |harness: &mut tilo_ui_app::app::driver::Driver| {
        harness
            .root_widget_mut()
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<Map>())
            .expect("the map")
            .test_laid()
            .borrow()["turn"]
    };
    let before = laid(&mut harness);
    // The view's next tree holds the failure's row: the reconciler inserts it in place.
    harness
        .with_widget_mut(|scroll: &mut tilo_ui_app::widget::Scrollable| {
            let stack = scroll
                .child_mut()
                .and_then(|child| child.as_any_mut())
                .and_then(|any| any.downcast_mut::<tilo_ui_app::widget::arrange::Stack>())
                .expect("the card");
            let banner = tilo_ui_app::widget::Banner::new(
                tilo_ui_app::widget::BannerKind::Error,
                "Request refused",
            );
            stack.insert(1, tilo_ui_app::widget::arrange::Part::new(Box::new(banner)));
        })
        .expect("the scroll");
    // The window draws what it asks for, as time passes, and nothing moves the pointer.
    let banner_height = |harness: &mut tilo_ui_app::app::driver::Driver| {
        harness
            .with_widget_mut(|banner: &mut tilo_ui_app::widget::Banner| banner.get_rect())
            .map(|rect| rect.height())
    };
    let deadline = tilo_ui_app::time::now() + std::time::Duration::from_millis(1500);
    loop {
        harness.idle();
        if banner_height(&mut harness).is_some_and(|height| height > 20.0) {
            break;
        }
        assert!(
            tilo_ui_app::time::now() < deadline,
            "the banner did not lay itself out without a pointer"
        );
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
    let banner = harness
        .with_widget_mut(|banner: &mut tilo_ui_app::widget::Banner| banner.get_rect())
        .expect("the banner");
    let after = laid(&mut harness);
    assert!(
        banner.height() > 20.0,
        "the banner stands in the card at its height: {banner:?}"
    );
    assert!(
        after.height() > before.height() + 20.0,
        "the place grows by the row it holds: {before:?} -> {after:?}"
    );
}
