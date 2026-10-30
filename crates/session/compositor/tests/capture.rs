//! Screen capture against a headless compositor: a trusted tool copies a
//! whole output and a region of it into shared memory through
//! `wlr-screencopy`, waits for damage when it asks to, and copies through
//! `ext-image-copy-capture`, and picks a window from the toplevel list and
//! copies only it, with the pointer when asked; a cursor session carries
//! the pointer as its own image and says where it is; ordinary clients see
//! none of these globals.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::os::fd::{AsFd as _, OwnedFd};
use std::time::Duration;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::Transform;
use tilo_compositor::{
    Compositor, HostFrame, HostLayer, InputEvent, OutputConfig, StackPolicy, SurfaceHost,
};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_output::WlOutput, wl_pointer::WlPointer,
    wl_registry, wl_registry::WlRegistry, wl_seat::WlSeat, wl_shm, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop, event_created_child,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols::ext::image_capture_source::v1::client::ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1;
use wayland_protocols::xdg::shell::client::{
    xdg_surface, xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel, xdg_wm_base,
    xdg_wm_base::XdgWmBase,
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_cursor_session_v1::{self, ExtImageCopyCaptureCursorSessionV1},
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

/// The desktop's background, B, G, R, X as XRGB8888 lies in memory.
const BACKGROUND: [u8; 3] = [38, 27, 26];
const RED: [u8; 3] = [0, 0, 255];
const GREEN: [u8; 3] = [0, 255, 0];

#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one flag per event a test tool waits for"
)]
struct Seen {
    globals: Vec<String>,
    shm: Option<WlShm>,
    output: Option<WlOutput>,
    screencopy: Option<ZwlrScreencopyManagerV1>,
    image_copy: Option<ExtImageCopyCaptureManagerV1>,
    sources: Option<ExtOutputImageCaptureSourceManagerV1>,
    window_sources: Option<ExtForeignToplevelImageCaptureSourceManagerV1>,
    toplevel_list: Option<ExtForeignToplevelListV1>,
    /// Listed windows and their app ids.
    toplevels: Vec<(ExtForeignToplevelHandleV1, String)>,
    compositor: Option<WlCompositor>,
    wm_base: Option<XdgWmBase>,
    configured: bool,
    /// The screencopy frame's shm buffer: format, width, height, stride.
    buffer: Option<(u32, u32, u32, u32)>,
    buffer_done: bool,
    damage: Vec<(u32, u32, u32, u32)>,
    ready: bool,
    failed: bool,
    /// The image-copy session's buffer size and shm formats.
    session_size: Option<(u32, u32)>,
    session_formats: Vec<u32>,
    session_done: bool,
    transform: Option<u32>,
    seat: Option<WlSeat>,
    /// Cursor session events, in order: `enter`, `leave`, `position x y`,
    /// `hotspot x y`.
    cursor: Vec<String>,
}

struct Tool {
    connection: Connection,
    queue: EventQueue<Seen>,
    seen: Seen,
}

impl Tool {
    fn connect(compositor: &mut Compositor, trusted: bool) -> Self {
        let socket = if trusted {
            compositor.connect_trusted_client()
        } else {
            compositor.connect_client()
        }
        .expect("client socket");
        let connection = Connection::from_socket(socket).expect("connection");
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        Self {
            connection,
            queue,
            seen: Seen::default(),
        }
    }

    fn exchange(&mut self) {
        self.connection.flush().expect("flush");
        if let Some(guard) = self.connection.prepare_read() {
            let _ = guard.read();
        }
        self.queue
            .dispatch_pending(&mut self.seen)
            .expect("client dispatch");
        self.connection.flush().expect("flush");
    }

    /// A shared-memory buffer of `width` x `height` XRGB8888 pixels and
    /// the memory behind it.
    fn buffer(&self, width: i32, height: i32) -> (WlBuffer, OwnedFd) {
        self.buffer_of(width, height, wl_shm::Format::Xrgb8888)
    }

    fn buffer_of(&self, width: i32, height: i32, format: wl_shm::Format) -> (WlBuffer, OwnedFd) {
        let qh = self.queue.handle();
        let fd =
            rustix::fs::memfd_create("tilo-capture-test", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        rustix::fs::ftruncate(&fd, u64::try_from(width * height * 4).unwrap()).unwrap();
        let pool = self.seen.shm.as_ref().expect("wl_shm").create_pool(
            fd.as_fd(),
            width * height * 4,
            &qh,
            (),
        );
        let buffer = pool.create_buffer(0, width, height, width * 4, format, &qh, ());
        pool.destroy();
        (buffer, fd)
    }
}

/// The B, G, R of the pixel at `x`, `y` of an XRGB8888 image `width` wide.
fn pixel(fd: &OwnedFd, width: usize, x: usize, y: usize) -> [u8; 3] {
    let mut bytes = [0_u8; 3];
    let offset = u64::try_from((y * width + x) * 4).unwrap();
    rustix::io::pread(fd, &mut bytes, offset).unwrap();
    bytes
}

fn near(got: [u8; 3], want: [u8; 3]) -> bool {
    got.iter()
        .zip(want)
        .all(|(got, want)| got.abs_diff(want) <= 1)
}

/// Let compositor and tool talk, drawing a frame each round, until `done`
/// holds; false when it never does.
fn pump_until(
    compositor: &mut Compositor,
    tool: &mut Tool,
    rounds: usize,
    mut done: impl FnMut(&Seen) -> bool,
) -> bool {
    for _ in 0..rounds {
        tool.exchange();
        compositor
            .dispatch(Duration::from_millis(2))
            .expect("dispatch");
        compositor.render().expect("render");
        tool.exchange();
        if done(&tool.seen) {
            return true;
        }
    }
    false
}

fn pump(compositor: &mut Compositor, tool: &mut Tool, done: impl FnMut(&Seen) -> bool) {
    assert!(
        pump_until(compositor, tool, 500, done),
        "condition not reached"
    );
}

/// A hosted surface: one square of one colour.
struct Square {
    rect: (i32, i32, i32, i32),
    buffer: MemoryRenderBuffer,
}

impl Square {
    fn new(rect: (i32, i32, i32, i32), bgr: [u8; 3]) -> Self {
        let pixel = [bgr[0], bgr[1], bgr[2], 255];
        let bytes = pixel.repeat(usize::try_from(rect.2 * rect.3).unwrap());
        let buffer = MemoryRenderBuffer::from_slice(
            &bytes,
            Fourcc::Argb8888,
            (rect.2, rect.3),
            1,
            Transform::Normal,
            None,
        );
        Self { rect, buffer }
    }
}

impl SurfaceHost for Square {
    fn layers(&self) -> Vec<HostLayer> {
        vec![HostLayer {
            location: (self.rect.0, self.rect.1),
            size: (self.rect.2, self.rect.3),
            frame: HostFrame::from_buffer(
                self.buffer.clone(),
                (
                    u32::try_from(self.rect.2).expect("a whole number of pixels"),
                    u32::try_from(self.rect.3).expect("a whole number of pixels"),
                ),
            ),
        }]
    }

    fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
        false
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, _button: u32, _pressed: bool) {}
}

/// A 64 x 48 output with a red square at 8, 8 of side 16.
fn compositor() -> Compositor {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 64, 48))
        .expect("output");
    compositor.add_host(Box::new(Square::new((8, 8, 16, 16), RED)));
    compositor
}

/// Open a window of `width` x `height` in one colour (B, G, R) and wait
/// until it is configured and shown.
fn open_window(
    compositor: &mut Compositor,
    tool: &mut Tool,
    width: i32,
    height: i32,
    bgr: [u8; 3],
) -> (WlSurface, XdgToplevel) {
    let qh = tool.queue.handle();
    let surface = tool
        .seen
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh, ());
    let xdg = tool
        .seen
        .wm_base
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("org.example.shared".into());
    surface.commit();
    pump(compositor, tool, |seen| seen.configured);
    let (buffer, memory) = tool.buffer(width, height);
    let pixel = [bgr[0], bgr[1], bgr[2], 255];
    rustix::io::pwrite(
        &memory,
        &pixel.repeat(usize::try_from(width * height).unwrap()),
        0,
    )
    .unwrap();
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, width, height);
    surface.commit();
    pump(compositor, tool, |_| true);
    (surface, toplevel)
}

fn trusted_tool(compositor: &mut Compositor) -> Tool {
    let mut tool = Tool::connect(compositor, true);
    pump(compositor, &mut tool, |seen| {
        seen.shm.is_some()
            && seen.output.is_some()
            && seen.screencopy.is_some()
            && seen.image_copy.is_some()
            && seen.sources.is_some()
            && seen.window_sources.is_some()
            && seen.toplevel_list.is_some()
            && seen.compositor.is_some()
            && seen.wm_base.is_some()
    });
    tool
}

#[test]
fn ordinary_clients_cannot_capture_the_screen() {
    let mut compositor = compositor();
    let mut tool = Tool::connect(&mut compositor, false);
    pump(&mut compositor, &mut tool, |seen| seen.output.is_some());
    for global in [
        "zwlr_screencopy_manager_v1",
        "ext_image_copy_capture_manager_v1",
        "ext_output_image_capture_source_manager_v1",
        "ext_foreign_toplevel_image_capture_source_manager_v1",
        "ext_foreign_toplevel_list_v1",
    ] {
        assert!(
            !tool.seen.globals.iter().any(|seen| seen == global),
            "{global} offered to an ordinary client"
        );
    }
}

#[test]
fn a_trusted_tool_copies_an_output_and_a_region_of_it() {
    let mut compositor = compositor();
    let mut tool = trusted_tool(&mut compositor);
    let qh = tool.queue.handle();
    let manager = tool.seen.screencopy.clone().unwrap();
    let output = tool.seen.output.clone().unwrap();

    let frame = manager.capture_output(0, &output, &qh, ());
    pump(&mut compositor, &mut tool, |seen| seen.buffer_done);
    assert_eq!(
        tool.seen.buffer,
        Some((wl_shm::Format::Xrgb8888 as u32, 64, 48, 256))
    );
    let (buffer, memory) = tool.buffer(64, 48);
    frame.copy(&buffer);
    pump(&mut compositor, &mut tool, |seen| seen.ready || seen.failed);
    assert!(tool.seen.ready, "the frame was copied");
    assert!(near(pixel(&memory, 64, 0, 0), BACKGROUND));
    assert!(near(pixel(&memory, 64, 63, 47), BACKGROUND));
    assert!(near(pixel(&memory, 64, 8, 8), RED));
    assert!(near(pixel(&memory, 64, 23, 23), RED));
    assert!(near(pixel(&memory, 64, 24, 24), BACKGROUND));
    frame.destroy();

    tool.seen.buffer = None;
    tool.seen.buffer_done = false;
    tool.seen.ready = false;
    let frame = manager.capture_output_region(0, &output, 16, 16, 16, 16, &qh, ());
    pump(&mut compositor, &mut tool, |seen| seen.buffer_done);
    assert_eq!(
        tool.seen.buffer,
        Some((wl_shm::Format::Xrgb8888 as u32, 16, 16, 64))
    );
    let (buffer, memory) = tool.buffer(16, 16);
    frame.copy(&buffer);
    pump(&mut compositor, &mut tool, |seen| seen.ready || seen.failed);
    assert!(tool.seen.ready);
    // The region's top left quarter is the square's bottom right.
    assert!(near(pixel(&memory, 16, 0, 0), RED));
    assert!(near(pixel(&memory, 16, 7, 7), RED));
    assert!(near(pixel(&memory, 16, 8, 8), BACKGROUND));
}

#[test]
fn a_copy_with_damage_waits_until_the_output_changes() {
    let mut compositor = compositor();
    let mut tool = trusted_tool(&mut compositor);
    let qh = tool.queue.handle();
    let manager = tool.seen.screencopy.clone().unwrap();
    let output = tool.seen.output.clone().unwrap();
    let (buffer, memory) = tool.buffer(64, 48);

    // The first copy has everything as damage.
    let frame = manager.capture_output(0, &output, &qh, ());
    pump(&mut compositor, &mut tool, |seen| seen.buffer_done);
    frame.copy_with_damage(&buffer);
    pump(&mut compositor, &mut tool, |seen| seen.ready);
    assert_eq!(tool.seen.damage, vec![(0, 0, 64, 48)]);
    frame.destroy();

    tool.seen.ready = false;
    tool.seen.damage.clear();
    let frame = manager.capture_output(0, &output, &qh, ());
    frame.copy_with_damage(&buffer);
    assert!(
        !pump_until(&mut compositor, &mut tool, 20, |seen| seen.ready),
        "nothing changed, nothing is copied"
    );
    compositor.add_host(Box::new(Square::new((40, 30, 8, 8), GREEN)));
    pump(&mut compositor, &mut tool, |seen| seen.ready);
    // The new square is damaged (and the one it moved down in the stack),
    // not the whole output.
    assert!(tool.seen.damage.contains(&(40, 30, 8, 8)));
    assert!(!tool.seen.damage.contains(&(0, 0, 64, 48)));
    assert!(near(pixel(&memory, 64, 44, 34), GREEN));
}

#[test]
fn a_wrong_buffer_is_a_protocol_error() {
    let mut compositor = compositor();
    let mut tool = trusted_tool(&mut compositor);
    let qh = tool.queue.handle();
    let manager = tool.seen.screencopy.clone().unwrap();
    let output = tool.seen.output.clone().unwrap();
    let frame = manager.capture_output(0, &output, &qh, ());
    pump(&mut compositor, &mut tool, |seen| seen.buffer_done);
    let (buffer, _memory) = tool.buffer(32, 48);
    frame.copy(&buffer);
    for _ in 0..50 {
        compositor
            .dispatch(Duration::from_millis(2))
            .expect("dispatch");
        tool.connection.flush().ok();
        if let Some(guard) = tool.connection.prepare_read() {
            let _ = guard.read();
        }
        if tool.queue.dispatch_pending(&mut tool.seen).is_err() {
            break;
        }
    }
    assert!(
        tool.connection.protocol_error().is_some(),
        "a buffer of the wrong size is refused"
    );
}

#[test]
fn a_trusted_tool_copies_an_output_through_image_copy_capture() {
    let mut compositor = compositor();
    let mut tool = trusted_tool(&mut compositor);
    let qh = tool.queue.handle();
    let sources = tool.seen.sources.clone().unwrap();
    let manager = tool.seen.image_copy.clone().unwrap();
    let output = tool.seen.output.clone().unwrap();

    let source = sources.create_source(&output, &qh, ());
    let session = manager.create_session(&source, Options::empty(), &qh, ());
    pump(&mut compositor, &mut tool, |seen| seen.session_done);
    assert_eq!(tool.seen.session_size, Some((64, 48)));
    assert!(
        tool.seen
            .session_formats
            .contains(&(wl_shm::Format::Xrgb8888 as u32))
    );

    let (buffer, memory) = tool.buffer(64, 48);
    let frame = session.create_frame(&qh, ());
    frame.attach_buffer(&buffer);
    frame.damage_buffer(0, 0, 64, 48);
    frame.capture();
    pump(&mut compositor, &mut tool, |seen| seen.ready || seen.failed);
    assert!(tool.seen.ready, "the frame was copied");
    assert_eq!(tool.seen.transform, Some(0));
    assert_eq!(tool.seen.damage, vec![(0, 0, 64, 48)]);
    assert!(near(pixel(&memory, 64, 0, 0), BACKGROUND));
    assert!(near(pixel(&memory, 64, 12, 12), RED));
    frame.destroy();

    // A buffer of the wrong size fails the frame, not the client.
    tool.seen.ready = false;
    let (small, _memory) = tool.buffer(16, 16);
    let frame = session.create_frame(&qh, ());
    frame.attach_buffer(&small);
    frame.damage_buffer(0, 0, 16, 16);
    frame.capture();
    compositor.add_host(Box::new(Square::new((40, 30, 8, 8), GREEN)));
    pump(&mut compositor, &mut tool, |seen| seen.ready || seen.failed);
    assert!(tool.seen.failed);
}

#[test]
fn a_trusted_tool_picks_a_window_from_the_list_and_copies_only_it() {
    let mut compositor = compositor();
    let mut tool = trusted_tool(&mut compositor);
    let _window = open_window(&mut compositor, &mut tool, 20, 10, GREEN);
    pump(&mut compositor, &mut tool, |seen| {
        seen.toplevels
            .iter()
            .any(|(_, app_id)| app_id == "org.example.shared")
    });
    let qh = tool.queue.handle();
    let handle = tool.seen.toplevels[0].0.clone();
    let source = tool
        .seen
        .window_sources
        .clone()
        .unwrap()
        .create_source(&handle, &qh, ());
    let session =
        tool.seen
            .image_copy
            .clone()
            .unwrap()
            .create_session(&source, Options::empty(), &qh, ());
    pump(&mut compositor, &mut tool, |seen| seen.session_done);
    assert_eq!(tool.seen.session_size, Some((20, 10)), "the window's size");

    let (buffer, memory) = tool.buffer(20, 10);
    let frame = session.create_frame(&qh, ());
    frame.attach_buffer(&buffer);
    frame.damage_buffer(0, 0, 20, 10);
    frame.capture();
    pump(&mut compositor, &mut tool, |seen| seen.ready || seen.failed);
    assert!(tool.seen.ready, "the frame was copied");
    assert!(near(pixel(&memory, 20, 0, 0), GREEN));
    assert!(near(pixel(&memory, 20, 19, 9), GREEN));
}

#[test]
fn a_cursor_session_carries_the_pointer_image_and_where_it_is() {
    let mut compositor = compositor();
    let mut tool = trusted_tool(&mut compositor);
    pump(&mut compositor, &mut tool, |seen| seen.seat.is_some());
    let qh = tool.queue.handle();
    let pointer = tool.seen.seat.as_ref().unwrap().get_pointer(&qh, ());
    let source = tool.seen.sources.clone().unwrap().create_source(
        tool.seen.output.as_ref().unwrap(),
        &qh,
        (),
    );
    compositor
        .input(InputEvent::PointerMotion { x: 30.0, y: 20.0 })
        .unwrap();
    let cursor = tool
        .seen
        .image_copy
        .clone()
        .unwrap()
        .create_pointer_cursor_session(&source, &pointer, &qh, ());
    let session = cursor.get_capture_session(&qh, ());
    pump(&mut compositor, &mut tool, |seen| {
        seen.session_done && seen.cursor.len() >= 3
    });
    assert_eq!(tool.seen.cursor[0], "enter");
    assert_eq!(tool.seen.cursor[1], "position 30 20");
    let hotspot: Vec<i32> = tool.seen.cursor[2]
        .strip_prefix("hotspot ")
        .expect("a hotspot")
        .split(' ')
        .map(|n| n.parse().unwrap())
        .collect();
    let (width, height) = tool.seen.session_size.expect("the image's size");
    assert!(
        width > 1 && height > 1,
        "the pointer's image, not the output"
    );
    let (width, height) = (
        i32::try_from(width).unwrap(),
        i32::try_from(height).unwrap(),
    );

    let (buffer, memory) = tool.buffer_of(width, height, wl_shm::Format::Argb8888);
    let frame = session.create_frame(&qh, ());
    frame.attach_buffer(&buffer);
    frame.damage_buffer(0, 0, width, height);
    frame.capture();
    pump(&mut compositor, &mut tool, |seen| seen.ready || seen.failed);
    assert!(tool.seen.ready, "the image was copied");
    let mut image = vec![0_u8; usize::try_from(width * height * 4).unwrap()];
    rustix::io::pread(&memory, &mut image, 0).unwrap();
    assert!(
        image.chunks_exact(4).any(|pixel| pixel[3] == 255),
        "the image shows the pointer"
    );
    assert!(
        image.chunks_exact(4).any(|pixel| pixel[3] == 0),
        "around the pointer is clear"
    );
    let at = usize::try_from((hotspot[1] * width + hotspot[0]) * 4).unwrap();
    assert!(image[at + 3] > 0, "the hotspot lies on the pointer");

    // Moving says where; the image is not copied again.
    tool.seen.cursor.clear();
    compositor
        .input(InputEvent::PointerMotion { x: 40.0, y: 10.0 })
        .unwrap();
    pump(&mut compositor, &mut tool, |seen| !seen.cursor.is_empty());
    assert_eq!(tool.seen.cursor, vec!["position 40 10".to_owned()]);
}

/// The pointer's image size as a cursor session copies it, from a
/// compositor showing it at `size` logical pixels.
fn pointer_image_size(compositor: &mut Compositor, size: u32) -> (u32, u32) {
    let mut a11y = compositor.a11y().clone();
    a11y.cursor.size = size;
    compositor.set_a11y(a11y).expect("a pointer size");
    let mut tool = trusted_tool(compositor);
    pump(compositor, &mut tool, |seen| seen.seat.is_some());
    let qh = tool.queue.handle();
    let pointer = tool.seen.seat.as_ref().unwrap().get_pointer(&qh, ());
    let source = tool.seen.sources.clone().unwrap().create_source(
        tool.seen.output.as_ref().unwrap(),
        &qh,
        (),
    );
    compositor
        .input(InputEvent::PointerMotion { x: 30.0, y: 20.0 })
        .unwrap();
    let cursor = tool
        .seen
        .image_copy
        .clone()
        .unwrap()
        .create_pointer_cursor_session(&source, &pointer, &qh, ());
    let _session = cursor.get_capture_session(&qh, ());
    pump(compositor, &mut tool, |seen| seen.session_size.is_some());
    tool.seen.session_size.expect("the pointer's size")
}

#[test]
fn a_bigger_pointer_is_the_size_the_person_asked_for() {
    let mut compositor = compositor();
    let plain = pointer_image_size(&mut compositor, 24);
    let large = pointer_image_size(&mut compositor, 48);
    assert!(
        large.0 > plain.0 && large.1 > plain.1,
        "the pointer is drawn bigger: {plain:?} then {large:?}"
    );
}

#[test]
fn a_window_capture_shows_the_pointer_over_it_when_asked() {
    let mut compositor = compositor();
    let mut tool = trusted_tool(&mut compositor);
    let _window = open_window(&mut compositor, &mut tool, 40, 30, GREEN);
    pump(&mut compositor, &mut tool, |seen| {
        !seen.toplevels.is_empty()
    });
    let (_, geometry) = compositor.windows()[0];
    let qh = tool.queue.handle();
    let handle = tool.seen.toplevels[0].0.clone();
    let source = tool
        .seen
        .window_sources
        .clone()
        .unwrap()
        .create_source(&handle, &qh, ());
    let session = tool.seen.image_copy.clone().unwrap().create_session(
        &source,
        Options::PaintCursors,
        &qh,
        (),
    );
    pump(&mut compositor, &mut tool, |seen| seen.session_done);
    let copy = |compositor: &mut Compositor, tool: &mut Tool| {
        tool.seen.ready = false;
        let (buffer, memory) = tool.buffer(40, 30);
        let frame = session.create_frame(&tool.queue.handle(), ());
        frame.attach_buffer(&buffer);
        frame.damage_buffer(0, 0, 40, 30);
        frame.capture();
        pump(compositor, tool, |seen| seen.ready || seen.failed);
        assert!(tool.seen.ready);
        frame.destroy();
        let mut image = vec![0_u8; 40 * 30 * 4];
        rustix::io::pread(&memory, &mut image, 0).unwrap();
        image
    };
    let green = |image: &[u8]| {
        image
            .chunks_exact(4)
            .filter(|pixel| near([pixel[0], pixel[1], pixel[2]], GREEN))
            .count()
    };

    // The pointer away from the window: all of it is the window.
    compositor
        .input(InputEvent::PointerMotion { x: 1.0, y: 47.0 })
        .unwrap();
    let away = copy(&mut compositor, &mut tool);
    assert_eq!(green(&away), 40 * 30);

    // Over it, the pointer covers part of it.
    compositor
        .input(InputEvent::PointerMotion {
            x: f64::from(geometry.0 + 10),
            y: f64::from(geometry.1 + 10),
        })
        .unwrap();
    let over = copy(&mut compositor, &mut tool);
    assert!(
        green(&over) < 40 * 30,
        "the pointer is drawn over the window"
    );
    let at = (10 * 40 + 10) * 4;
    assert!(!near([over[at], over[at + 1], over[at + 2]], GREEN));
}

impl Dispatch<WlRegistry, ()> for Seen {
    fn event(
        seen: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            seen.globals.push(interface.clone());
            match interface.as_str() {
                "wl_shm" => seen.shm = Some(registry.bind(name, 1, qh, ())),
                "wl_output" => seen.output = Some(registry.bind(name, 1, qh, ())),
                "zwlr_screencopy_manager_v1" => {
                    seen.screencopy = Some(registry.bind(name, 3, qh, ()));
                }
                "ext_image_copy_capture_manager_v1" => {
                    seen.image_copy = Some(registry.bind(name, 1, qh, ()));
                }
                "ext_output_image_capture_source_manager_v1" => {
                    seen.sources = Some(registry.bind(name, 1, qh, ()));
                }
                "ext_foreign_toplevel_image_capture_source_manager_v1" => {
                    seen.window_sources = Some(registry.bind(name, 1, qh, ()));
                }
                "ext_foreign_toplevel_list_v1" => {
                    seen.toplevel_list = Some(registry.bind(name, 1, qh, ()));
                }
                "wl_compositor" => seen.compositor = Some(registry.bind(name, 4, qh, ())),
                "xdg_wm_base" => seen.wm_base = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" => seen.seat = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                let format = match format {
                    WEnum::Value(format) => format as u32,
                    WEnum::Unknown(format) => format,
                };
                seen.buffer = Some((format, width, height, stride));
            }
            zwlr_screencopy_frame_v1::Event::BufferDone => seen.buffer_done = true,
            zwlr_screencopy_frame_v1::Event::Damage {
                x,
                y,
                width,
                height,
            } => seen.damage.push((x, y, width, height)),
            zwlr_screencopy_frame_v1::Event::Ready { .. } => seen.ready = true,
            zwlr_screencopy_frame_v1::Event::Failed => seen.failed = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                seen.session_size = Some((width, height));
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat { format } => {
                seen.session_formats.push(match format {
                    WEnum::Value(format) => format as u32,
                    WEnum::Unknown(format) => format,
                });
            }
            ext_image_copy_capture_session_v1::Event::Done => seen.session_done = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_frame_v1::Event::Transform { transform } => {
                seen.transform = Some(match transform {
                    WEnum::Value(transform) => transform as u32,
                    WEnum::Unknown(transform) => transform,
                });
            }
            ext_image_copy_capture_frame_v1::Event::Damage {
                x,
                y,
                width,
                height,
            } => seen.damage.push((
                u32::try_from(x).unwrap(),
                u32::try_from(y).unwrap(),
                u32::try_from(width).unwrap(),
                u32::try_from(height).unwrap(),
            )),
            ext_image_copy_capture_frame_v1::Event::Ready => seen.ready = true,
            ext_image_copy_capture_frame_v1::Event::Failed { .. } => seen.failed = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            seen.toplevels.push((toplevel, String::new()));
        }
    }

    event_created_child!(Seen, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_handle_v1::Event::AppId { app_id } = event
            && let Some(listed) = seen
                .toplevels
                .iter_mut()
                .find(|(listed, _)| listed == handle)
        {
            listed.1 = app_id;
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Seen {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
        event: xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Seen {
    fn event(
        seen: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            seen.configured = true;
        }
    }
}

impl Dispatch<ExtImageCopyCaptureCursorSessionV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ExtImageCopyCaptureCursorSessionV1,
        event: ext_image_copy_capture_cursor_session_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        seen.cursor.push(match event {
            ext_image_copy_capture_cursor_session_v1::Event::Enter => "enter".to_owned(),
            ext_image_copy_capture_cursor_session_v1::Event::Leave => "leave".to_owned(),
            ext_image_copy_capture_cursor_session_v1::Event::Position { x, y } => {
                format!("position {x} {y}")
            }
            ext_image_copy_capture_cursor_session_v1::Event::Hotspot { x, y } => {
                format!("hotspot {x} {y}")
            }
            _ => return,
        });
    }
}

delegate_noop!(Seen: ignore WlSeat);
delegate_noop!(Seen: ignore WlPointer);
delegate_noop!(Seen: ignore WlShm);
delegate_noop!(Seen: ignore WlCompositor);
delegate_noop!(Seen: ignore WlSurface);
delegate_noop!(Seen: ignore XdgToplevel);
delegate_noop!(Seen: ExtForeignToplevelImageCaptureSourceManagerV1);
delegate_noop!(Seen: ignore WlOutput);
delegate_noop!(Seen: ignore WlShmPool);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore ZwlrScreencopyManagerV1);
delegate_noop!(Seen: ExtImageCopyCaptureManagerV1);
delegate_noop!(Seen: ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(Seen: ExtImageCaptureSourceV1);
