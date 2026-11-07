//! A window on a session's desk, as an app's is: a Wayland client of this test
//! process's own connects to the session's display and maps a toplevel, which
//! the session's desk places as a window of the workspace in front.
//!
//! A headless session draws no display to see, but it serves a display to its
//! apps all the same (`WAYLAND_DISPLAY`, in the device's runtime directory):
//! what a package's window connects to when the launcher starts it. Opening
//! one here is the same mapping a toolkit's window makes, without the app.
//!
//! One client carries every toplevel a test maps, and hears what the session
//! says back: each window's configuration and size, its contents, the keys and
//! pointer buttons delivered to it, and whose keyboard it is. A test that opens
//! one window and a test that opens six read it the same way, by the toplevel's
//! index.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "a test fixture: a step it cannot take is the failure the test reads"
)]

use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tilo_compositor::{Compositor, InputEvent};
use tilo_session::Session;
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_keyboard::{self, WlKeyboard},
    wl_pointer::{self, WlPointer},
    wl_registry,
    wl_registry::WlRegistry,
    wl_seat::{self, WlSeat},
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

/// The colour the compositor paints a test window, as a person sees it.
pub const WINDOW: [u8; 4] = [40, 40, 200, 255];
/// [`WINDOW`] as the bytes of an `ARGB8888` buffer hold it.
pub const WINDOW_BYTES: [u8; 4] = [200, 40, 40, 255];

/// What a key sends, in evdev codes.
pub const KEY_ESCAPE: u32 = 1;
/// See [`KEY_ESCAPE`].
pub const KEY_A: u32 = 30;
/// The left button of a two-and-more button pointer.
pub const BTN_LEFT: u32 = 0x110;

/// What one toplevel of the client was told.
#[derive(Default, Clone, Debug)]
pub struct Toplevel {
    /// Whether the session configured it.
    pub configured: bool,
    /// The size the last configure named.
    pub size: Option<(i32, i32)>,
}

/// What the client heard from its session.
#[derive(Default)]
pub struct Seen {
    pub compositor: Option<WlCompositor>,
    pub shm: Option<WlShm>,
    pub wm_base: Option<XdgWmBase>,
    pub seat: Option<WlSeat>,
    pub keyboard: Option<WlKeyboard>,
    pub pointer: Option<WlPointer>,
    /// One entry for each toplevel the client opened, by its index.
    pub toplevels: Vec<Toplevel>,
    /// Whether the keyboard is on one of this client's surfaces.
    pub keyboard_focused: bool,
    /// Every key the client was delivered, as the window sees it.
    pub keys: Vec<(u32, bool)>,
    /// Every pointer button the client was delivered as pressed, in evdev
    /// codes.
    pub buttons: Vec<u32>,
    /// How many times the pointer came onto a window of this client.
    pub pointer_on_window: usize,
}

impl Seen {
    /// The toplevel at `index`, as the session last told it.
    #[track_caller]
    pub fn toplevel(&self, index: usize) -> &Toplevel {
        self.toplevels.get(index).expect("the client opened it")
    }

    /// Whether the toplevel at `index` was configured.
    pub fn ready(&self, index: usize) -> bool {
        self.toplevel(index).configured
    }

    /// The size the toplevel at `index` draws at, once the session named one
    /// a window can hold.
    pub fn size_at(&self, index: usize) -> Option<(i32, i32)> {
        self.toplevel(index)
            .size
            .filter(|(width, height)| *width > 0 && *height > 0)
    }

    /// How many times the key `keycode` was pressed.
    pub fn pressed(&self, keycode: u32) -> usize {
        self.keys
            .iter()
            .filter(|(key, pressed)| *key == keycode && *pressed)
            .count()
    }
}

/// The windows of one client on a session's desk, up while this value lives.
pub struct Client {
    connection: Connection,
    queue: EventQueue<Seen>,
    /// What the client heard: the session's answer to what this client sends.
    pub seen: Seen,
    windows: Vec<(WlSurface, XdgToplevel)>,
    /// The contents each window shows: a client that lets its buffer go shows
    /// nothing, and the session takes the window back.
    buffers: Vec<WlBuffer>,
}

impl Client {
    /// Connect to the display `compositor` serves, as a package's window
    /// connecting from the device's runtime directory does.
    ///
    /// The client has its session to step, so it reads what arrived without
    /// waiting for more: [`Self::exchange`] and [`Self::pump`] carry it on.
    pub fn connect(compositor: &mut Compositor) -> Self {
        Self::of(compositor.connect_client().expect("client socket"))
    }

    /// Map a window of `app` called `title` on the desk of the session whose
    /// device directory is `device`, waiting until the desk places it.
    ///
    /// # Panics
    /// The device's session serves no display, or the window is not placed.
    pub fn open(device: &Path, app: &str, title: &str) -> Self {
        Self::on(&display(device), app, title)
    }

    /// Map a window of `app` called `title` on the display at `socket`: what a
    /// package's window connects to, named by its own path rather than a
    /// device's runtime directory.
    ///
    /// The test drives no session of its own here, so the client waits for its
    /// globals, its configuration and its first frame itself.
    ///
    /// # Panics
    /// The display refuses the connection, or the window is not placed.
    pub fn on(socket: &Path, app: &str, title: &str) -> Self {
        let stream = UnixStream::connect(socket).expect("a window's connection");
        stream
            .set_nonblocking(true)
            .expect("a window's connection reads what is there");
        let mut client = Self::of(stream);
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(30))
            .expect("a window's deadline");
        while client.seen.compositor.is_none()
            || client.seen.wm_base.is_none()
            || client.seen.shm.is_none()
        {
            let _ = client.connection.flush();
            if let Some(guard) = client.connection.prepare_read() {
                let _ = guard.read();
            }
            let _ = client.queue.dispatch_pending(&mut client.seen);
            assert!(
                Instant::now() < deadline,
                "{}'s session announced no compositor and xdg shell",
                socket.display()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let index = client.open_toplevel_with(app, |toplevel| toplevel.set_title(title.to_owned()));
        while !client.seen.ready(index) {
            client.exchange();
            assert!(
                Instant::now() < deadline,
                "the session never configured the window of {app}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let (width, height) = client.seen.size_at(index).unwrap_or((320, 200));
        client.show(index, width, height, WINDOW_BYTES);
        // The compositor maps a toplevel on the commit that gives it contents.
        let _ = client.connection.flush();
        client
    }

    fn of(socket: UnixStream) -> Self {
        let connection = Connection::from_socket(socket).expect("a connection to the display");
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        Self {
            connection,
            queue,
            seen: Seen::default(),
            windows: Vec::new(),
            buffers: Vec::new(),
        }
    }

    /// Send what this client queued and take what arrived, without waiting for
    /// more: a session stopped under a live window is the test's to notice,
    /// not this loop's.
    pub fn exchange(&mut self) {
        let _ = self.connection.flush();
        if let Some(guard) = self.connection.prepare_read() {
            // Nothing to read is fine: WouldBlock.
            let _ = guard.read();
        }
        let _ = self.queue.dispatch_pending(&mut self.seen);
        let _ = self.connection.flush();
    }

    /// Step the session and the client together until `done` holds.
    #[track_caller]
    pub fn pump(
        &mut self,
        session: &mut Session,
        mut done: impl FnMut(&mut Session, &Seen) -> bool,
    ) {
        pump(session, &mut [self], |session, clients| {
            done(session, &clients[0].seen)
        });
    }

    /// Open toplevel number `self.windows.len()` of `app`. Returns its index.
    ///
    /// # Panics
    /// The client heard no compositor or xdg shell yet.
    pub fn open_toplevel(&mut self, app: &str) -> usize {
        self.open_toplevel_with(app, |_| {})
    }

    /// [`Self::open_toplevel`], set up by `before` before the first commit: what a
    /// dialog names its parent, and what a window calls itself.
    ///
    /// # Panics
    /// The client heard no compositor or xdg shell yet.
    pub fn open_toplevel_with(&mut self, app: &str, before: impl FnOnce(&XdgToplevel)) -> usize {
        let index = self.windows.len();
        let qh = self.queue.handle();
        let surface = self
            .seen
            .compositor
            .as_ref()
            .expect("wl_compositor")
            .create_surface(&qh, ());
        let xdg = self
            .seen
            .wm_base
            .as_ref()
            .expect("xdg_wm_base")
            .get_xdg_surface(&surface, &qh, index);
        let toplevel = xdg.get_toplevel(&qh, index);
        toplevel.set_app_id(app.to_owned());
        before(&toplevel);
        surface.commit();
        self.seen.toplevels.push(Toplevel::default());
        self.windows.push((surface, toplevel));
        index
    }

    /// The toplevel at `index`, for a test that asks it something (a popup's
    /// parent, a rectangle to grab).
    #[track_caller]
    pub fn toplevel(&self, index: usize) -> &XdgToplevel {
        &self.windows.get(index).expect("the client opened it").1
    }

    /// Attach a buffer of `colour` to the toplevel at `index` and commit it.
    ///
    /// # Panics
    /// The client heard no shared memory, or opened no toplevel at `index`.
    pub fn show(&mut self, index: usize, width: i32, height: i32, colour: [u8; 4]) {
        let qh = self.queue.handle();
        let stride = width * 4;
        let len = usize::try_from(stride * height).expect("a window's size");
        let fd = rustix::fs::memfd_create("tilo-window-test", rustix::fs::MemfdFlags::CLOEXEC)
            .expect("a buffer's memory");
        let mut file = std::fs::File::from(fd);
        file.write_all(&colour.repeat(len / 4))
            .expect("the window's contents");
        let pool = self.seen.shm.as_ref().expect("wl_shm").create_pool(
            file.as_fd(),
            stride * height,
            &qh,
            (),
        );
        let buffer =
            pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, &qh, ());
        let surface = &self.windows.get(index).expect("the client opened it").0;
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.commit();
        pool.destroy();
        let _ = self.connection.flush();
        self.buffers.push(buffer);
    }

    /// Open the toplevel of `app` (set up by `before`), draw it at the size it
    /// was configured to, else `fallback`, and wait until the desk shows it.
    #[track_caller]
    pub fn map(
        &mut self,
        session: &mut Session,
        app: &str,
        fallback: (i32, i32),
        before: impl FnOnce(&XdgToplevel),
    ) -> usize {
        let index = self.open_toplevel_with(app, before);
        self.pump(session, |_, seen| seen.ready(index));
        let (width, height) = self.seen.size_at(index).unwrap_or(fallback);
        self.show(index, width, height, WINDOW_BYTES);
        let app = app.to_owned();
        self.pump(session, |session, _| {
            window_of(session.compositor(), &app).is_some()
        });
        index
    }

    /// [`Self::map`] of a window that is set up by nothing.
    #[track_caller]
    pub fn map_plain(&mut self, session: &mut Session, app: &str, fallback: (i32, i32)) -> usize {
        self.map(session, app, fallback, |_| {})
    }

    /// Press and release `keycode` on the session's keyboard and let the client
    /// hear what became of it.
    pub fn tap(&mut self, session: &mut Session, keycode: u32) {
        for pressed in [true, false] {
            session
                .compositor()
                .input(InputEvent::Key { keycode, pressed })
                .expect("the key");
            self.exchange();
            session.step(Duration::from_millis(1)).expect("step");
            self.exchange();
        }
    }

    /// Wait for a configure to a size other than `was`, then draw at it: the
    /// commit that makes the state the configure carried the window's own.
    #[track_caller]
    pub fn redraw_when_resized(
        &mut self,
        session: &mut Session,
        index: usize,
        was: (i32, i32),
    ) -> (i32, i32) {
        self.pump(session, |_, seen| {
            seen.size_at(index).is_some_and(|size| size != was)
        });
        let size = self.seen.size_at(index).expect("configured");
        self.show(index, size.0, size.1, WINDOW_BYTES);
        size
    }
}

/// Step the session and `clients` together until `done` holds: what the
/// session waits for is often a worker thread (a folder read, a bus call), so
/// counting frames would not do.
#[track_caller]
pub fn pump(
    session: &mut Session,
    clients: &mut [&mut Client],
    mut done: impl FnMut(&mut Session, &[&mut Client]) -> bool,
) {
    super::wait_for(session, "what the test asked of the session", |session| {
        for client in clients.iter_mut() {
            client.exchange();
        }
        let answer = done(session, clients);
        for client in clients.iter_mut() {
            client.exchange();
        }
        answer.then_some(())
    });
}

/// The window of `app` and its rectangle `(x, y, w, h)`, once the desk places
/// it.
pub fn window_of(compositor: &Compositor, app: &str) -> Option<(u64, (i32, i32, i32, i32))> {
    compositor
        .windows()
        .into_iter()
        .find(|(info, _)| info.app_id.as_deref() == Some(app))
        .map(|(info, rect)| (info.id, rect))
}

/// Press `keycode` (`pressed`) on the session's keyboard, as the person does.
pub fn press_key(session: &mut Session, keycode: u32, pressed: bool) {
    session
        .compositor()
        .input(InputEvent::Key { keycode, pressed })
        .expect("the key");
    session.step(Duration::from_millis(1)).expect("step");
}

/// Press and release `keycode` on the session's keyboard, as the person does.
pub fn tap_key(session: &mut Session, keycode: u32) {
    press_key(session, keycode, true);
    press_key(session, keycode, false);
}

/// Press and release the left button at `(x, y)` of the session's display, as
/// the person does.
pub fn click(session: &mut Session, x: f64, y: f64) {
    session
        .compositor()
        .input(InputEvent::PointerMotion { x, y })
        .expect("the pointer");
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: BTN_LEFT,
                pressed,
            })
            .expect("the button");
        session.step(Duration::from_millis(1)).expect("step");
    }
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
                .is_some_and(|name| {
                    name.starts_with("wayland-")
                        && std::path::Path::new(name)
                            .extension()
                            .is_none_or(|extension| extension != "lock")
                })
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
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
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
                "wl_seat" => seen.seat = Some(registry.bind(name, version.min(7), qh, ())),
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

impl Dispatch<XdgSurface, usize> for Seen {
    fn event(
        seen: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            if let Some(toplevel) = seen.toplevels.get_mut(*index) {
                toplevel.configured = true;
            }
        }
    }
}

impl Dispatch<XdgToplevel, usize> for Seen {
    fn event(
        seen: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event
            && let Some(toplevel) = seen.toplevels.get_mut(*index)
        {
            toplevel.size = Some((width, height));
        }
    }
}

impl Dispatch<WlSeat, ()> for Seen {
    fn event(
        seen: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Keyboard) && seen.keyboard.is_none() {
                seen.keyboard = Some(seat.get_keyboard(qh, ()));
            }
            if capabilities.contains(wl_seat::Capability::Pointer) && seen.pointer.is_none() {
                seen.pointer = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { .. } => seen.keyboard_focused = true,
            wl_keyboard::Event::Leave { .. } => seen.keyboard_focused = false,
            wl_keyboard::Event::Key { key, state, .. } => {
                let pressed = state == WEnum::Value(wl_keyboard::KeyState::Pressed);
                seen.keys.push((key, pressed));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { .. } => {
                seen.pointer_on_window = seen.pointer_on_window.saturating_add(1);
            }
            wl_pointer::Event::Button {
                button,
                state: WEnum::Value(wl_pointer::ButtonState::Pressed),
                ..
            } => seen.buttons.push(button),
            _ => {}
        }
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
