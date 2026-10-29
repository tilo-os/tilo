//! A Wayland test client wired straight to an in-process compositor: no
//! socket path, no session, just a socket pair and a loop that pumps both
//! ends until the exchange settles.

use std::fs::File;
use std::ops::{Deref, DerefMut};
use std::os::fd::{AsFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::fs::FileExt as _;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use automerge::{AutoCommit, ROOT};
use shell::compositor::state::{ClientState, State};
use shell::shell::frame::FrameBuilder;
use shell::shell::state::TileContent;
use smithay::reexports::calloop::{EventLoop, LoopHandle};
use smithay::reexports::wayland_server::{Display, DisplayHandle};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use winit::event::{Modifiers, WindowEvent};

/// Emits the handlers every test client repeats: the registry records each
/// advertised global into the client's `globals: Vec<(u32, String, u32)>`,
/// `xdg_wm_base` pings are answered (an unanswered one is a protocol error
/// that closes the connection mid-test) and every `xdg_surface` configure is
/// acked. Name the parts to take only some: `test_client!(Peer: registry, ping)`.
/// `surface`, `toplevel`, `shm`, `buffer` and `seat` ignore the events of
/// `wl_compositor` and `wl_surface`, `xdg_toplevel`, `wl_shm` and its pools,
/// `wl_buffer` and `wl_seat`.
macro_rules! test_client {
    ($client:ty) => {
        $crate::support::wayland::test_client!($client: registry, ping, configure);
    };
    ($client:ty: $($part:ident),+) => {
        $($crate::support::wayland::test_client!(@$part $client);)+
    };
    (@registry $client:ty) => {
        impl ::wayland_client::Dispatch<::wayland_client::protocol::wl_registry::WlRegistry, ()>
            for $client
        {
            fn event(
                state: &mut Self,
                _: &::wayland_client::protocol::wl_registry::WlRegistry,
                event: ::wayland_client::protocol::wl_registry::Event,
                (): &(),
                _: &::wayland_client::Connection,
                _: &::wayland_client::QueueHandle<Self>,
            ) {
                if let ::wayland_client::protocol::wl_registry::Event::Global {
                    name,
                    interface,
                    version,
                } = event
                {
                    state.globals.push((name, interface, version));
                }
            }
        }

        impl $crate::support::wayland::Globals for $client {
            fn globals(&self) -> &[(u32, String, u32)] {
                &self.globals
            }
        }
    };
    (@ping $client:ty) => {
        impl ::wayland_client::Dispatch<
            ::wayland_protocols::xdg::shell::client::xdg_wm_base::XdgWmBase,
            (),
        > for $client
        {
            fn event(
                _: &mut Self,
                base: &::wayland_protocols::xdg::shell::client::xdg_wm_base::XdgWmBase,
                event: ::wayland_protocols::xdg::shell::client::xdg_wm_base::Event,
                (): &(),
                _: &::wayland_client::Connection,
                _: &::wayland_client::QueueHandle<Self>,
            ) {
                if let ::wayland_protocols::xdg::shell::client::xdg_wm_base::Event::Ping {
                    serial,
                } = event
                {
                    base.pong(serial);
                }
            }
        }
    };
    (@configure $client:ty) => {
        impl ::wayland_client::Dispatch<
            ::wayland_protocols::xdg::shell::client::xdg_surface::XdgSurface,
            (),
        > for $client
        {
            fn event(
                _: &mut Self,
                surface: &::wayland_protocols::xdg::shell::client::xdg_surface::XdgSurface,
                event: ::wayland_protocols::xdg::shell::client::xdg_surface::Event,
                (): &(),
                _: &::wayland_client::Connection,
                _: &::wayland_client::QueueHandle<Self>,
            ) {
                if let ::wayland_protocols::xdg::shell::client::xdg_surface::Event::Configure {
                    serial,
                } = event
                {
                    surface.ack_configure(serial);
                }
            }
        }
    };
    // The objects most clients create without caring for their events.
    (@surface $client:ty) => {
        ::wayland_client::delegate_noop!($client: ignore wayland_client::protocol::wl_compositor::WlCompositor);
        ::wayland_client::delegate_noop!($client: ignore wayland_client::protocol::wl_surface::WlSurface);
    };
    (@toplevel $client:ty) => {
        ::wayland_client::delegate_noop!($client: ignore wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel);
    };
    (@shm $client:ty) => {
        ::wayland_client::delegate_noop!($client: ignore wayland_client::protocol::wl_shm::WlShm);
        ::wayland_client::delegate_noop!($client: ignore wayland_client::protocol::wl_shm_pool::WlShmPool);
    };
    (@buffer $client:ty) => {
        ::wayland_client::delegate_noop!($client: ignore wayland_client::protocol::wl_buffer::WlBuffer);
    };
    (@seat $client:ty) => {
        ::wayland_client::delegate_noop!($client: ignore wayland_client::protocol::wl_seat::WlSeat);
    };
    // Like `configure`, and records in the client's `configured` that the
    // surface has been configured.
    (@configured $client:ty) => {
        impl ::wayland_client::Dispatch<
            ::wayland_protocols::xdg::shell::client::xdg_surface::XdgSurface,
            (),
        > for $client
        {
            fn event(
                state: &mut Self,
                surface: &::wayland_protocols::xdg::shell::client::xdg_surface::XdgSurface,
                event: ::wayland_protocols::xdg::shell::client::xdg_surface::Event,
                (): &(),
                _: &::wayland_client::Connection,
                _: &::wayland_client::QueueHandle<Self>,
            ) {
                if let ::wayland_protocols::xdg::shell::client::xdg_surface::Event::Configure {
                    serial,
                } = event
                {
                    surface.ack_configure(serial);
                    state.configured = true;
                }
            }
        }
    };
}
pub(crate) use test_client;

/// A client that records the globals its registry heard.
pub trait Globals {
    fn globals(&self) -> &[(u32, String, u32)];

    /// The advertised name of `interface` and the version to bind it at:
    /// `version`, or less if the compositor offers less.
    fn global(&self, interface: &str, version: u32) -> Option<(u32, u32)> {
        self.globals()
            .iter()
            .find(|(_, advertised, _)| advertised == interface)
            .map(|&(name, _, advertised)| (name, version.min(advertised)))
    }
}

/// A compositor with one client connected over a socket pair; the client's
/// registry has already heard the globals.
pub struct Harness<C> {
    pub display: Display<State>,
    pub state: State,
    pub queue: EventQueue<C>,
    pub client: C,
    pub conn: Connection,
    pub registry: wl_registry::WlRegistry,
    // Dropping the loop invalidates the handle the compositor state holds.
    _event_loop: EventLoop<'static, State>,
}

impl<C> Harness<C>
where
    C: Globals + Default + Dispatch<wl_registry::WlRegistry, ()> + 'static,
{
    pub fn new() -> Self {
        Self::with_state(State::new)
    }

    /// As [`Harness::new`], with the compositor state built by `make`.
    pub fn with_state(
        make: impl FnOnce(LoopHandle<'static, State>, DisplayHandle) -> State,
    ) -> Self {
        Self::with_state_and_identity(make, |_| Arc::new(ClientState::default()))
    }

    pub fn with_identity(identity: impl FnOnce(&State) -> Arc<ClientState>) -> Self {
        Self::with_state_and_identity(State::new, identity)
    }

    fn with_state_and_identity(
        make: impl FnOnce(LoopHandle<'static, State>, DisplayHandle) -> State,
        identity: impl FnOnce(&State) -> Arc<ClientState>,
    ) -> Self {
        let event_loop = EventLoop::try_new().expect("calloop");
        let display: Display<State> = Display::new().expect("display");
        let state = make(event_loop.handle(), display.handle());
        let (server, client) = UnixStream::pair().expect("socket pair");
        display
            .handle()
            .insert_client(server, identity(&state))
            .expect("client insert");
        client.set_nonblocking(true).expect("nonblocking");
        let conn = Connection::from_socket(client).expect("client connection");
        let queue = conn.new_event_queue::<C>();
        let registry = conn.display().get_registry(&queue.handle(), ());
        let mut harness = Self {
            display,
            state,
            queue,
            client: C::default(),
            conn,
            registry,
            _event_loop: event_loop,
        };
        harness.pump();
        harness
    }

    /// Lets requests and events cross both ways a few times, enough for a
    /// request, its reply and whatever that reply prompts.
    pub fn pump(&mut self) {
        for _ in 0..6 {
            let _ = self.queue.flush();
            let _ = self.display.dispatch_clients(&mut self.state);
            let _ = self.display.flush_clients();
            if let Some(guard) = self.conn.prepare_read() {
                let _ = guard.read();
            }
            let _ = self.queue.dispatch_pending(&mut self.client);
        }
    }

    /// Binds `interface` at `version`, or lower if that is all on offer.
    pub fn bind<I: Proxy + 'static>(&self, interface: &str, version: u32) -> I
    where
        C: Dispatch<I, ()>,
    {
        self.try_bind(interface, version)
            .unwrap_or_else(|| panic!("{interface} is advertised"))
    }

    /// As [`Harness::bind`], or `None` if `interface` is not advertised.
    pub fn try_bind<I: Proxy + 'static>(&self, interface: &str, version: u32) -> Option<I>
    where
        C: Dispatch<I, ()>,
    {
        let (name, version) = self.client.global(interface, version)?;
        Some(self.registry.bind(name, version, &self.queue.handle(), ()))
    }

    /// The tab id of the adopted toplevel with `app_id`.
    pub fn foreign_id(&self, app_id: &str) -> u64 {
        foreign_id(&self.state, app_id)
    }

    /// An `ARGB8888` buffer of `pixels`, `width` by `height` and tightly
    /// packed, in a pool of its own on a freshly bound `wl_shm`.
    pub fn shm_buffer(&self, width: i32, height: i32, pixels: &[u8]) -> wl_buffer::WlBuffer
    where
        C: Dispatch<wl_shm::WlShm, ()>
            + Dispatch<wl_shm_pool::WlShmPool, ()>
            + Dispatch<wl_buffer::WlBuffer, ()>,
    {
        let shm = self.bind::<wl_shm::WlShm>("wl_shm", 1);
        shm_buffer(&shm, &self.queue.handle(), width, height, pixels)
    }
}

/// An anonymous shared-memory file holding `bytes`.
pub fn memfd(bytes: &[u8]) -> File {
    // SAFETY: a null-terminated name and a valid flag word are the whole
    // contract of `memfd_create`.
    let raw = unsafe { libc::memfd_create(c"tilo-test-shm".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(
        raw >= 0,
        "memfd_create: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: `memfd_create` just handed us this descriptor.
    let file = File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    file.set_len(u64::try_from(bytes.len()).expect("memfd size"))
        .expect("size the memfd");
    file.write_all_at(bytes, 0).expect("fill the memfd");
    file
}

/// A `wl_shm` pool over a memfd holding `bytes`, and that memfd.
pub fn shm_pool<C>(
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<C>,
    bytes: &[u8],
) -> (wl_shm_pool::WlShmPool, File)
where
    C: Dispatch<wl_shm_pool::WlShmPool, ()> + 'static,
{
    let file = memfd(bytes);
    let len = i32::try_from(bytes.len()).expect("pool size");
    (shm.create_pool(file.as_fd(), len, qh, ()), file)
}

/// An `ARGB8888` buffer of `pixels`, `width` by `height` and tightly packed,
/// in a pool of its own.
pub fn shm_buffer<C>(
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<C>,
    width: i32,
    height: i32,
    pixels: &[u8],
) -> wl_buffer::WlBuffer
where
    C: Dispatch<wl_shm_pool::WlShmPool, ()> + Dispatch<wl_buffer::WlBuffer, ()> + 'static,
{
    let stride = width.checked_mul(4).expect("stride");
    assert_eq!(
        usize::try_from(stride.checked_mul(height).expect("buffer size")).ok(),
        Some(pixels.len()),
        "{width}x{height} ARGB8888 pixels"
    );
    let (pool, _file) = shm_pool(shm, qh, pixels);
    pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, qh, ())
}

/// `width` by `height` copies of `texel`, `[B, G, R, A]` as an `ARGB8888`
/// buffer holds it.
pub fn solid(width: i32, height: i32, texel: [u8; 4]) -> Vec<u8> {
    let count = usize::try_from(width.checked_mul(height).expect("texel count")).expect("size");
    texel.repeat(count)
}

/// A toplevel's objects, as [`create_toplevel`] makes them.
#[expect(
    clippy::struct_field_names,
    reason = "each field is named for the protocol object it holds"
)]
pub struct Toplevel {
    pub surface: wl_surface::WlSurface,
    pub xdg: xdg_surface::XdgSurface,
    pub toplevel: xdg_toplevel::XdgToplevel,
}

/// A new toplevel named `app_id`, not yet committed.
pub fn create_toplevel<C>(
    compositor: &wl_compositor::WlCompositor,
    wm: &xdg_wm_base::XdgWmBase,
    qh: &QueueHandle<C>,
    app_id: &str,
) -> Toplevel
where
    C: Dispatch<wl_surface::WlSurface, ()>
        + Dispatch<xdg_surface::XdgSurface, ()>
        + Dispatch<xdg_toplevel::XdgToplevel, ()>
        + 'static,
{
    let surface = compositor.create_surface(qh, ());
    let xdg = wm.get_xdg_surface(&surface, qh, ());
    let toplevel = xdg.get_toplevel(qh, ());
    toplevel.set_app_id(app_id.into());
    Toplevel {
        surface,
        xdg,
        toplevel,
    }
}

/// A [`Harness`] whose compositor state the production frame builder lays
/// out and paints, as the native loop does.
pub struct Framed<C> {
    pub wl: Harness<C>,
    pub fb: FrameBuilder,
    pub doc: AutoCommit,
    pub focus: Option<taffy::NodeId>,
}

impl<C> Deref for Framed<C> {
    type Target = Harness<C>;
    fn deref(&self) -> &Self::Target {
        &self.wl
    }
}

impl<C> DerefMut for Framed<C> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.wl
    }
}

impl<C> Framed<C>
where
    C: Globals + Default + Dispatch<wl_registry::WlRegistry, ()> + 'static,
{
    /// On a 1280x800 display.
    pub fn new() -> Self {
        Self::with_viewport((1280, 800))
    }

    pub fn with_viewport(viewport: (u32, u32)) -> Self {
        Self::from_harness(Harness::new(), viewport)
    }

    pub fn from_harness(wl: Harness<C>, viewport: (u32, u32)) -> Self {
        let fb = FrameBuilder::with_handle(wl.state.workspace_tree.clone(), false, viewport);
        Self {
            wl,
            fb,
            doc: AutoCommit::new(),
            focus: None,
        }
    }

    /// One frame the way the native loop runs it: clients, apps, layout,
    /// paint.
    pub fn frame(&mut self) {
        self.wl.pump();
        self.fb.pump_apps(&mut self.doc, &ROOT);
        let _ = self.fb.paint_settled();
    }

    /// Local input in production order: the seat first, the shell with what
    /// it leaves. Whether a client took it.
    pub fn route(&mut self, event: &WindowEvent, modifiers: Modifiers) -> bool {
        let to_client = shell::compositor::client_input::route_input(
            &mut self.wl.state,
            &mut self.fb,
            event,
            &modifiers,
        );
        if !to_client {
            self.fb.dispatch_event(
                event,
                &modifiers,
                &mut ui::widget::Edits::lend(&mut self.doc),
                &ROOT,
                &mut self.focus,
            );
        }
        to_client
    }
}

/// Connects a new client to `display`; its socket is non-blocking, so a read
/// with nothing pending returns at once.
pub fn connect(display: &Display<State>) -> Connection {
    let (server, client) = UnixStream::pair().expect("socket pair");
    display
        .handle()
        .insert_client(server, Arc::new(ClientState::default()))
        .expect("client insert");
    client.set_nonblocking(true).expect("non-blocking");
    Connection::from_socket(client).expect("client connection")
}

/// Every foreign (Wayland client) tab, as its app id and tab id.
pub fn foreign_tabs(state: &State) -> Vec<(String, u64)> {
    state
        .workspace_tree
        .read()
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .filter_map(|tab| match &tab.tile {
            TileContent::Foreign {
                app_id, surface_id, ..
            } => Some((app_id.clone(), *surface_id)),
            _ => None,
        })
        .collect()
}

/// The tab id of the adopted toplevel with `app_id`.
pub fn foreign_id(state: &State, app_id: &str) -> u64 {
    foreign_tabs(state)
        .into_iter()
        .find_map(|(actual, surface_id)| (actual == app_id).then_some(surface_id))
        .unwrap_or_else(|| panic!("{app_id} was adopted into a tab"))
}

/// One of the clients a [`Clients`] compositor has connected.
pub struct Connected<P> {
    /// The compositor's handle for this connection.
    pub server: smithay::reexports::wayland_server::Client,
    pub conn: Connection,
    pub queue: EventQueue<P>,
    pub peer: P,
}

impl<P: Globals + Dispatch<wl_registry::WlRegistry, ()> + 'static> Connected<P> {
    /// Binds `interface` at `version`, or lower if that is all on offer.
    pub fn bind<I: Proxy + 'static>(&self, interface: &str, version: u32) -> I
    where
        P: Dispatch<I, ()>,
    {
        self.try_bind(interface, version)
            .unwrap_or_else(|| panic!("{interface} is not advertised"))
    }

    /// As [`Connected::bind`], or `None` if `interface` is not advertised.
    pub fn try_bind<I: Proxy + 'static>(&self, interface: &str, version: u32) -> Option<I>
    where
        P: Dispatch<I, ()>,
    {
        let (name, version) = self.peer.global(interface, version)?;
        let registry = self.conn.display().get_registry(&self.queue.handle(), ());
        Some(registry.bind(name, version, &self.queue.handle(), ()))
    }

    /// Whether the compositor advertises `interface` to this client.
    pub fn sees(&self, interface: &str) -> bool {
        self.peer.global(interface, 1).is_some()
    }
}

/// A compositor any number of clients connect to, each with its own
/// credential.
#[expect(
    clippy::struct_field_names,
    reason = "the compositor's clients are what the harness is about"
)]
pub struct Clients<P> {
    pub display: Display<State>,
    pub state: State,
    pub clients: Vec<Connected<P>>,
    // Dropping the loop invalidates the handle the compositor state holds.
    _event_loop: EventLoop<'static, State>,
}

impl<P> Clients<P>
where
    P: Default + Dispatch<wl_registry::WlRegistry, ()> + 'static,
{
    pub fn new() -> Self {
        let event_loop = EventLoop::try_new().expect("calloop");
        let display = Display::new().expect("display");
        let state = State::new(event_loop.handle(), display.handle());
        Self {
            display,
            state,
            clients: Vec::new(),
            _event_loop: event_loop,
        }
    }

    /// Connects a client with `credential`, as the matching socket acceptor
    /// would, and reads its registry. Returns its index.
    pub fn connect(
        &mut self,
        credential: Option<shell::compositor::input_method::TrustedInputMethod>,
    ) -> usize {
        let (server, client) = UnixStream::pair().expect("socket pair");
        let data = credential.map_or_else(
            || Arc::new(ClientState::default()),
            ClientState::input_method_arc,
        );
        let inserted = self
            .display
            .handle()
            .insert_client(server, data)
            .expect("client insert");
        if let Some(kind) = credential {
            self.state
                .register_trusted_input_method_client(&inserted, kind);
        }
        client.set_nonblocking(true).expect("nonblocking");
        let conn = Connection::from_socket(client).expect("client connection");
        let queue = conn.new_event_queue::<P>();
        let _registry = conn.display().get_registry(&queue.handle(), ());
        self.clients.push(Connected {
            server: inserted,
            conn,
            queue,
            peer: P::default(),
        });
        self.pump();
        self.clients.len().saturating_sub(1)
    }

    /// Lets requests and events cross both ways between every client and
    /// the compositor a few times.
    pub fn pump(&mut self) {
        for _ in 0..8 {
            for client in &mut self.clients {
                let _ = client.queue.flush();
            }
            let _ = self.display.dispatch_clients(&mut self.state);
            let _ = self.display.flush_clients();
            for client in &mut self.clients {
                if let Some(guard) = client.conn.prepare_read() {
                    let _ = guard.read();
                }
                let _ = client.queue.dispatch_pending(&mut client.peer);
            }
        }
    }

    /// Pumps until `done` holds, failing with `what` at the hang bound:
    /// buffer and role requests can take more than one exchange to land.
    pub fn pump_until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        while !done(self) {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            self.pump();
            std::thread::yield_now();
        }
    }
}
