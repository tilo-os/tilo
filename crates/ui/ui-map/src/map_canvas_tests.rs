//! How the canvas feels to point at (`docs/designs/app-language/canvas.md` §7, "The bar it is
//! held to"): free space is a text cursor and says once what it is for, an answer hugs its words
//! up to the room it was asked in, and what arrives grows out of where the person pointed.
use super::test_support::*;

use super::*;
use tilo_ui_base::{component::Fired, component::Intent};

/// Over the free canvas the pointer is a text cursor, since a click there puts a caret to write
/// at; over a place, what the place holds says, and outside the map the map says nothing.
#[test]
fn free_space_shows_a_text_cursor() {
    let queue = tilo_ui_base::Intents::<Fired>::default();
    let mut map = Map::new().pin(
        "note",
        place(100.0, 100.0, 200.0, 150.0, false),
        Box::new(tilo_ui_base::widget::Spacer::new()),
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
        Box::new(tilo_ui_base::widget::Spacer::new()),
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
        || -> Element { Box::new(tilo_ui::widget::Text::new("Two tasks are due this week.")) };
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
        Box::new(tilo_ui::widget::Text::new(&long)),
    );
    laid(&mut tall);
    let answer = tall.laid.borrow()["answer"];
    assert!(
        (answer.height() - 200.0).abs() < 0.5,
        "an answer longer than its room stands at the room's height: {answer:?}"
    );
}

/// What arrives grows out of where it was asked: on its first frame it stands at the point the
/// person pointed at, half its size and unseen; on its last it stands at its place, whole. With
/// reduced motion it only fades in where it stands.
#[test]
fn an_answer_grows_out_of_where_it_was_asked() {
    let rect = Rect::new(400.0, 300.0, 800.0, 500.0);
    let from = Point::new(100.0, 80.0);
    let motion = tilo_ui_base::tokens::EffectiveMotion::default();
    let start = tilo_ui_base::motion::WindowMotion::grown_pose(0.0, rect, from, motion);
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
    let halfway = tilo_ui_base::motion::WindowMotion::grown_pose(0.5, rect, from, motion);
    let mid = halfway.drawn_rect(rect);
    assert!(
        (from.x..rect.x0).contains(&mid.x0),
        "on its way from the caret to its place: {mid:?}"
    );
    assert!(
        mid.width() > drawn.width() && mid.width() < rect.width(),
        "growing on its way: {mid:?}"
    );
    let end = tilo_ui_base::motion::WindowMotion::grown_pose(1.0, rect, from, motion);
    assert!(end.is_rest(), "it rests at its place");
    let reduced =
        tilo_ui_base::tokens::EffectiveMotion::new(tilo_ui_base::tokens::Motion::default(), true);
    let still = tilo_ui_base::motion::WindowMotion::grown_pose(0.3, rect, from, reduced);
    assert_eq!(
        still.transform,
        Affine::IDENTITY,
        "reduced motion: it fades in where it stands"
    );
}

/// A view that says no height of its own (a list that scrolls measures as nothing) cannot hug:
/// its place fills its most rather than as a sliver.
#[test]
fn a_view_with_no_height_of_its_own_fills_its_most() {
    let mut map = Map::new().pin(
        "list",
        place(0.0, 0.0, 400.0, 300.0, true),
        Box::new(tilo_ui_base::widget::Spacer::new()),
    );
    laid(&mut map);
    let list = map.laid.borrow()["list"];
    assert!(
        (list.height() - 300.0).abs() < 0.5,
        "it stands at its most: {list:?}"
    );
}

/// A map of the world draws back until the whole world fits, and its pins keep the size the
/// view gave them at every zoom, standing centred on the places they name.
#[test]
fn a_world_map_draws_back_to_the_world_with_its_pins_screen_sized() {
    let mut map = Map::from_props(
        "places",
        MapProps {
            name: Some("Places".into()),
            world: true,
            latitude: Some(52.52),
            longitude: Some(13.405),
            pins: vec![MapPinProps {
                key: "photo".into(),
                name: Some("Photo".into()),
                latitude: Some(48.8566),
                longitude: Some(2.3522),
                width: Some(96),
                height: Some(96),
                ..MapPinProps::default()
            }],
            ..MapProps::default()
        },
    );
    laid(&mut map);
    assert!(
        (map.camera.zoom - MAP_ZOOM_WORLD_MIN).abs() < 1e-12,
        "a map of the world opens showing the whole world: {}",
        map.camera.zoom
    );
    let drawn = map.laid.borrow()["photo"];
    assert!(
        (drawn.width() - 96.0).abs() < 0.5 && (drawn.height() - 96.0).abs() < 0.5,
        "a world pin keeps the size its view gave it on screen, not the camera's scale: {drawn:?}"
    );
    let at = mercator_project(48.8566, 2.3522);
    let on_screen = map.shown.get().to_screen(Point::ORIGIN, at);
    assert!(
        (drawn.center().x - on_screen.x).abs() < 0.5
            && (drawn.center().y - on_screen.y).abs() < 0.5,
        "its middle stands on its place: {drawn:?} against {on_screen:?}"
    );
}

/// A window whose place just left stands at its last spot and takes nothing: a press on it points
/// at no place, paints no caret under it and sizes nothing, so the actions of a place that has
/// left the layout are never reachable while its frame plays its departure.
#[test]
fn a_leaving_place_takes_no_press_and_sizes_nothing() {
    let pins = |leaving: bool| {
        let mut map = Map::from_props(
            "canvas",
            MapProps {
                pins: vec![MapPinProps {
                    key: "gone".into(),
                    name: Some("A note".into()),
                    x: Some(100),
                    y: Some(100),
                    width: Some(200),
                    height: Some(150),
                    leaving,
                    item: Some("gone".into()),
                    body: vec![Box::new(tilo_ui_base::widget::Spacer::new())],
                    ..MapPinProps::default()
                }],
                ..MapProps::default()
            },
        );
        laid(&mut map);
        map
    };
    // The place's own middle and its trailing edge, in the map's own coordinates.
    let (middle, edge) = (Point::new(200.0, 170.0), Point::new(300.0, 170.0));

    let leaving = pins(true);
    assert!(
        leaving.leaving_pin_under(middle),
        "the place still stands where it stood"
    );
    assert_eq!(
        leaving.pin_under(middle),
        None,
        "a press on it points at no place"
    );
    assert_eq!(
        leaving.pin_under(edge),
        None,
        "and its edge is no handle to size"
    );
    assert_eq!(
        leaving.edge_at(edge),
        None,
        "so dragging there sizes nothing"
    );
    assert_eq!(
        leaving.cursor_shape(middle),
        None,
        "and the canvas under it takes no caret"
    );

    let standing = pins(false);
    assert_eq!(
        standing.pin_under(middle).as_deref(),
        Some("gone"),
        "a window that stands is the place a press points at"
    );
    assert!(
        standing.edge_at(edge).is_some(),
        "and its edge is its handle"
    );
}
