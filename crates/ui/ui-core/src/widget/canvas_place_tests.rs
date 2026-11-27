//! A place on a canvas fills the room its pin gives it: its ground under the band its handle
//! shows in.

use super::*;
use crate::widget::{Map, PinPlace};

/// The place's ground, found under `widget` by its name.
fn ground_rect(widget: &dyn Widget) -> Option<Rect> {
    if widget.name() == "PlaceGround" {
        return Some(widget.get_rect());
    }
    widget.children().into_iter().find_map(ground_rect)
}

/// A place put at (40, 30), 400 by 300, stands its surface exactly there and that size: the band
/// its handle shows in stands over that point, outside the surface, so neither the place nor its
/// view shifts for it.
#[test]
fn a_places_surface_stands_where_it_was_put() {
    let place = CanvasPlace::from_props(
        "p",
        PlaceProps {
            name: Some("Inbox".into()),
            carries: Some("tl:mail:root".into()),
            controls: None,
            body: None,
            ..PlaceProps::default()
        },
    );
    let mut map = Map::new().pin(
        "p",
        PinPlace {
            x: 40.0,
            y: 30.0,
            width: 400.0,
            height: Some(300.0),
            band: PLACE_BAND,
            hug: false,
            grows: false,
        },
        Box::new(place),
    );
    let mut text = tilo_ui_base::font_catalog::fixture_renderer();
    tilo_ui_base::widget::layout::lay_out_detached(
        &mut map,
        Rect::new(0.0, 0.0, 800.0, 600.0),
        &tilo_ui_base::Theme::dark(),
        &mut text,
    )
    .expect("the map lays out");
    let ground = ground_rect(&map).expect("the place's ground");
    let handle = named_rect(&map, "HoverToolbarFace").expect("the place's handle");
    assert!(
        (ground.x0 - 40.0).abs() < 1.0
            && (ground.y0 - 30.0).abs() < 1.0
            && (ground.width() - 400.0).abs() < 1.0
            && (ground.height() - 300.0).abs() < 1.0,
        "the surface stands where the place was put, at its size: {ground:?}"
    );
    assert!(
        handle.y0 >= 30.0 - PLACE_BAND - 0.5 && handle.y1 <= 30.0 + 0.5 && handle.x1 <= 440.0 + 0.5,
        "the handle shows in the band over the surface: it covers no line of the view, and \
         showing it moves nothing: {handle:?} over {ground:?}"
    );
}

/// The box of the first widget under `widget` named `name`.
fn named_rect(widget: &dyn Widget, name: &str) -> Option<Rect> {
    if widget.name() == name {
        return Some(widget.get_rect());
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| named_rect(child, name))
}

/// A place says what it is at rest, quietly: its title stands at the leading end of the band over
/// its surface whether or not its handle shows, so nothing moves when the controls come on hover.
#[test]
fn a_place_says_what_it_is_at_rest() {
    let place = CanvasPlace::from_props(
        "p",
        PlaceProps {
            name: Some("Inbox".into()),
            carries: Some("tl:mail:root".into()),
            ..PlaceProps::default()
        },
    );
    let mut map = Map::new().pin(
        "p",
        PinPlace {
            x: 40.0,
            y: 60.0,
            width: 400.0,
            height: Some(300.0),
            band: PLACE_BAND,
            hug: false,
            grows: false,
        },
        Box::new(place),
    );
    let mut text = tilo_ui_base::font_catalog::fixture_renderer();
    tilo_ui_base::widget::layout::lay_out_detached(
        &mut map,
        Rect::new(0.0, 0.0, 800.0, 600.0),
        &tilo_ui_base::Theme::dark(),
        &mut text,
    )
    .expect("the map lays out");
    let caption = text_rect(&map, "Inbox").expect("the place's caption");
    assert!(
        caption.height() > 0.0
            && caption.y0 >= 60.0 - PLACE_BAND - 0.5
            && caption.y1 <= 60.0 + 0.5
            && caption.x0 < 60.0,
        "the caption stands at the band's leading end, over the surface: {caption:?}"
    );
}

/// The box of the first text under `widget` that says `said`.
fn text_rect(widget: &dyn Widget, said: &str) -> Option<Rect> {
    if widget.name() == "Text"
        && widget
            .accessibility()
            .and_then(|node| node.name)
            .is_some_and(|name| name == said)
    {
        return Some(widget.get_rect());
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| text_rect(child, said))
}

/// A place's caption longer than its width — a turn's caption is the person's whole question —
/// wraps and is said in full, a quiet line over the surface: never cut short with an ellipsis.
/// The band grows to hold it, so the caption still stands wholly over the surface.
#[test]
fn a_long_caption_wraps_and_is_said_in_full() {
    let question = "remind me to call mum tomorrow at 6pm and tell her about the trip";
    let laid = |name: &str| {
        let place = CanvasPlace::from_props(
            "p",
            PlaceProps {
                name: Some(name.into()),
                carries: Some("chat.workspace.turns:q1".into()),
                ..PlaceProps::default()
            },
        );
        let mut map = Map::new().pin(
            "p",
            PinPlace {
                x: 40.0,
                y: 100.0,
                width: 260.0,
                height: Some(200.0),
                band: PLACE_BAND,
                hug: false,
                grows: false,
            },
            Box::new(place),
        );
        let mut text = tilo_ui_base::font_catalog::fixture_renderer();
        tilo_ui_base::widget::layout::lay_out_detached(
            &mut map,
            Rect::new(0.0, 0.0, 800.0, 600.0),
            &tilo_ui_base::Theme::dark(),
            &mut text,
        )
        .expect("the map lays out");
        let caption = text_rect(&map, name).expect("the place's caption");
        let ground = ground_rect(&map).expect("the place's ground");
        (caption, ground)
    };
    let (line, _) = laid("hi");
    let (caption, ground) = laid(question);
    assert!(
        caption.height() >= 2.0 * line.height() - 0.5,
        "the caption wraps to as many lines as it needs: {caption:?}, one line is {line:?}"
    );
    assert!(
        caption.y0 >= 100.0 - PLACE_BAND - 0.5 && caption.y1 <= ground.y0 + 0.5,
        "it stands over the surface, the band grown to hold it: {caption:?} over {ground:?}"
    );
    assert!(
        caption.x1 <= 300.0 + 0.5,
        "within the place's width: {caption:?}"
    );
}
