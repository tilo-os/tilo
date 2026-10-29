//! What a client asks the pointer to look like reaches what the compositor
//! shows: hiding it (`wl_pointer.set_cursor` with no surface), naming a shape
//! (`wp_cursor_shape_v1`) and handing over its own cursor surface. The
//! result is read from the frame builder, which is where captures and the
//! control socket's state dump read it.
#![allow(clippy::expect_used, reason = "test harness failures are assertions")]

use wayland_client::protocol::{wl_compositor, wl_pointer, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1, wp_cursor_shape_manager_v1,
};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};

use shell::compositor::client_input::sync_pointer_image;
use shell::compositor::state::State;
use shell::shell::frame::FrameBuilder;

use crate::support::wayland::{self, Toplevel, create_toplevel};

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    enter_serial: Option<u32>,
}

crate::support::wayland::test_client!(Client: registry, ping, configure, surface, toplevel, shm, buffer, seat);

impl Dispatch<wl_pointer::WlPointer, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { serial, .. } => state.enter_serial = Some(serial),
            wl_pointer::Event::Leave { .. } => state.enter_serial = None,
            _ => {}
        }
    }
}

delegate_noop!(Client: ignore wp_cursor_shape_manager_v1::WpCursorShapeManagerV1);
delegate_noop!(Client: ignore wp_cursor_shape_device_v1::WpCursorShapeDeviceV1);

type Harness = crate::support::wayland::Harness<Client>;

impl Harness {
    fn at_unit_scale() -> Self {
        Self::with_state(|event_loop, display| {
            let mut state = State::new(event_loop, display);
            // Cursor surfaces are resampled to the display scale; pin it so the
            // expected sizes do not depend on the machine's settings.
            state.scale = 1.0;
            state
        })
    }

    /// Put the pointer over the app's surface and return the enter serial a
    /// cursor request must quote.
    fn hover(&mut self, surface_id: u64) -> u32 {
        self.state
            .forward_client_pointer_motion((10.0, 10.0), Some((surface_id, (0.0, 0.0))));
        self.pump();
        self.client
            .enter_serial
            .expect("the app saw the pointer enter")
    }
}

struct App {
    compositor: wl_compositor::WlCompositor,
    pointer: wl_pointer::WlPointer,
    surface_id: u64,
    _surface: wl_surface::WlSurface,
    _toplevel: xdg_toplevel::XdgToplevel,
}

fn open_app(h: &mut Harness, app_id: &str) -> App {
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let qh = h.queue.handle();
    let pointer = seat.get_pointer(&qh, ());
    let Toplevel {
        surface,
        xdg: _,
        toplevel,
    } = create_toplevel(&compositor, &wm, &qh, app_id);
    h.pump();
    let surface_id = h.foreign_id(app_id);
    App {
        compositor,
        pointer,
        surface_id,
        _surface: surface,
        _toplevel: toplevel,
    }
}

fn shown(h: &Harness, fb: &mut FrameBuilder) -> serde_json::Value {
    let _ = sync_pointer_image(&h.state, fb, false, false);
    fb.pointer_image().describe()
}

#[test]
fn a_client_can_hide_the_pointer_over_its_surface() {
    let mut h = Harness::at_unit_scale();
    let app = open_app(&mut h, "org.example.HideCursor");
    let mut fb = FrameBuilder::new((1280, 800));
    let serial = h.hover(app.surface_id);
    assert_eq!(shown(&h, &mut fb)["kind"], "shape");

    app.pointer.set_cursor(serial, None, 0, 0);
    h.pump();
    assert_eq!(
        shown(&h, &mut fb),
        serde_json::json!({ "kind": "hidden" }),
        "a game or video player that hides the pointer must get no pointer at all"
    );

    // Leaving the client gives the pointer back to the compositor.
    h.state.forward_client_pointer_motion((10.0, 10.0), None);
    h.pump();
    assert_eq!(shown(&h, &mut fb)["shape"], "default");
}

#[test]
fn a_named_cursor_shape_is_drawn_by_the_compositor() {
    let mut h = Harness::at_unit_scale();
    let app = open_app(&mut h, "org.example.ShapeCursor");
    let mut fb = FrameBuilder::new((1280, 800));
    let manager = h.bind::<wp_cursor_shape_manager_v1::WpCursorShapeManagerV1>(
        "wp_cursor_shape_manager_v1",
        1,
    );
    let device = manager.get_pointer(&app.pointer, &h.queue.handle(), ());
    let serial = h.hover(app.surface_id);

    device.set_shape(serial, wp_cursor_shape_device_v1::Shape::Text);
    h.pump();
    assert_eq!(
        shown(&h, &mut fb),
        serde_json::json!({ "kind": "shape", "shape": "text" })
    );

    device.set_shape(serial, wp_cursor_shape_device_v1::Shape::NsResize);
    h.pump();
    assert_eq!(shown(&h, &mut fb)["shape"], "ns-resize");

    // A stale serial is not this client's pointer focus any more.
    device.set_shape(
        serial.wrapping_sub(1000),
        wp_cursor_shape_device_v1::Shape::Wait,
    );
    h.pump();
    assert_eq!(shown(&h, &mut fb)["shape"], "ns-resize");
}

#[test]
fn a_client_cursor_surface_is_shown_with_its_hotspot() {
    let mut h = Harness::at_unit_scale();
    let app = open_app(&mut h, "org.example.SurfaceCursor");
    let mut fb = FrameBuilder::new((1280, 800));
    let serial = h.hover(app.surface_id);
    let qh = h.queue.handle();

    let (width, height) = (16_i32, 16_i32);
    let buffer = h.shm_buffer(width, height, &wayland::solid(width, height, [0xff; 4]));
    let cursor = app.compositor.create_surface(&qh, ());
    app.pointer.set_cursor(serial, Some(&cursor), 3, 5);
    cursor.attach(Some(&buffer), 0, 0);
    cursor.damage_buffer(0, 0, width, height);
    cursor.commit();
    h.pump();

    let shown = shown(&h, &mut fb);
    drop(fb);
    assert_eq!(shown["kind"], "surface", "{shown}");
    assert_eq!(shown["width"], 16);
    assert_eq!(shown["hotspot"], serde_json::json!([3, 5]));
}
