//! The sideways wheel is offered to the app's content first, and what the
//! content does not scroll with pans the shell's window strip: the gesture's
//! first event decides for all of its events, so a gesture never splits
//! between the two.

#![cfg(all(feature = "native", unix))]

use std::cell::Cell;
use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::rc::Rc;

use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, MouseScrollDelta, TouchPhase, WindowEvent};

use crate::floating_surface::{FloatingSurfaceClientMessage, FloatingSurfaceServiceClient};

use super::App;
use super::framework_history_tests::{app_with_root, layout};
use super::surface::AppSurface;

/// A window 900×650 whose root is `root`, on a shell-ui connection whose
/// shell end the test holds.
fn window(root: Box<dyn crate::widget::Widget>) -> (App, UnixStream) {
    let mut app = app_with_root(crate::State::new(), root);
    let (app_side, shell_side) = UnixStream::pair().unwrap();
    app.floating.client = Some(FloatingSurfaceServiceClient::from_stream(app_side).unwrap());
    layout(&mut app);
    // The wheel goes to what is under the pointer: the window's own content.
    app.surface_input(
        &Surface,
        crate::synthetic_input::cursor_moved(450.0, 325.0),
        false,
    );
    (app, shell_side)
}

/// A window's own surface, as the native runner passes it to every event.
struct Surface;

impl AppSurface for Surface {
    fn scale_factor(&self) -> f64 {
        1.0
    }

    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(900, 650)
    }

    fn request_redraw(&self) {}

    fn set_cursor(&self, _: winit::window::CursorIcon) {}
}

/// A sideways wheel of `dx` physical pixels, positive scrolling the content
/// toward its later columns (a wheel turned leftward).
fn sideways(dx: f64) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(dx, 0.0)),
        phase: TouchPhase::Moved,
    }
}

/// A plain wheel of `dy` physical pixels, which these windows' views do not
/// scroll with.
fn plain(dy: f64) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, dy)),
        phase: TouchPhase::Moved,
    }
}

/// What a window left to the shell's strip since the last call, summed: the
/// travel it did not scroll with, toward later windows.
fn left_to_the_strip(app: &mut App, shell: &mut UnixStream) -> f64 {
    app.pump_floating_surface_service();
    shell.set_nonblocking(true).unwrap();
    let mut text = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match shell.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => text.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("the connection broke: {error}"),
        }
    }
    let whole = text
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(&[][..], |end| &text[..end]);
    String::from_utf8(whole.to_vec())
        .expect("frames are text")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            match serde_json::from_str::<FloatingSurfaceClientMessage>(line.trim())
                .expect("a shell-ui frame")
            {
                FloatingSurfaceClientMessage::SidewaysWheel { travel } => Some(travel),
                _ => None,
            }
        })
        .sum()
}

/// A view three windows wide that scrolls sideways with the wheel, as a table
/// of many columns or an open page does.
struct Wide {
    rect: crate::kurbo::Rect,
    offset: Rc<Cell<f64>>,
}

impl Wide {
    const WIDTH: f64 = 1800.0;
}

impl crate::widget::Widget for Wide {
    fn build_layout(
        &mut self,
        taffy: &mut crate::taffy::TaffyTree<()>,
        _: &crate::Theme,
        _: &mut std::collections::HashMap<crate::taffy::NodeId, *mut dyn crate::widget::Widget>,
    ) -> crate::taffy::NodeId {
        taffy
            .new_leaf(crate::taffy::Style {
                flex_grow: 1.0,
                ..Default::default()
            })
            .unwrap()
    }

    fn apply_layout(
        &mut self,
        taffy: &crate::taffy::TaffyTree<()>,
        node: crate::taffy::NodeId,
        origin: crate::kurbo::Point,
    ) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let WindowEvent::MouseWheel { delta, .. } = event else {
            return;
        };
        let Some(travel) = crate::widget::sideways_travel(delta, ctx.scale_factor) else {
            return;
        };
        let reach = (Self::WIDTH - self.rect.width()).max(0.0);
        let next = (self.offset.get() + travel).clamp(0.0, reach);
        if (next - self.offset.get()).abs() < 0.001 {
            return;
        }
        self.offset.set(next);
        *ctx.handled = true;
    }

    fn name(&self) -> &'static str {
        "Wide"
    }
}

/// A window whose content scrolls sideways with the wheel, and that content's
/// offset.
fn wide() -> (App, UnixStream, Rc<Cell<f64>>) {
    let offset = Rc::new(Cell::new(0.0));
    let view = Wide {
        rect: crate::kurbo::Rect::ZERO,
        offset: Rc::clone(&offset),
    };
    let (app, shell) = window(Box::new(view));
    (app, shell, offset)
}

#[test]
fn a_wide_view_keeps_its_sideways_scroll_and_the_strip_hears_nothing() {
    let (mut app, mut shell, offset) = wide();
    let window = Surface;

    app.surface_input(&window, sideways(-120.0), false);
    app.surface_input(&window, sideways(-120.0), false);
    assert_eq!(offset.get(), 240.0, "the content scrolled by both events");
    assert_eq!(
        left_to_the_strip(&mut app, &mut shell),
        0.0,
        "an event the app scrolled with is never the strip's"
    );
}

#[test]
fn a_content_that_does_not_scroll_hands_the_whole_gesture_to_the_strip() {
    let (mut app, mut shell) = window(Box::new(crate::widget::Column::new()));
    let window = Surface;

    // The first event decides: nothing here scrolls sideways, so every event
    // of the gesture is the strip's, and no view of the app sees one.
    let mut handed = 0.0;
    for _ in 0..5 {
        app.surface_input(&window, sideways(-40.0), false);
        handed += left_to_the_strip(&mut app, &mut shell);
    }
    assert_eq!(handed, 200.0, "the strip pans by what the window left");

    // A pause ends the gesture: what comes after decides afresh.
    app.floating.sideways_gesture = None;
    app.surface_input(&window, sideways(-40.0), false);
    assert_eq!(left_to_the_strip(&mut app, &mut shell), 40.0);
}

#[test]
fn a_gesture_the_content_took_is_never_split_by_the_strip() {
    let (mut app, mut shell, offset) = wide();
    let window = Surface;

    // The view scrolls until its last column, and the wheel carries on: the
    // rest of the gesture is still the content's, and the strip stays home.
    for _ in 0..20 {
        app.surface_input(&window, sideways(-120.0), false);
    }
    assert!(
        offset.get() > 0.0,
        "the content scrolled as far as it goes: {}",
        offset.get()
    );
    assert_eq!(
        left_to_the_strip(&mut app, &mut shell),
        0.0,
        "a gesture the content took never pans the strip"
    );
}

#[test]
fn a_plain_wheel_is_no_sideways_gesture_at_all() {
    let (mut app, mut shell) = window(Box::new(crate::widget::Column::new()));
    let window = Surface;

    for _ in 0..3 {
        app.surface_input(&window, plain(-40.0), false);
    }
    assert_eq!(
        left_to_the_strip(&mut app, &mut shell),
        0.0,
        "a vertical wheel neither pans the strip nor decides a sideways gesture"
    );
    // Nor did it take ownership of one: the next sideways wheel is the
    // strip's from its first event.
    app.surface_input(&window, sideways(-40.0), false);
    assert_eq!(left_to_the_strip(&mut app, &mut shell), 40.0);
}

#[test]
fn the_shell_hears_nothing_of_a_gesture_the_app_keeps() {
    let (mut app, mut shell, _offset) = wide();
    let window = Surface;

    // The content scrolls with the gesture's first event, so every event of
    // it is the content's — including one at its end that moves nothing.
    app.surface_input(&window, sideways(-120.0), false);
    app.surface_input(&window, sideways(-100_000.0), false);
    assert_eq!(left_to_the_strip(&mut app, &mut shell), 0.0);

    // What the shell writes on the same connection is still read as its own.
    shell
        .write_all(b"{\"kind\":\"reattach\",\"id\":\"gone\"}\n")
        .unwrap();
    app.pump_floating_surface_service();
}
