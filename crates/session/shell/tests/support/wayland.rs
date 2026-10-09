//! A Wayland test client wired straight to an in-process compositor: no
//! socket path, no session, just a socket pair and a loop that pumps both
//! ends until the exchange settles.

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use shell::compositor::state::{ClientState, State};
use shell::shell::state::TileContent;
use smithay::reexports::calloop::{EventLoop, LoopHandle};
use smithay::reexports::wayland_server::{Display, DisplayHandle};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy};

/// Emits the handlers every test client repeats: the registry records each
/// advertised global into the client's `globals: Vec<(u32, String, u32)>`,
/// `xdg_wm_base` pings are answered (an unanswered one is a protocol error
/// that closes the connection mid-test) and every `xdg_surface` configure is
/// acked. Name the parts to take only some: `test_client!(Peer: registry, ping)`.
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
        let event_loop = EventLoop::try_new().expect("calloop");
        let display: Display<State> = Display::new().expect("display");
        let state = make(event_loop.handle(), display.handle());
        let conn = connect(&display);
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
