//! How the canvas feels to point at (`docs/designs/app-language/canvas.md` §7, "The bar it is
//! held to"): free space is a text cursor and says once what it is for, an answer hugs its words
//! up to the room it was asked in, and what arrives grows out of where the person pointed.

use super::*;
use crate::Theme;
use crate::component::{Fired, Intent};

/// The key the map reports its pointing under.
const CANVAS: &str = "canvas";

/// A pin at `(x, y)`, `width` wide and at most `height` tall.
fn place(x: f64, y: f64, width: f64, height: f64, hug: bool) -> PinPlace {
    PinPlace {
        x,
        y,
        width,
        height: Some(height),
        band: 0.0,
        hug,
        grows: true,
    }
}

/// `map` laid out over an 800 by 600 viewport.
fn laid(map: &mut Map) {
    let mut text = crate::font_catalog::fixture_renderer();
    crate::widget::layout::lay_out_detached(
        map,
        Rect::new(0.0, 0.0, 800.0, 600.0),
        &Theme::dark(),
        &mut text,
    )
    .expect("the map lays out");
}

/// Over the free canvas the pointer is a text cursor, since a click there puts a caret to write
/// at; over a place, what the place holds says, and outside the map the map says nothing.
#[test]
fn free_space_shows_a_text_cursor() {
    let queue = crate::Intents::<Fired>::default();
    let mut map = Map::new().pin(
        "note",
        place(100.0, 100.0, 200.0, 150.0, false),
        Box::new(crate::widget::Spacer::new()),
    );
    map.press = Event::View(Intent::new(&queue, CANVAS, "press"));
    laid(&mut map);
    assert_eq!(
        map.cursor_shape(Point::new(600.0, 400.0)),
        Some(winit::window::CursorIcon::Text),
        "free space invites writing"
    );
    assert_eq!(
        map.cursor_shape(Point::new(200.0, 170.0)),
        None,
        "over a place the place's own content says what the pointer is"
    );
    assert_eq!(map.cursor_shape(Point::new(900.0, 700.0)), None);
    // A map nobody points on (no `press`) is not a page to write on.
    let mut still = Map::new();
    laid(&mut still);
    assert_eq!(still.cursor_shape(Point::new(400.0, 300.0)), None);
}

/// The empty canvas says what it is for at its middle, and is read so to assistive technology;
/// once anything stands on it, or the person points at it, it says nothing.
#[test]
fn the_empty_canvas_says_once_what_it_is_for() {
    let hint = "Click anywhere and say what you want";
    let mut map = Map::from_props(
        "canvas",
        MapProps {
            name: Some("Canvas".into()),
            hint: Some(hint.into()),
            ..MapProps::default()
        },
    );
    laid(&mut map);
    let described = map
        .accessibility()
        .and_then(|node| node.description)
        .unwrap_or_default();
    assert_eq!(described, hint, "the empty canvas says it");
    let mut held = Map::from_props(
        "canvas",
        MapProps {
            name: Some("Canvas".into()),
            hint: Some(hint.into()),
            ..MapProps::default()
        },
    )
    .pin(
        "note",
        place(0.0, 0.0, 200.0, 100.0, false),
        Box::new(crate::widget::Spacer::new()),
    );
    laid(&mut held);
    assert!(
        held.accessibility()
            .and_then(|node| node.description)
            .is_none(),
        "a canvas with something on it says nothing"
    );
    // Told it was said (the shell's record), the map takes it back in place.
    assert!(map.set_props(MapProps {
        name: Some("Canvas".into()),
        hint: Some(String::new()),
        ..MapProps::default()
    }));
    assert!(
        map.accessibility()
            .and_then(|node| node.description)
            .is_none(),
        "said once, it is gone"
    );
}

/// An answer hugs its words up to the room it was asked in, where a place that fills its room
/// stands at its whole height: a short reply is a short card, never a tall one cut or padded.
#[test]
fn an_answer_hugs_its_words_up_to_its_room() {
    let words =
        || -> Element { Box::new(crate::widget::Text::new("Two tasks are due this week.")) };
    let mut map = Map::new()
        .pin("answer", place(0.0, 0.0, 400.0, 400.0, true), words())
        .pin("filled", place(500.0, 0.0, 200.0, 400.0, false), words());
    laid(&mut map);
    let laid_out = map.laid.borrow().clone();
    let answer = laid_out["answer"];
    let filled = laid_out["filled"];
    assert!(
        answer.height() > 1.0 && answer.height() < 100.0,
        "the answer is as tall as its line: {answer:?}"
    );
    assert!(
        (filled.height() - 400.0).abs() < 0.5,
        "a place that fills stands at its height: {filled:?}"
    );
    // Words longer than the room stand at the room's height, never past it.
    let long = "A reply that goes on. ".repeat(200);
    let mut tall = Map::new().pin(
        "answer",
        place(0.0, 0.0, 300.0, 200.0, true),
        Box::new(crate::widget::Text::new(&long)),
    );
    laid(&mut tall);
    let answer = tall.laid.borrow()["answer"];
    assert!(
        (answer.height() - 200.0).abs() < 0.5,
        "an answer longer than its room stands at the room's height: {answer:?}"
    );
}

/// Where the person points is what an arriving answer grows out of: the click's canvas point, or
/// the dragged rectangle's corner.
#[test]
fn pointing_marks_where_an_answer_grows_from() {
    let queue = crate::Intents::<Fired>::default();
    let mut map = Map::new();
    map.press = Event::View(Intent::new(&queue, CANVAS, "press"));
    map.frame = Event::View(Intent::new(&queue, CANVAS, "frame"));
    map.set_camera(MapCamera {
        origin: Point::new(100.0, 50.0),
        zoom: 2.0,
    });
    let mut host = crate::headless::FrameHarness::for_root(
        crate::State::new(),
        map,
        (800, 600),
        Theme::dark(),
    );
    host.relayout();
    host.send(&crate::synthetic_input::cursor_moved(300.0, 200.0));
    for state in [ElementState::Pressed, ElementState::Released] {
        host.send(&crate::synthetic_input::mouse_input(
            state,
            winit::event::MouseButton::Left,
        ));
    }
    let map = host
        .root_widget_mut()
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<Map>())
        .expect("the map");
    let asked = map.asked.expect("the click is where an answer grows from");
    assert!(
        (asked.x - 250.0).abs() < 0.5 && (asked.y - 150.0).abs() < 0.5,
        "the canvas point under the click, through the camera: {asked:?}"
    );
}

/// What arrives grows out of where it was asked: on its first frame it stands at the point the
/// person pointed at, half its size and unseen; on its last it stands at its place, whole. With
/// reduced motion it only fades in where it stands.
#[test]
fn an_answer_grows_out_of_where_it_was_asked() {
    let rect = Rect::new(400.0, 300.0, 800.0, 500.0);
    let from = Point::new(100.0, 80.0);
    let motion = crate::tokens::EffectiveMotion::default();
    let start = crate::motion::WindowMotion::grown_pose(0.0, rect, from, motion);
    let drawn = start.drawn_rect(rect);
    assert!(
        (drawn.x0 - from.x).abs() < 0.5 && (drawn.y0 - from.y).abs() < 0.5,
        "it starts at the point it was asked at: {drawn:?}"
    );
    assert!(
        (drawn.width() - rect.width() / 2.0).abs() < 0.5,
        "half its size: {drawn:?}"
    );
    assert!(start.opacity < 0.01, "unseen at first");
    let halfway = crate::motion::WindowMotion::grown_pose(0.5, rect, from, motion);
    let mid = halfway.drawn_rect(rect);
    assert!(
        mid.x0 > from.x
            && mid.x0 < rect.x0
            && mid.width() > drawn.width()
            && mid.width() < rect.width(),
        "on its way out of the caret: {mid:?}"
    );
    let end = crate::motion::WindowMotion::grown_pose(1.0, rect, from, motion);
    assert!(end.is_rest(), "it rests at its place");
    let reduced = crate::tokens::EffectiveMotion::new(crate::tokens::Motion::default(), true);
    let still = crate::motion::WindowMotion::grown_pose(0.3, rect, from, reduced);
    assert_eq!(
        still.transform,
        Affine::IDENTITY,
        "reduced motion: it fades in where it stands"
    );
}
