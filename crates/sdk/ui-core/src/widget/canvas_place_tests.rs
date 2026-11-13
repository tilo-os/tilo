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

/// Whether a menu stands open under `widget`: an open menu is among the children its area
/// shows.
fn menu_open(widget: &dyn Widget) -> bool {
    widget.name() == "ContextMenu" || widget.children().into_iter().any(menu_open)
}

/// A right-click on a place opens its resource's actions as its menu (`canvas.md` §1, "Right-click
/// keeps its menu"): the same commands its handle's menu lists.
#[test]
fn a_right_click_on_a_place_opens_its_actions() {
    let place = CanvasPlace::from_props(
        "p",
        PlaceProps {
            name: Some("Inbox".into()),
            carries: Some("tl:mail:root".into()),
            controls: None,
            body: Some(Box::new(crate::widget::Spacer::fixed(300.0))),
            items: vec![MenuItemProps {
                label: "Archive".into(),
                ..MenuItemProps::default()
            }],
            ..PlaceProps::default()
        },
    );
    let mut place = place;
    let mut text = crate::font_catalog::fixture_renderer();
    crate::widget::layout::lay_out_detached(
        &mut place,
        Rect::new(0.0, 0.0, 400.0, 300.0),
        &crate::Theme::dark(),
        &mut text,
    )
    .expect("the place lays out");
    let mut host = crate::headless::FrameHarness::for_root(
        crate::State::new(),
        place,
        (400, 300),
        crate::Theme::dark(),
    );
    host.relayout();
    host.send(&crate::synthetic_input::cursor_moved(200.0, 150.0));
    host.send(&crate::synthetic_input::mouse_input(
        winit::event::ElementState::Pressed,
        winit::event::MouseButton::Right,
    ));
    assert!(
        menu_open(host.root_widget()),
        "the right-click opened the place's menu of its actions"
    );
}
