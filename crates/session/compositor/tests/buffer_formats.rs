//! Every buffer a client can hand the compositor in a format it offered shows: a plain
//! shared-memory client in each `wl_shm` format, and a real GL client (the machine's EGL over
//! Wayland, Mesa's software rasterizer where there is no GPU) in every colour depth its EGL offers
//! a window. A format the compositor offers but cannot draw would leave the window blank, and a GL
//! client picks the deepest colour its EGL offers, so a 10-bit format offered and not drawn hides
//! every GL app.

#![cfg(target_os = "linux")]

use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tilo_compositor::{Compositor, StackPolicy};
use wayland_client::protocol::wl_shm;
use wayland_client::{Connection, Proxy as _};

use crate::harness::{Seen, TestClient, headless, pump};

/// The colour each window is filled with: values exact in every format below, 8-bit, 10-bit and
/// half float.
const RED: f32 = 0.75;
const GREEN: f32 = 0.25;
const BLUE: f32 = 0.5;

/// The colour as the composited 8-bit frame holds it.
const SHOWN: [u8; 3] = [191, 64, 128];

/// How far a channel may be from [`SHOWN`]: rounding of a deeper format to 8 bits.
const TOLERANCE: u8 = 3;

/// The middle of the one output, where [`StackPolicy`] centres a window.
const MIDDLE: (u32, u32) = (320, 240);

fn close(pixel: [u8; 4]) -> bool {
    pixel
        .iter()
        .zip(SHOWN)
        .all(|(&got, want)| got.abs_diff(want) <= TOLERANCE)
}

/// A pixel of `format` holding [`RED`], [`GREEN`], [`BLUE`] and full alpha, in memory order.
#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "the channels are between 0 and 1, scaled to their bit depth"
)]
fn pixel(format: wl_shm::Format) -> Vec<u8> {
    use wl_shm::Format as F;
    let eight = |v: f32| u32::from((v * 255.0).round() as u8);
    let ten = |v: f32| (v * 1023.0).round() as u32;
    let half = |v: f32| -> [u8; 2] {
        // The three channel values and 1.0 are exact halves.
        let bits: u16 = match v.to_bits() {
            bits if bits == 0.25_f32.to_bits() => 0x3400,
            bits if bits == 0.5_f32.to_bits() => 0x3800,
            bits if bits == 0.75_f32.to_bits() => 0x3a00,
            bits if bits == 1.0_f32.to_bits() => 0x3c00,
            _ => panic!("{v} is not one of the exact halves"),
        };
        bits.to_le_bytes()
    };
    let (r, g, b) = (RED, GREEN, BLUE);
    match format {
        F::Argb8888 | F::Xrgb8888 => ((0xff << 24) | (eight(r) << 16) | (eight(g) << 8) | eight(b))
            .to_le_bytes()
            .to_vec(),
        F::Abgr8888 | F::Xbgr8888 => ((0xff << 24) | (eight(b) << 16) | (eight(g) << 8) | eight(r))
            .to_le_bytes()
            .to_vec(),
        F::Argb2101010 | F::Xrgb2101010 => {
            ((0b11 << 30) | (ten(r) << 20) | (ten(g) << 10) | ten(b))
                .to_le_bytes()
                .to_vec()
        }
        F::Abgr2101010 | F::Xbgr2101010 => {
            ((0b11 << 30) | (ten(b) << 20) | (ten(g) << 10) | ten(r))
                .to_le_bytes()
                .to_vec()
        }
        F::Abgr16161616f | F::Xbgr16161616f => [half(r), half(g), half(b), half(1.0)].concat(),
        F::Argb16161616f | F::Xrgb16161616f => [half(b), half(g), half(r), half(1.0)].concat(),
        other => panic!("the compositor offers {other:?}: teach this test its layout"),
    }
}

/// Let the compositor run until the middle of the output shows what `wanted` accepts, for at most
/// two seconds; what it showed last if it never did.
fn wait_for(
    compositor: &mut Compositor,
    mut exchange: impl FnMut(),
    wanted: impl Fn([u8; 4]) -> bool,
) -> Result<(), [u8; 4]> {
    let began = Instant::now();
    let mut last = [0; 4];
    while began.elapsed() < Duration::from_secs(2) {
        exchange();
        compositor
            .dispatch(Duration::from_millis(5))
            .expect("dispatch");
        compositor.draw_frame().expect("a frame");
        let frame = compositor.screenshot("HEADLESS-1").expect("the frame");
        last = frame.pixel(MIDDLE.0, MIDDLE.1).expect("the middle");
        if wanted(last) {
            return Ok(());
        }
    }
    Err(last)
}

fn black(pixel: [u8; 4]) -> bool {
    pixel.iter().take(3).all(|&channel| channel <= TOLERANCE)
}

#[test]
fn a_buffer_in_every_offered_shm_format_shows() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = TestClient::connect(&mut compositor);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.wm_base.is_some()
            && clients[0].seen.shm.is_some()
            && clients[0].seen.seat.is_some()
    });
    // The formats arrive right after the bind.
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.shm_formats.len() >= 2
    });
    client.open_window("formats");
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.configured
    });
    let formats = client.seen.shm_formats.clone();
    let mut blank = Vec::new();
    for format in &formats {
        // Black first, so what shows next is this format's buffer and not the last one's.
        client.show(200, 100, [0, 0, 0, 255]);
        wait_for(&mut compositor, || client.exchange(), black).expect("the black buffer shows");
        client.show_pixels(200, 100, *format, &pixel(*format));
        if let Err(seen) = wait_for(&mut compositor, || client.exchange(), close) {
            blank.push(format!("{format:?} showed {seen:?}"));
        }
    }
    assert!(
        blank.is_empty(),
        "offered shm formats whose buffers do not show {SHOWN:?} (of {formats:?}): {blank:?}"
    );
}

/// What the GL client says to the test, and the test answers.
enum Gl {
    /// A window of this colour depth committed its first frame.
    Drew(String),
    /// The client could not draw one: why.
    Failed(String),
    /// Every depth was drawn.
    Done(usize),
}

#[test]
fn a_gl_client_shows_in_every_colour_depth_its_egl_offers() {
    let mut compositor = headless(Box::new(StackPolicy));
    let socket = compositor.connect_client().expect("client socket");
    let (said, heard) = mpsc::channel();
    let (go_on, wait) = mpsc::channel::<()>();
    let client = std::thread::spawn(move || gl_client(socket, &said, &wait));
    let mut blank = Vec::new();
    let began = Instant::now();
    let drawn = loop {
        assert!(
            began.elapsed() < Duration::from_secs(120),
            "the GL client stopped answering"
        );
        compositor
            .dispatch(Duration::from_millis(5))
            .expect("dispatch");
        match heard.try_recv() {
            Ok(Gl::Drew(depth)) => {
                if let Err(seen) = wait_for(&mut compositor, || {}, close) {
                    blank.push(format!("{depth} showed {seen:?}"));
                }
                go_on.send(()).expect("the client waits");
            }
            Ok(Gl::Failed(why)) => panic!("the GL client failed: {why}"),
            Ok(Gl::Done(count)) => break count,
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => panic!("the GL client ended early"),
        }
    };
    client.join().expect("the GL client");
    assert!(drawn > 0, "the machine's EGL offered no window config");
    assert!(
        blank.is_empty(),
        "GL windows that do not show {SHOWN:?}: {blank:?}"
    );
}

/// A GL client: for every colour depth the machine's EGL offers a Wayland window, one window
/// cleared to the test's colour, kept up until the test looked at it.
fn gl_client(socket: UnixStream, said: &mpsc::Sender<Gl>, wait: &mpsc::Receiver<()>) {
    if let Err(why) = gl_windows(socket, said, wait) {
        let _ = said.send(Gl::Failed(why));
    }
}

/// The GLES 2 calls the client draws with.
struct Gl2 {
    clear_color: extern "system" fn(f32, f32, f32, f32),
    clear: extern "system" fn(u32),
}

const COLOR_BUFFER_BIT: u32 = 0x4000;

type Egl = khronos_egl::DynamicInstance<khronos_egl::EGL1_4>;

fn gl_windows(
    socket: UnixStream,
    said: &mpsc::Sender<Gl>,
    wait: &mpsc::Receiver<()>,
) -> Result<(), String> {
    let connection = Connection::from_socket(socket).map_err(|e| e.to_string())?;
    let queue = connection.new_event_queue();
    connection.display().get_registry(&queue.handle(), ());
    let mut client = TestClient {
        connection: connection.clone(),
        queue,
        seen: Seen::default(),
        surface: None,
        toplevel: None,
    };
    client
        .queue
        .roundtrip(&mut client.seen)
        .map_err(|e| e.to_string())?;

    // SAFETY: loads the system's libEGL; nothing else in this thread holds an EGL instance.
    let lib = unsafe { Egl::load_required() }.map_err(|e| format!("libEGL: {e}"))?;
    let wl_display = connection.backend().display_ptr();
    // SAFETY: the pointer is this connection's live `wl_display`, which outlives the EGL display
    // (terminated below, before the connection drops).
    let display = unsafe { lib.get_display(wl_display.cast()) }.ok_or("no EGL display")?;
    lib.initialize(display).map_err(|e| e.to_string())?;
    lib.bind_api(khronos_egl::OPENGL_ES_API)
        .map_err(|e| e.to_string())?;
    let depths = window_depths(&lib, display)?;
    // SAFETY: names of core GLES 2 entry points, with their C signatures.
    let gl = unsafe {
        Gl2 {
            clear_color: std::mem::transmute::<
                extern "system" fn(),
                extern "system" fn(f32, f32, f32, f32),
            >(
                lib.get_proc_address("glClearColor")
                    .ok_or("no glClearColor")?,
            ),
            clear: std::mem::transmute::<extern "system" fn(), extern "system" fn(u32)>(
                lib.get_proc_address("glClear").ok_or("no glClear")?,
            ),
        }
    };
    for (index, (depth, config)) in depths.iter().enumerate() {
        let label = format!("r{} g{} b{} a{}", depth[0], depth[1], depth[2], depth[3]);
        let drawn = draw_window(
            &mut client,
            (&lib, display, *config, &gl),
            &format!("gl-{index}"),
            &label,
        )?;
        said.send(Gl::Drew(label)).map_err(|e| e.to_string())?;
        wait.recv().map_err(|e| e.to_string())?;
        // Kept until the test looked: the frame's buffer is the EGL surface's.
        drawn.end(&lib, display)?;
        // Unmapped, so the next window is the only one in the middle.
        let surface = client.surface.clone().ok_or("no surface")?;
        surface.attach(None, 0, 0);
        surface.commit();
        client
            .queue
            .roundtrip(&mut client.seen)
            .map_err(|e| e.to_string())?;
    }
    lib.terminate(display).map_err(|e| e.to_string())?;
    said.send(Gl::Done(depths.len()))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// One config of each colour depth the machine's EGL offers a GLES 2 window in: configs differing
/// only in depth, stencil or samples draw into the same kind of buffer.
fn window_depths(
    lib: &Egl,
    display: khronos_egl::Display,
) -> Result<Vec<([i32; 4], khronos_egl::Config)>, String> {
    use khronos_egl as egl;
    let wanted = [
        egl::SURFACE_TYPE,
        egl::WINDOW_BIT,
        egl::RENDERABLE_TYPE,
        egl::OPENGL_ES2_BIT,
        egl::RED_SIZE,
        1,
        egl::GREEN_SIZE,
        1,
        egl::BLUE_SIZE,
        1,
        egl::NONE,
    ];
    let count = lib
        .matching_config_count(display, &wanted)
        .map_err(|e| e.to_string())?;
    let mut configs = Vec::with_capacity(count);
    lib.choose_config(display, &wanted, &mut configs)
        .map_err(|e| e.to_string())?;
    let mut depths: Vec<([i32; 4], egl::Config)> = Vec::new();
    for config in configs {
        let attrib = |name| lib.get_config_attrib(display, config, name).unwrap_or(0);
        if attrib(egl::SAMPLES) != 0 {
            continue;
        }
        let depth = [
            attrib(egl::RED_SIZE),
            attrib(egl::GREEN_SIZE),
            attrib(egl::BLUE_SIZE),
            attrib(egl::ALPHA_SIZE),
        ];
        if !depths.iter().any(|(seen, _)| *seen == depth) {
            depths.push((depth, config));
        }
    }
    Ok(depths)
}

/// A window a GL client drew into, while its EGL surface and context last.
struct Drawn {
    window: wayland_egl::WlEglSurface,
    surface: khronos_egl::Surface,
    context: khronos_egl::Context,
}

impl Drawn {
    fn end(self, lib: &Egl, display: khronos_egl::Display) -> Result<(), String> {
        lib.make_current(display, None, None, None)
            .map_err(|e| e.to_string())?;
        lib.destroy_context(display, self.context)
            .map_err(|e| e.to_string())?;
        lib.destroy_surface(display, self.surface)
            .map_err(|e| e.to_string())?;
        drop(self.window);
        Ok(())
    }
}

/// Open a window as `app_id` and draw one frame of the test's colour into it with `config`, as a
/// GL app does: an EGL window surface, a GLES 2 context, a clear and a swap.
fn draw_window(
    client: &mut TestClient,
    (lib, display, config, gl): (&Egl, khronos_egl::Display, khronos_egl::Config, &Gl2),
    app_id: &str,
    label: &str,
) -> Result<Drawn, String> {
    use khronos_egl as egl;
    client.seen.configured = false;
    client.open_window(app_id);
    while !client.seen.configured {
        client
            .queue
            .blocking_dispatch(&mut client.seen)
            .map_err(|e| e.to_string())?;
    }
    let surface = client.surface.clone().ok_or("no surface")?;
    let window =
        wayland_egl::WlEglSurface::new(surface.id(), 200, 100).map_err(|e| e.to_string())?;
    // SAFETY: the native window is the `wl_egl_window` above, alive until the surface is
    // destroyed below.
    let egl_surface =
        unsafe { lib.create_window_surface(display, config, window.ptr().cast_mut(), None) }
            .map_err(|e| format!("{label}: window surface: {e}"))?;
    let context = lib
        .create_context(
            display,
            config,
            None,
            &[egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE],
        )
        .map_err(|e| format!("{label}: context: {e}"))?;
    lib.make_current(display, Some(egl_surface), Some(egl_surface), Some(context))
        .map_err(|e| format!("{label}: make current: {e}"))?;
    lib.swap_interval(display, 0).map_err(|e| e.to_string())?;
    (gl.clear_color)(RED, GREEN, BLUE, 1.0);
    (gl.clear)(COLOR_BUFFER_BIT);
    lib.swap_buffers(display, egl_surface)
        .map_err(|e| format!("{label}: swap: {e}"))?;
    client.connection.flush().map_err(|e| e.to_string())?;
    Ok(Drawn {
        window,
        surface: egl_surface,
        context,
    })
}
