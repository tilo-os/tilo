//! A map under the display-less driver, as the map's tests press and read it.

use tilo_ui_app::headless::Driver;
use tilo_ui_app::kurbo::{Point, Rect};
use tilo_ui_base::component::Fired;
use tilo_ui_base::theme::Theme;
use tilo_ui_map::Map;
use tilo_ui_map::MapCamera;
use tilo_ui_map::test_support::pointing_map;
use winit::event::{ElementState, MouseButton, WindowEvent};

/// A map laid out over the whole viewport, with its camera as given: the camera is the map's
/// own, kept across the layout, as a live frame leaves it.
pub fn laid_out(mut map: Map) -> Driver {
    let mut text = tilo_ui_base::font_catalog::fixture_renderer();
    tilo_ui_base::widget::layout::lay_out_detached(
        &mut map,
        Rect::new(0.0, 0.0, 800.0, 600.0),
        &Theme::dark(),
        &mut text,
    )
    .expect("the map lays out");
    Driver::for_root(tilo_ui_app::State::new(), map, (800, 600), Theme::dark())
}

/// [`laid_out`] of a map with no places, looking with `camera`.
pub fn harness(queue: &tilo_ui_base::Intents<Fired>, camera: MapCamera) -> Driver {
    laid_out(pointing_map(queue, camera))
}

/// Press the map's right button at `at`, with the pointer there.
pub fn right_click(host: &mut Driver, at: Point) {
    let device = winit::event::DeviceId::dummy();
    host.dispatch_to_root(&WindowEvent::CursorMoved {
        device_id: device,
        position: winit::dpi::PhysicalPosition::new(at.x, at.y),
    });
    for state in [ElementState::Pressed, ElementState::Released] {
        host.dispatch_to_root(&WindowEvent::MouseInput {
            device_id: device,
            state,
            button: MouseButton::Right,
        });
    }
}

/// The camera the driven map looks with now.
pub fn map_camera(host: &mut Driver) -> MapCamera {
    host.root_widget_mut()
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<Map>())
        .expect("the root is the map")
        .camera()
}
