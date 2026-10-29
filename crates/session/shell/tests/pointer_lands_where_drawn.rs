//! A click lands on the point of a window drawn under the pointer.
//!
//! A browser that draws its own frame (Chromium, Firefox) commits a buffer
//! with a shadow margin around the window, declares the window inside it
//! with `xdg_surface.set_window_geometry`, and on a scaled display hands
//! over a denser buffer: through `wp_viewporter` at a fractional scale, or
//! with `wl_surface.set_buffer_scale` at an integer one. The tile shows the
//! window's rectangle only, so every click must be told to the client at
//! the surface-local point whose pixels were drawn under the pointer, not
//! shifted by the margin, by the window bar, or by mixing buffer and
//! logical pixels. These walk a real socket: the client draws a target,
//! the test finds where the compositor drew it, clicks its centre through
//! the production input router and reads the coordinates off the wire.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "test harness failures are assertions; the sums are window sizes in pixels, far \
              from overflow"
)]

use shell::compositor::state::State;
use vello::kurbo::{Point, Rect};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_pointer, wl_region, wl_seat, wl_subcompositor, wl_subsurface,
    wl_surface,
};
use wayland_client::{Connection, Dispatch, Proxy as _, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

use crate::support::wayland::{self, Toplevel, create_toplevel};

/// The client-drawn shadow around the window, logical pixels. Uneven, as a
/// frame whose shadow falls downward has it, so a margin taken off the
/// wrong edge shows up as an offset.
const SHADOW_LEFT: i32 = 10;
const SHADOW_TOP: i32 = 6;
const SHADOW_RIGHT: i32 = 10;
const SHADOW_BOTTOM: i32 = 14;

/// The target the test clicks: window-local logical pixels.
const TARGET: (i32, i32, i32, i32) = (200, 120, 24, 24);

/// Bytes of one pixel in `wl_shm` ARGB8888 (little-endian B, G, R, A).
const SHADOW_PIXEL: [u8; 4] = [40, 40, 40, 255];
const PAGE_PIXEL: [u8; 4] = [255, 255, 255, 255];
const TARGET_PIXEL: [u8; 4] = [0, 0, 255, 255];

#[derive(Debug, Clone, Copy, PartialEq)]
enum Wire {
    /// The pointer entered the surface with this protocol id.
    Enter(u32),
    At(f64, f64),
    Press,
}

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    /// The last size the compositor configured the window to, logical.
    configured: Option<(i32, i32)>,
    wire: Vec<Wire>,
    /// The protocol id of the surface the pointer is on, as the client was
    /// told by its last enter and leave.
    entered: Option<u32>,
}

wayland::test_client!(Client: registry, ping, configure, surface, shm, buffer, seat);

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for Client {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event {
            state.configured = Some((width, height));
        }
    }
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
        match &event {
            wl_pointer::Event::Enter { surface, .. } => {
                state.entered = Some(surface.id().protocol_id());
                state.wire.push(Wire::Enter(surface.id().protocol_id()));
            }
            wl_pointer::Event::Leave { .. } => state.entered = None,
            _ => {}
        }
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            }
            | wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => state.wire.push(Wire::At(surface_x, surface_y)),
            wl_pointer::Event::Button {
                state: WEnum::Value(wl_pointer::ButtonState::Pressed),
                ..
            } => state.wire.push(Wire::Press),
            _ => {}
        }
    }
}

delegate_noop!(Client: ignore wl_region::WlRegion);
delegate_noop!(Client: ignore wl_subcompositor::WlSubcompositor);
delegate_noop!(Client: ignore wl_subsurface::WlSubsurface);
delegate_noop!(Client: ignore wp_viewporter::WpViewporter);
delegate_noop!(Client: ignore wp_viewport::WpViewport);

/// How the client hands the compositor a buffer denser than its window.
#[derive(Debug, Clone, Copy)]
enum Density {
    /// `wp_viewporter`: a buffer `scale` times the logical size, declared at
    /// the logical size, as Chromium does with fractional scaling.
    Viewport(f64),
    /// `wl_surface.set_buffer_scale`.
    BufferScale(i32),
}

impl Density {
    fn factor(self) -> f64 {
        match self {
            Self::Viewport(scale) => scale,
            Self::BufferScale(scale) => f64::from(scale),
        }
    }
}

/// A logical length times a density, in whole buffer pixels.
#[allow(
    clippy::cast_possible_truncation,
    reason = "rounded first, and every length here is a few thousand pixels at most"
)]
fn buffer_pixels(scaled: f64) -> i32 {
    scaled.round() as i32
}

/// The buffer of a `width`×`height` window drawn with its shadow around
/// it and the target in it, `factor` buffer pixels per logical pixel: its
/// ARGB8888 bytes and size.
fn framed_pixels((width, height): (i32, i32), factor: f64) -> (Vec<u8>, i32, i32) {
    let pixels = |value: i32| buffer_pixels(f64::from(value) * factor);
    let buffer_w = pixels(width + SHADOW_LEFT + SHADOW_RIGHT);
    let buffer_h = pixels(height + SHADOW_TOP + SHADOW_BOTTOM);
    let target = (
        pixels(SHADOW_LEFT + TARGET.0),
        pixels(SHADOW_TOP + TARGET.1),
        pixels(SHADOW_LEFT + TARGET.0 + TARGET.2),
        pixels(SHADOW_TOP + TARGET.1 + TARGET.3),
    );
    let window = (
        pixels(SHADOW_LEFT),
        pixels(SHADOW_TOP),
        pixels(SHADOW_LEFT + width),
        pixels(SHADOW_TOP + height),
    );
    let inside = |x: i32, y: i32, (x0, y0, x1, y1): (i32, i32, i32, i32)| {
        x >= x0 && y >= y0 && x < x1 && y < y1
    };
    let mut rgba = Vec::with_capacity(usize::try_from(buffer_w * buffer_h * 4).unwrap());
    for y in 0..buffer_h {
        for x in 0..buffer_w {
            let pixel = if inside(x, y, target) {
                TARGET_PIXEL
            } else if inside(x, y, window) {
                PAGE_PIXEL
            } else {
                SHADOW_PIXEL
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    (rgba, buffer_w, buffer_h)
}

type Harness = wayland::Framed<Client>;

/// A window whose frame and shadow the client draws, mapped and on screen.
struct Window {
    id: u64,
    surface: wl_surface::WlSurface,
    // Held so the client's objects outlive the test's clicks.
    _toplevel: xdg_toplevel::XdgToplevel,
    _pointer: wl_pointer::WlPointer,
    _viewport: Option<wp_viewport::WpViewport>,
    _buffer: wl_buffer::WlBuffer,
}

impl Harness {
    fn at_display_scale(scale: f64) -> Self {
        let wl = wayland::Harness::<Client>::with_state(|event_loop, display| {
            let mut state = State::new(event_loop, display);
            state.scale = scale;
            state
        });
        Self::from_harness(wl, (1280, 800))
    }

    /// Open a window the way a browser drawing its own frame does, at the
    /// size the compositor configures, with `density` buffer pixels per
    /// logical pixel and the target drawn into it.
    fn open_framed_browser(&mut self, density: Density) -> Window {
        let qh = self.queue.handle();
        let seat = self.bind::<wl_seat::WlSeat>("wl_seat", 9);
        let pointer = seat.get_pointer(&qh, ());
        let compositor = self.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
        let wm = self.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 2);
        let viewporter = self.bind::<wp_viewporter::WpViewporter>("wp_viewporter", 1);
        let Toplevel {
            surface,
            xdg,
            toplevel,
        } = create_toplevel(&compositor, &wm, &qh, "org.example.FramedBrowser");
        surface.commit();
        self.pump();
        let _ = self.fb.paint_settled();
        self.pump();
        let id = self.foreign_id("org.example.FramedBrowser");

        let (width, height) = match self.client.configured {
            Some((w, h)) if w > 0 && h > 0 => (w, h),
            _ => (640, 400),
        };
        let logical = (
            width + SHADOW_LEFT + SHADOW_RIGHT,
            height + SHADOW_TOP + SHADOW_BOTTOM,
        );
        let (rgba, buffer_w, buffer_h) = framed_pixels((width, height), density.factor());
        let buffer = self.shm_buffer(buffer_w, buffer_h, &rgba);

        let viewport = match density {
            Density::Viewport(_) => {
                let viewport = viewporter.get_viewport(&surface, &qh, ());
                viewport.set_destination(logical.0, logical.1);
                Some(viewport)
            }
            Density::BufferScale(scale) => {
                surface.set_buffer_scale(scale);
                None
            }
        };
        xdg.set_window_geometry(SHADOW_LEFT, SHADOW_TOP, width, height);
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, buffer_w, buffer_h);
        surface.commit();
        // The capture runs on a worker; the commit, with its geometry and
        // scale, applies once it is done.
        let display = self.wl.display.handle();
        let deadline = std::time::Instant::now()
            .checked_add(tilo_test_env::hang_bound())
            .expect("the hang bound fits in an Instant");
        let applied = || {
            shell::shell::foreign::store()
                .frame_geometry(id)
                .is_some_and(|geometry| {
                    geometry["raw_buffer"]["w"] == buffer_w
                        && geometry["raw_buffer"]["h"] == buffer_h
                        && !geometry["window_geometry"].is_null()
                })
        };
        while !applied() {
            assert!(
                std::time::Instant::now() < deadline,
                "the window's buffer was never captured with its geometry"
            );
            self.pump();
            self.wl.state.pump_foreign_capture_barriers(&display);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let _ = self.fb.paint_settled();
        self.pump();
        assert!(
            shell::shell::foreign::store().is_mapped(id),
            "the window is mapped and can take the pointer"
        );
        Window {
            id,
            surface,
            _toplevel: toplevel,
            _pointer: pointer,
            _viewport: viewport,
            _buffer: buffer,
        }
    }

    fn tile(&self, surface_id: u64) -> Rect {
        let chrome = self.fb.chrome();
        let tree = chrome.tree();
        shell::shell::input_policy::foreign_tiles(&tree, self.fb.host())
            .into_iter()
            .find(|hit| hit.surface_id == surface_id)
            .expect("the window is on screen")
            .rect
    }

    /// Where on the desktop the compositor drew the target: its pixels
    /// found in the window's composed image, placed where the tile paints
    /// that image.
    fn drawn_target(&self, window: &Window) -> Rect {
        let store = shell::shell::foreign::store();
        let tile = self.tile(window.id);
        let frame = store.snapshot(window.id).expect("the window has a frame");
        let width = usize::try_from(frame.width).unwrap();
        let mut found: Option<(usize, usize, usize, usize)> = None;
        for (index, pixel) in frame.rgba.chunks_exact(4).enumerate() {
            // The image is RGBA; the target is pure red, resampled edges
            // excluded.
            if pixel[0] > 200 && pixel[1] < 40 && pixel[2] < 40 {
                let (x, y) = (index % width, index / width);
                found = Some(found.map_or((x, y, x, y), |(x0, y0, x1, y1)| {
                    (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
                }));
            }
        }
        let (x0, y0, x1, y1) = found.expect("the target is drawn in the window");
        let drawn = store
            .frame_rect(window.id, tile, frame.width, frame.height)
            .expect("the window is painted in its tile");
        let sx = drawn.width() / f64::from(frame.width);
        let sy = drawn.height() / f64::from(frame.height);
        let to_desktop = |x: usize, y: usize| {
            Point::new(
                drawn.x0 + f64::from(u32::try_from(x).unwrap()) * sx,
                drawn.y0 + f64::from(u32::try_from(y).unwrap()) * sy,
            )
        };
        Rect::from_points(to_desktop(x0, y0), to_desktop(x1 + 1, y1 + 1))
    }

    /// The production order: the seat first, the shell with what it leaves.
    fn input(&mut self, event: &WindowEvent) {
        let modifiers = Modifiers::default();
        let _ = shell::compositor::client_input::route_input(
            &mut self.wl.state,
            &mut self.fb,
            event,
            &modifiers,
        );
        self.wl.pump();
        let _ = self.fb.paint_settled();
    }

    /// Move to `at` (logical desktop pixels) and press the left button;
    /// where the client was told the pointer was when the press arrived.
    fn click(&mut self, at: Point) -> (f64, f64) {
        self.client.wire.clear();
        self.input(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(at.x, at.y),
        });
        self.input(&WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        let wire = &self.client.wire;
        let press = wire
            .iter()
            .position(|event| *event == Wire::Press)
            .unwrap_or_else(|| panic!("the press reached the window: {wire:?}"));
        wire[..press]
            .iter()
            .rev()
            .find_map(|event| match event {
                Wire::At(x, y) => Some((*x, *y)),
                Wire::Enter(_) | Wire::Press => None,
            })
            .unwrap_or_else(|| panic!("the window was told where the pointer is: {wire:?}"))
    }
}

/// At display `scale`, a client handing over `density` buffers: a click on
/// the centre of the drawn target reaches the client at the target's own
/// centre, surface-local.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the harness (a frame builder) is what the test drives, up to its last assertion"
)]
fn a_click_reaches_the_point_drawn_under_it(scale: f64, density: Density) {
    let mut h = Harness::at_display_scale(scale);
    let window = h.open_framed_browser(density);
    let tile = h.tile(window.id);
    let drawn = h.drawn_target(&window);
    assert!(
        tile.contains(drawn.center()),
        "the target is drawn inside the tile ({tile:?}): {drawn:?}"
    );
    let (x, y) = h.click(drawn.center());
    let want = (
        f64::from(SHADOW_LEFT + TARGET.0) + f64::from(TARGET.2) / 2.0,
        f64::from(SHADOW_TOP + TARGET.1) + f64::from(TARGET.3) / 2.0,
    );
    assert!(
        (x - want.0).abs() <= 1.0 && (y - want.1).abs() <= 1.0,
        "at {scale}x with {density:?} a click on the centre of the drawn \
         target ({:?}) reached the window at surface-local ({x}, {y}); the \
         target's centre is ({}, {}), an offset of ({}, {})",
        drawn.center(),
        want.0,
        want.1,
        x - want.0,
        y - want.1,
    );
    shell::shell::foreign::store().forget(window.id);
}

#[test]
fn a_click_reaches_the_point_drawn_under_it_at_unit_scale() {
    a_click_reaches_the_point_drawn_under_it(1.0, Density::BufferScale(1));
}

#[test]
fn a_click_reaches_the_point_drawn_under_it_at_a_fractional_scale() {
    a_click_reaches_the_point_drawn_under_it(1.25, Density::Viewport(1.25));
}

#[test]
fn a_click_reaches_the_point_drawn_under_it_at_double_scale_through_a_viewport() {
    a_click_reaches_the_point_drawn_under_it(2.0, Density::Viewport(2.0));
}

#[test]
fn a_click_reaches_the_point_drawn_under_it_at_double_scale_with_a_buffer_scale() {
    a_click_reaches_the_point_drawn_under_it(2.0, Density::BufferScale(2));
}

/// The page a browser draws into a subsurface over its window.
struct Page {
    id: u64,
    protocol_id: u32,
    _surface: wl_surface::WlSurface,
    _subsurface: wl_subsurface::WlSubsurface,
    _buffer: wl_buffer::WlBuffer,
}

impl Harness {
    /// Cover `window`'s whole window geometry with a subsurface the way
    /// Firefox (and so `LibreWolf`) draws everything it shows: the toplevel is
    /// GTK's, the page, tab strip and toolbar are drawn into a subsurface.
    /// `passes_input` gives that subsurface an empty input region, as
    /// Firefox does so that GTK, which only handles input on its own
    /// toplevel, receives every press.
    fn cover_with_page(&mut self, window: &Window, passes_input: bool) -> Page {
        let qh = self.queue.handle();
        let compositor = self.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
        let subcompositor = self.bind::<wl_subcompositor::WlSubcompositor>("wl_subcompositor", 1);
        let (width, height) = match self.client.configured {
            Some((w, h)) if w > 0 && h > 0 => (w, h),
            _ => (640, 400),
        };
        let surface = compositor.create_surface(&qh, ());
        let subsurface = subcompositor.get_subsurface(&surface, &window.surface, &qh, ());
        subsurface.set_position(SHADOW_LEFT, SHADOW_TOP);
        let rgba = PAGE_PIXEL.repeat(usize::try_from(width * height).unwrap());
        let buffer = self.shm_buffer(width, height, &rgba);
        if passes_input {
            let region = compositor.create_region(&qh, ());
            surface.set_input_region(Some(&region));
            region.destroy();
        }
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.commit();
        // A synchronized subsurface's state applies with its parent's commit.
        window.surface.commit();
        let protocol_id = surface.id().protocol_id();
        let id = (window.id & !0xFFFF_FFFF) | u64::from(protocol_id);
        let display = self.wl.display.handle();
        let deadline = std::time::Instant::now()
            .checked_add(tilo_test_env::hang_bound())
            .expect("the hang bound fits in an Instant");
        let store = shell::shell::foreign::store();
        while !(store.is_mapped(id) && store.frame_size(id).is_some()) {
            assert!(
                std::time::Instant::now() < deadline,
                "the page subsurface was never captured"
            );
            self.pump();
            self.wl.state.pump_foreign_capture_barriers(&display);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let _ = self.fb.paint_settled();
        self.pump();
        Page {
            id,
            protocol_id,
            _surface: surface,
            _subsurface: subsurface,
            _buffer: buffer,
        }
    }

    /// The protocol id of the surface the pointer is on.
    fn entered(&self) -> Option<u32> {
        self.client.entered
    }
}

/// A browser drawing everything into a subsurface that passes input to its
/// toplevel takes a press anywhere in its window — the tab strip in its top
/// rows and the page below — on the toplevel, at the surface-local point
/// drawn under the pointer. Pressing the window bar Tilo draws above it
/// stays with Tilo.
#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the harness (a frame builder) is what the test drives, up to its last assertion"
)]
fn a_press_anywhere_in_a_browser_reaches_the_surface_that_takes_input() {
    let mut h = Harness::at_display_scale(1.0);
    let window = h.open_framed_browser(Density::BufferScale(1));
    let page = h.cover_with_page(&window, true);
    let tile = h.tile(window.id);
    let root_protocol_id = window.surface.id().protocol_id();
    // Its top rows (where the tab strip and URL bar are), its middle and
    // near its bottom-right corner, window-local.
    for (wx, wy) in [(40.0, 3.0), (300.0, 20.0), (200.0, 150.0), (500.0, 300.0)] {
        let at = Point::new(tile.x0 + wx, tile.y0 + wy);
        let (x, y) = h.click(at);
        assert_eq!(
            h.entered(),
            Some(root_protocol_id),
            "a press at window-local ({wx}, {wy}) went to the toplevel GTK \
             handles input on, not the page subsurface with an empty input \
             region: {:?}",
            h.client.wire
        );
        // Where the painter drew that point of the toplevel; the existing
        // tests above pin this inverse against the drawn pixels.
        let want = shell::shell::foreign::store().surface_point(window.id, tile, (at.x, at.y));
        assert!(
            (x - want.0).abs() <= 1.0 && (y - want.1).abs() <= 1.0,
            "a press at window-local ({wx}, {wy}) reached the toplevel at \
             ({x}, {y}), want {want:?}"
        );
        h.input(&WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        });
    }

    // Tilo's own window bar sits above the tile: its presses are Tilo's.
    h.client.wire.clear();
    let bar = Point::new(tile.x0 + 40.0, tile.y0 - 4.0);
    h.input(&WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(bar.x, bar.y),
    });
    let modifiers = Modifiers::default();
    let to_client = shell::compositor::client_input::route_input(
        &mut h.wl.state,
        &mut h.fb,
        &WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
        &modifiers,
    );
    h.wl.pump();
    assert!(
        !to_client && !h.client.wire.contains(&Wire::Press),
        "a press on Tilo's window bar above the tile stayed with Tilo: {:?}",
        h.client.wire
    );
    let store = shell::shell::foreign::store();
    store.forget(page.id);
    store.forget(window.id);
}

/// A subsurface that keeps the default input region (the whole surface)
/// still takes the presses on it, at its own surface-local point.
#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the harness (a frame builder) is what the test drives, up to its last assertion"
)]
fn a_subsurface_with_the_default_input_region_takes_its_presses() {
    let mut h = Harness::at_display_scale(1.0);
    let window = h.open_framed_browser(Density::BufferScale(1));
    let page = h.cover_with_page(&window, false);
    let tile = h.tile(window.id);
    let at = Point::new(tile.x0 + 120.0, tile.y0 + 4.0);
    let on_root = shell::shell::foreign::store().surface_point(window.id, tile, (at.x, at.y));
    let want = (
        on_root.0 - f64::from(SHADOW_LEFT),
        on_root.1 - f64::from(SHADOW_TOP),
    );
    let (x, y) = h.click(at);
    assert_eq!(
        h.entered(),
        Some(page.protocol_id),
        "the press went to the subsurface drawn there: {:?}",
        h.client.wire
    );
    assert!(
        (x - want.0).abs() <= 1.0 && (y - want.1).abs() <= 1.0 && want.1 >= 0.0,
        "the subsurface got the press at ({x}, {y}), want {want:?}"
    );
    let store = shell::shell::foreign::store();
    store.forget(page.id);
    store.forget(window.id);
}
