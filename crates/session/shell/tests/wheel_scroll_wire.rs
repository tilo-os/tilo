//! A wheel notch reaches the window under the pointer as one scroll step,
//! and a finger scroll ends once, when the fingers lift.
//!
//! Every wheel event used to carry `wl_pointer.axis_stop` for the axis it
//! did not move. To a toolkit that is the end of a scroll gesture: winit
//! reported each notch as an ended scroll, which a scroll view takes for a
//! fling release and ignores, so a wheel scrolled nothing in Tilo's apps.
#![allow(clippy::expect_used, reason = "test harness failures are assertions")]

use wayland_client::protocol::{wl_compositor, wl_pointer, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};
use winit::event::{MouseScrollDelta, TouchPhase};

#[derive(Debug, PartialEq)]
enum Wire {
    Source(wl_pointer::AxisSource),
    Value120(wl_pointer::Axis, i32),
    Value(wl_pointer::Axis),
    Stop(wl_pointer::Axis),
    Frame,
}

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    wire: Vec<Wire>,
}

crate::support::wayland::test_client!(Client);

fn axis(axis: WEnum<wl_pointer::Axis>) -> wl_pointer::Axis {
    axis.into_result().expect("a known axis")
}

impl Dispatch<wl_pointer::WlPointer, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let wire = match event {
            wl_pointer::Event::AxisSource { axis_source } => {
                Wire::Source(axis_source.into_result().expect("a known source"))
            }
            wl_pointer::Event::AxisValue120 { axis: a, value120 } => {
                Wire::Value120(axis(a), value120)
            }
            wl_pointer::Event::Axis { axis: a, .. } => Wire::Value(axis(a)),
            wl_pointer::Event::AxisStop { axis: a, .. } => Wire::Stop(axis(a)),
            wl_pointer::Event::Frame => Wire::Frame,
            _ => return,
        };
        state.wire.push(wire);
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_seat::WlSeat);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);

type Harness = crate::support::wayland::Harness<Client>;

impl Harness {
    /// What the client heard for one scroll event.
    fn scroll(&mut self, delta: MouseScrollDelta, phase: TouchPhase) -> Vec<Wire> {
        self.client.wire.clear();
        assert!(self.state.forward_client_pointer_axis(delta, phase));
        self.pump();
        std::mem::take(&mut self.client.wire)
    }
}

#[test]
fn a_wheel_notch_scrolls_and_a_finger_scroll_stops_once() {
    let mut h = Harness::new();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let qh = h.queue.handle();
    let _pointer = seat.get_pointer(&qh, ());
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("org.example.List".into());
    h.pump();
    let list = h.foreign_id("org.example.List");
    h.state.set_client_keyboard_focus(Some(list));
    h.state
        .forward_client_pointer_motion((400.0, 400.0), Some((list, (0.0, 0.0))));
    h.pump();

    let notch = h.scroll(MouseScrollDelta::LineDelta(0.0, -1.0), TouchPhase::Moved);
    assert_eq!(
        notch,
        vec![
            Wire::Source(wl_pointer::AxisSource::Wheel),
            Wire::Value120(wl_pointer::Axis::VerticalScroll, 120),
            Wire::Value(wl_pointer::Axis::VerticalScroll),
            Wire::Frame,
        ],
        "a notch is a step on the axis it turned, and ends nothing"
    );

    let fingers = h.scroll(
        MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, -12.0)),
        TouchPhase::Moved,
    );
    assert!(
        !fingers.iter().any(|wire| matches!(wire, Wire::Stop(_))),
        "fingers still on the pad end no axis: {fingers:?}"
    );
    let lifted = h.scroll(
        MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, 0.0)),
        TouchPhase::Ended,
    );
    assert!(
        lifted.contains(&Wire::Stop(wl_pointer::Axis::VerticalScroll))
            && lifted.contains(&Wire::Stop(wl_pointer::Axis::HorizontalScroll)),
        "lifting the fingers ends the scroll: {lifted:?}"
    );
}
