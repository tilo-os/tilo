//! Pointing on a map (`docs/designs/app-language/canvas.md` §6, "Pointing"): a click on free
//! space reports the canvas point under it, a drag across free space reports the rectangle it
//! covered, and a right press reports the place under it and the canvas point. All of them are
//! in canvas pixels at the camera the map looks with, so each is right at any pan or zoom. A
//! press on a place is the place's, never the map's, and a middle drag pans.
use super::test_support::*;

use super::*;
use tilo_ui_base::component::Intent;

/// The rectangle a view points at is drawn as the frame's outline in the theme's focus colour,
/// at the camera's pan and zoom, and a map pointing at none draws no outline.
#[test]
fn a_pointed_rectangle_is_outlined_where_it_stands() {
    let theme = tilo_ui_base::Theme::dark();
    let render = |outline: Vec<i64>| -> Vec<u8> {
        let queue = tilo_ui_base::Intents::default();
        let mut map = pointing_map(&queue, MapCamera::default());
        map.outline = outline_of(&outline);
        let mut text = tilo_ui_base::font_catalog::fixture_renderer();
        tilo_ui_base::widget::layout::lay_out_detached(
            &mut map,
            Rect::new(0.0, 0.0, 400.0, 300.0),
            &theme,
            &mut text,
        )
        .expect("the map lays out");
        let mut scene = Scene::new();
        Widget::draw(
            &mut map,
            &mut tilo_ui_base::draw::DrawCtx::new(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                tilo_ui_base::presence::PresenceCtx::empty().draw_env(),
            ),
        );
        tilo_ui_base::offscreen::OffscreenRenderer::new(400, 300)
            .render(&scene, vello::peniko::Color::BLACK)
    };
    let plain = render(Vec::new());
    let outlined = render(vec![100, 80, 120, 90]);
    let at = |rgba: &[u8], x: u32, y: u32| {
        tilo_ui_base::offscreen::OffscreenRenderer::sample(rgba, 400, x, y)
    };
    assert_ne!(
        at(&outlined, 160, 80),
        at(&plain, 160, 80),
        "the outline's top edge is drawn where the rectangle stands"
    );
    assert_eq!(
        at(&outlined, 160, 125),
        at(&plain, 160, 125),
        "inside the rectangle stays the canvas"
    );
}

/// A hugging pin reports the height it is drawn at, in canvas pixels, once: a view that reads the
/// canvas's free area learns what the pin takes without a report a frame, and hears of it again
/// only when its view comes to another height.
#[test]
fn a_hugging_pin_reports_the_height_it_is_drawn_at_once() {
    let _frame = tilo_ui_base::motion::pin_frame();
    let queue = tilo_ui_base::Intents::default();
    let mut map = pointing_map(&queue, MapCamera::default());
    map.drawn = Event::View(Intent::new(&queue, CANVAS, "drawn"));
    // A pin that hugs its words: as tall as they come to, up to its own height.
    map = map.pin(
        "turn",
        PinPlace {
            x: 0.0,
            y: 0.0,
            width: 400.0,
            height: Some(480.0),
            band: 0.0,
            hug: true,
            grows: true,
        },
        Box::new(crate::widget::Text::new("Here are your mails.")),
    );
    lay_out(&mut map);
    // The first solve reports the height the words come to, once.
    let first = reported(&queue);
    let height = drawn_height(&first);
    assert!(
        height > 0 && height < 480,
        "the turn hugs its words, under its own height: {height}"
    );
    // A frame that changes nothing reports nothing: the map rests.
    lay_out(&mut map);
    assert_eq!(reported(&queue), Vec::new(), "a still canvas says nothing");
    // The pin's view comes to another height: it is reported once more, taller.
    map.put_pin_view(
        0,
        Box::new(crate::widget::Text::new(
            "Here are your mails. This is a longer answer, so the card it stands in is taller \
             than the last one, and the height it is drawn at changes with it.",
        )),
    );
    lay_out(&mut map);
    let again = reported(&queue);
    let taller = drawn_height(&again);
    assert!(
        taller > height,
        "the taller answer is drawn taller: {taller} was {height}, {again:?}"
    );
    // And the frame after it says nothing, the change reported once.
    lay_out(&mut map);
    assert_eq!(reported(&queue), Vec::new(), "the change was reported once");
}
