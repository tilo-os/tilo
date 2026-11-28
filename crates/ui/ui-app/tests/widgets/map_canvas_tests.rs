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
use tilo_ui_map::test_support::*;
use tilo_ui_map::*;
use winit::event::*;
use winit::keyboard::*;
/// Where the person points is what an arriving answer grows out of: the click's canvas point, or
/// the dragged rectangle's corner.
#[test]
fn pointing_marks_where_an_answer_grows_from() {
    let queue = tilo_ui_base::Intents::<Fired>::default();
    let mut map = Map::new();
    map.press = Event::View(Intent::new(&queue, CANVAS, "press"));
    map.frame = Event::View(Intent::new(&queue, CANVAS, "frame"));
    map.set_camera(MapCamera {
        origin: Point::new(100.0, 50.0),
        zoom: 2.0,
    });
    let mut host = tilo_ui_app::app::driver::Driver::for_root(
        tilo_ui_app::State::new(),
        map,
        (800, 600),
        Theme::dark(),
    );
    host.relayout();
    host.send(&tilo_ui_base::synthetic_input::cursor_moved(300.0, 200.0));
    for state in [ElementState::Pressed, ElementState::Released] {
        host.send(&tilo_ui_base::synthetic_input::mouse_input(
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
