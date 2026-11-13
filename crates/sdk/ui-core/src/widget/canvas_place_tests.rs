//! A place on a canvas fills the room its pin gives it: its ground is the whole place.

use super::*;
use crate::widget::{Map, PinPlace};

/// The place's ground, found under `widget` by its name.
fn ground_rect(widget: &dyn Widget) -> Option<Rect> {
    if widget.name() == "PlaceGround" {
        return Some(widget.get_rect());
    }
    widget.children().into_iter().find_map(ground_rect)
}

/// A place in a pin 400 by 300 is laid out 400 by 300: the view it holds has the place's room.
#[test]
fn a_place_fills_its_pin() {
    let place = CanvasPlace::from_props(
        "p",
        PlaceProps {
            name: Some("Inbox".into()),
            carries: Some("tl:mail:root".into()),
            controls: None,
            body: None,
        },
    );
    let mut map = Map::new().pin(
        "p",
        PinPlace {
            x: 40.0,
            y: 30.0,
            width: 400.0,
            height: Some(300.0),
        },
        Box::new(place),
    );
    let mut text = crate::font_catalog::fixture_renderer();
    crate::widget::layout::lay_out_detached(
        &mut map,
        Rect::new(0.0, 0.0, 800.0, 600.0),
        &crate::Theme::dark(),
        &mut text,
    )
    .expect("the map lays out");
    let ground = ground_rect(&map).expect("the place's ground");
    assert!(
        (ground.width() - 400.0).abs() < 1.0 && (ground.height() - 300.0).abs() < 1.0,
        "the place fills its pin: {ground:?}"
    );
}
