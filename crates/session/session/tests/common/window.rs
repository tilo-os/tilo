//! A window on a device's desk, as an app's is: a Wayland client of this test
//! process's own connects to the device's session and maps one toplevel, which
//! the session's desk places as a window of the workspace in front.
//!
//! A headless session draws no display to see, but it serves a display to its
//! apps all the same (`WAYLAND_DISPLAY`, in the device's runtime directory):
//! what a package's window connects to when the launcher starts it. Opening
//! one here is the same mapping a toolkit's window makes, without the app.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test fixture: a step it cannot take is the failure the test reads"
)]

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

/// What the client heard from its session.
#[derive(Default)]
struct Seen {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    configured: bool,
}

/// One window of one device's desk, up while this value lives.
pub struct Window {
    connection: Connection,
    queue: EventQueue<Seen>,
    seen: Seen,
    _surface: WlSurface,
    _toplevel: XdgToplevel,
    /// The window's contents, which it keeps showing: a client that lets its
    /// buffer go shows nothing, and the session takes the window back.
    _buffer: WlBuffer,
}

impl Window {
    /// Map a window of `app` called `title` on the desk of the session whose
    /// device directory is `device`.
    ///
    /// # Panics
    /// The device's session serves no display, or the window is not placed.
    pub fn open(device: &Path, app: &str, title: &str) -> Self {
        let stream = UnixStream::connect(display(device)).unwrap();
        stream
            .set_nonblocking(true)
            .expect("a window's connection reads what is there");
        let connection = Connection::from_socket(stream).expect("a connection to the display");
        let mut queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        let mut seen = Seen::default();
        let qh = queue.handle();
        let deadline = Instant::now() + Duration::from_secs(30);
        // The session's globals, then the window's configuration: what a
        // toolkit reads before it draws its first frame.
        let surface = loop {
            exchange(&connection, &mut queue, &mut seen);
            if let (Some(compositor), Some(wm_base)) = (&seen.compositor, &seen.wm_base) {
                let surface = compositor.create_surface(&qh, ());
                let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
                let toplevel = xdg.get_toplevel(&qh, ());
                toplevel.set_app_id(app.to_owned());
                toplevel.set_title(title.to_owned());
                surface.commit();
                break (surface, toplevel);
            }
            assert!(
                Instant::now() < deadline,
                "{}'s session announced no compositor and xdg shell",
                device.display()
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        let (surface, toplevel) = surface;
        while !seen.configured {
            exchange(&connection, &mut queue, &mut seen);
            assert!(
                Instant::now() < deadline,
                "the session never configured the window of {app}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let buffer = paint(&connection, &mut queue, &seen, &surface);
        // The compositor maps a toplevel on the commit that gives it
        // contents.
        let _ = connection.flush();
        Self {
            connection,
            queue,
            seen,
            _surface: surface,
            _toplevel: toplevel,
            _buffer: buffer,
        }
    }

    /// Send what this client queued and take what arrived, without waiting
    /// for more: a session stopped under a live window is the test's to
    /// notice, not this loop's.
    pub fn exchange(&mut self) {
        exchange(&self.connection, &mut self.queue, &mut self.seen);
    }
}

/// One turn of the client: what it queued out, what the session sent back in.
fn exchange(connection: &Connection, queue: &mut EventQueue<Seen>, seen: &mut Seen) {
    let _ = connection.flush();
    if let Some(guard) = connection.prepare_read() {
        let _ = guard.read();
    }
    let _ = queue.dispatch_pending(seen);
    let _ = connection.flush();
}

/// Attach one colour's buffer to `surface` and commit it.
fn paint(
    connection: &Connection,
    queue: &mut EventQueue<Seen>,
    seen: &Seen,
    surface: &WlSurface,
) -> WlBuffer {
    use std::io::Write as _;
    use std::os::fd::AsFd as _;

    let qh = queue.handle();
    let (width, height) = (320i32, 200i32);
    let stride = width * 4;
    let len = usize::try_from(stride * height).expect("a window's size");
    let fd = rustix::fs::memfd_create("tilo-window-test", rustix::fs::MemfdFlags::CLOEXEC)
        .expect("a buffer's memory");
    let mut file = std::fs::File::from(fd);
    // ARGB8888 is B, G, R, A in memory.
    file.write_all(&[0x40u8, 0x60, 0x80, 0xff].repeat(len / 4))
        .expect("the window's contents");
    let pool = seen
        .shm
        .as_ref()
        .expect("the session's shared memory")
        .create_pool(file.as_fd(), stride * height, &qh, ());
    let buffer = pool.create_buffer(
        0,
        width,
        height,
        stride,
        wayland_client::protocol::wl_shm::Format::Argb8888,
        &qh,
        (),
    );
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, width, height);
    surface.frame(&qh, ());
    surface.commit();
    pool.destroy();
    let _ = connection.flush();
    buffer
}

/// The display the device at `device` serves its apps, from its runtime
/// directory: the socket name is the session's own.
fn display(device: &Path) -> PathBuf {
    let runtime = device.join("runtime");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&runtime)
        .unwrap_or_else(|error| panic!("{}'s runtime directory: {error}", runtime.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|name| name.starts_with("wayland-") && !name.ends_with(".lock"))
        })
        .collect();
    found.sort();
    found
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("{} serves no display", device.display()))
}

impl Dispatch<WlRegistry, ()> for Seen {
    fn event(
        seen: &mut Self,
        registry: &WlRegistry,
        event: wayland_client::protocol::wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    seen.compositor = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "wl_shm" => seen.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => seen.wm_base = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
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

impl Dispatch<XdgToplevel, ()> for Seen {
    fn event(
        _: &mut Self,
        _: &XdgToplevel,
        _: xdg_toplevel::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlCallback, ()> for Seen {
    fn event(
        _: &mut Self,
        _: &WlCallback,
        _: wayland_client::protocol::wl_callback::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(Seen: ignore WlCompositor);
delegate_noop!(Seen: ignore WlShm);
delegate_noop!(Seen: ignore WlShmPool);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore WlSurface);
