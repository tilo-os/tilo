//! Tilo's Wayland compositor.
//!
//! It shows buffers and routes input; it draws no interface of its own.
//! Client surfaces and the layers of hosted surface apps ([`SurfaceHost`])
//! are composited per output ([`render`]); where windows go and who has focus
//! is a [`WindowPolicy`]'s decision; a [`Backend`] supplies outputs and
//! input ([`backend::headless`]: CPU composition with no display, for tests
//! and screenshots; [`backend::nested`]: a window inside another desktop;
//! [`backend::drm`]: a bare TTY).
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use tilo_compositor::{Compositor, OutputConfig, StackPolicy};
//! let mut compositor = Compositor::headless(Box::new(StackPolicy))?;
//! compositor.add_output(&OutputConfig::new("HEADLESS-1", 1280, 720))?;
//! let socket = compositor.connect_client()?; // hand to a client
//! compositor.dispatch(std::time::Duration::from_millis(16))?;
//! compositor.render()?;
//! let frame = compositor.screenshot("HEADLESS-1")?;
//! # drop((socket, frame)); Ok(()) }
//! ```

#![cfg(target_os = "linux")]

pub mod backend;
mod cursor;
pub mod dim;
pub mod gestures;
pub mod host;
pub mod idle;
pub mod input;
pub mod keys;
mod lock;
pub mod output;
mod output_management;
mod output_power;
pub mod policy;
mod power;
mod present;
mod protocols;
pub mod render;
mod state;
mod tablet;
mod text_input;
mod touch;
mod xwayland;

use std::os::unix::net::UnixStream;
use std::time::Duration;

use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{EventLoop, Interest, LoopHandle, Mode, PostAction};
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;

pub use backend::{
    Backend, BackendError, DisplayConfig, DrmBackend, HeadlessBackend, InputConfig, NestedBackend,
    NestedConfig, Screenshot, TabletConfig,
};
pub use dim::Backlight;
pub use host::{HostLayer, SurfaceHost};
pub use idle::{IdleConfig, SessionTransition};
pub use input::{
    GestureEvent, HostKey, InputEvent, KeyModifiers, Switch, TabletEvent, TabletPhase,
};
pub use keys::{KeyClaim, KeyRepeat};
pub use output::{ModeRequest, OutputChange, OutputConfig};
pub use policy::{Arrangement, StackPolicy, WindowInfo, WindowPolicy};
pub use state::State;

/// Why the compositor could not start or keep running.
#[derive(Debug)]
pub enum Error {
    /// The event loop or a client socket failed.
    Io(std::io::Error),
    /// The backend failed.
    Backend(BackendError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "compositor I/O: {error}"),
            Self::Backend(error) => write!(f, "compositor backend: {error}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<BackendError> for Error {
    fn from(error: BackendError) -> Self {
        Self::Backend(error)
    }
}

fn io(error: impl std::fmt::Display) -> Error {
    Error::Io(std::io::Error::other(error.to_string()))
}

/// What the event loop's sources reach: the state and the backend.
pub struct LoopData {
    /// The Wayland state, windows and seat.
    pub state: State,
    /// The backend that created the sources.
    pub backend: Box<dyn Backend>,
}

/// A running compositor: the Wayland display, its state and a backend.
pub struct Compositor {
    event_loop: EventLoop<'static, LoopData>,
    display: Display<State>,
    data: LoopData,
}

impl Compositor {
    /// A compositor on the backend `make_backend` creates, which may
    /// register its sources in the loop it is handed.
    pub fn new(
        policy: Box<dyn WindowPolicy>,
        make_backend: impl FnOnce(
            &LoopHandle<'static, LoopData>,
            &mut State,
        ) -> Result<Box<dyn Backend>, Error>,
    ) -> Result<Self, Error> {
        let event_loop = EventLoop::try_new().map_err(io)?;
        let mut display = Display::<State>::new().map_err(io)?;
        let mut state = State::new(&display.handle(), policy);
        // Client requests are read after the loop wakes, in `dispatch`; the
        // loop only needs to wake when the display's socket is readable.
        let poll_fd = display.backend().poll_fd().try_clone_to_owned()?;
        event_loop
            .handle()
            .insert_source(
                Generic::new(poll_fd, Interest::READ, Mode::Level),
                |_, _, _| Ok(PostAction::Continue),
            )
            .map_err(io)?;
        let backend = make_backend(&event_loop.handle(), &mut state)?;
        Ok(Self {
            event_loop,
            display,
            data: LoopData { state, backend },
        })
    }

    /// A compositor whose outputs have no display (see [`backend::headless`]).
    pub fn headless(policy: Box<dyn WindowPolicy>) -> Result<Self, Error> {
        Self::new(policy, |_, state| {
            Ok(Box::new(HeadlessBackend::new(state)?))
        })
    }

    /// A compositor on a bare TTY (see [`backend::drm`]).
    pub fn drm(
        policy: Box<dyn WindowPolicy>,
        config: backend::DisplayConfig,
    ) -> Result<Self, Error> {
        Self::new(policy, |handle, state| {
            Ok(Box::new(backend::DrmBackend::start(handle, state, config)?))
        })
    }

    /// A compositor in a window of the desktop it runs inside (see
    /// [`backend::nested`]).
    pub fn nested(policy: Box<dyn WindowPolicy>, config: &NestedConfig) -> Result<Self, Error> {
        Self::new(policy, |handle, state| {
            Ok(Box::new(NestedBackend::start(handle, state, config)?))
        })
    }

    /// Run until `stop` holds or the backend is done: wait for clients, devices and due frames,
    /// handle them and draw.
    pub fn run(&mut self, mut stop: impl FnMut(&Self) -> bool) -> Result<(), Error> {
        while !self.closed() && !stop(self) {
            let timeout = self.next_wait();
            self.dispatch(timeout)?;
            self.render()?;
        }
        Ok(())
    }

    /// Whether the backend is done (its window closed, its seat gone).
    pub fn closed(&self) -> bool {
        self.data.backend.closed()
    }

    /// How long a loop may wait for clients and devices before the next
    /// frame is due.
    pub fn next_wait(&self) -> Duration {
        /// The longest the loop sleeps with nothing due, so a caller's own
        /// checks still run.
        const IDLE: Duration = Duration::from_millis(250);
        self.data.backend.next_deadline().map_or(IDLE, |at| {
            at.saturating_duration_since(std::time::Instant::now())
                .min(IDLE)
        })
    }

    /// Add an output the backend simulates.
    pub fn add_output(&mut self, config: &OutputConfig) -> Result<(), Error> {
        self.data.backend.add_output(&mut self.data.state, config)?;
        Ok(())
    }

    /// Host a surface app above the windows (and above earlier hosts), or
    /// under them when it says so ([`SurfaceHost::below_windows`]).
    pub fn add_host(&mut self, mut host: Box<dyn SurfaceHost>) {
        host.outputs_changed(&self.data.state.hosted_outputs());
        self.data.state.hosts.push(host);
    }

    /// Connect a client over a socket pair; the returned end is the
    /// client's (`wl_display` connection).
    pub fn connect_client(&mut self) -> Result<UnixStream, Error> {
        let (ours, theirs) = UnixStream::pair()?;
        self.display
            .handle()
            .insert_client(ours, state::client_data(false))
            .map_err(io)?;
        Ok(theirs)
    }

    /// Connect a client the session trusts with privileged protocols
    /// (locking the session); otherwise as [`Compositor::connect_client`].
    pub fn connect_trusted_client(&mut self) -> Result<UnixStream, Error> {
        let (ours, theirs) = UnixStream::pair()?;
        self.display
            .handle()
            .insert_client(ours, state::client_data(true))
            .map_err(io)?;
        Ok(theirs)
    }

    /// Start Xwayland for X11 clients and return its display number (they
    /// connect with `DISPLAY=:N`, at once).
    pub fn start_xwayland(&mut self) -> Result<u32, Error> {
        xwayland::start(&self.event_loop.handle(), &self.data.state)
    }

    /// Whether the session is locked.
    pub fn locked(&self) -> bool {
        self.data.state.locked()
    }

    /// Listen for clients on a fresh `wayland-N` socket in
    /// `$XDG_RUNTIME_DIR` and return its name (for `WAYLAND_DISPLAY`).
    pub fn listen(&mut self) -> Result<String, Error> {
        let source = ListeningSocketSource::new_auto().map_err(io)?;
        let name = source.socket_name().to_string_lossy().into_owned();
        self.event_loop
            .handle()
            .insert_source(source, |stream, (), data| {
                if let Err(error) = data
                    .state
                    .display
                    .insert_client(stream, state::client_data(false))
                {
                    tracing::warn!(%error, "client not accepted");
                }
            })
            .map_err(io)?;
        Ok(name)
    }

    /// Wait up to `timeout` for client requests or backend events, handle
    /// them and flush the answers.
    pub fn dispatch(&mut self, timeout: Duration) -> Result<(), Error> {
        // A held key's next repeat for a hosted surface wakes the loop.
        let timeout = self.data.state.host_keys.next_due().map_or(timeout, |due| {
            timeout.min(due.saturating_duration_since(std::time::Instant::now()))
        });
        self.event_loop
            .dispatch(Some(timeout), &mut self.data)
            .map_err(io)?;
        self.data.state.repeat_keys();
        self.display.dispatch_clients(&mut self.data.state)?;
        self.data.state.refresh();
        self.configure_from_clients();
        self.data.state.tick_idle();
        self.sync_power();
        self.display.flush_clients()?;
        Ok(())
    }

    /// Magnify the whole scene `zoom` times about the pointer, which stays
    /// on what it points at; 1 (or less) turns the magnifier off.
    pub fn set_zoom(&mut self, zoom: f64) {
        self.data.state.zoom = if zoom.is_finite() { zoom.max(1.0) } else { 1.0 };
    }

    /// The magnifier's zoom; 1 is off.
    pub fn zoom(&self) -> f64 {
        self.data.state.zoom
    }

    /// How held keys repeat, for clients (the seat's repeat info) and
    /// hosted surfaces alike.
    pub fn set_key_repeat(&mut self, repeat: KeyRepeat) {
        self.data.state.host_keys.repeat = repeat;
        if let Some(keyboard) = self.data.state.seat.get_keyboard() {
            let delay = i32::try_from(repeat.delay.as_millis()).unwrap_or(i32::MAX);
            keyboard.change_repeat_info(i32::try_from(repeat.rate).unwrap_or(i32::MAX), delay);
        }
    }

    /// Switch the panels whose wanted power changed.
    fn sync_power(&mut self) {
        let LoopData { state, backend } = &mut self.data;
        backend.sync_power(state);
    }

    /// Apply idle transitions a session request caused, at once.
    fn apply_idle(&mut self, transitions: Vec<SessionTransition>) -> Result<(), Error> {
        self.data.state.apply_idle(transitions);
        self.sync_power();
        self.display.flush_clients()?;
        Ok(())
    }

    /// When the displays dim, blank, and the session locks or suspends
    /// with no input. Applied to the current idle episode: a shorter
    /// timeout can fire at the next dispatch.
    pub fn set_idle_config(&mut self, config: IdleConfig) {
        let session = &mut self.data.state.idle.session;
        session.set_blank_timeout(config.blank_timeout);
        session.set_lock_timeout(config.lock_timeout);
        session.set_suspend_timeout(config.suspend_timeout);
    }

    /// The panel's backlight, which the idle dim lowers; without one the
    /// scene is darkened instead.
    pub fn set_backlight(&mut self, backlight: Box<dyn Backlight>) {
        self.data.state.idle.dimmer = dim::ScreenDimmer::new(backlight);
    }

    /// What the session has to act on since the last call:
    /// [`SessionTransition::Locked`] (start the lock client),
    /// [`SessionTransition::Unlocked`] and [`SessionTransition::Suspend`]
    /// (ask logind, unless something vetoes it).
    pub fn take_session_events(&mut self) -> Vec<SessionTransition> {
        std::mem::take(&mut self.data.state.idle.events)
    }

    /// Something outside the compositor (a download, a remote session, a
    /// keep-awake lease) holds the machine awake: it does not suspend when
    /// idle, though the screen still dims, blanks and locks.
    pub fn hold_awake(&mut self, held: bool) {
        self.data.state.idle.suspend_held = held;
    }

    /// Lock now (a key or a command): reported as
    /// [`SessionTransition::Locked`] for the session to start its lock
    /// client.
    pub fn lock_now(&mut self) -> Result<(), Error> {
        let transitions = self
            .data
            .state
            .idle
            .session
            .request_lock(std::time::Instant::now());
        self.apply_idle(transitions)
    }

    /// Switch every panel off now, as the idle timeout would (the power
    /// key); input lights them again.
    pub fn blank_now(&mut self) -> Result<(), Error> {
        let transitions = self
            .data
            .state
            .idle
            .session
            .blank_now(std::time::Instant::now());
        self.apply_idle(transitions)
    }

    /// Light the panels without counting as input (a notification that
    /// wakes the screen): the lock and idle timers stay as they are.
    pub fn wake_displays(&mut self) -> Result<(), Error> {
        let transitions = self
            .data
            .state
            .idle
            .session
            .request_display_wake(std::time::Instant::now());
        self.apply_idle(transitions)
    }

    /// The machine is about to sleep: lock first when `lock_on_sleep`.
    pub fn prepare_for_sleep(&mut self, lock_on_sleep: bool) -> Result<(), Error> {
        let transitions = self
            .data
            .state
            .idle
            .session
            .prepare_for_sleep(std::time::Instant::now(), lock_on_sleep);
        self.apply_idle(transitions)
    }

    /// The machine woke: light the panels, keep the lock.
    pub fn resume_from_sleep(&mut self) -> Result<(), Error> {
        let transitions = self
            .data
            .state
            .idle
            .session
            .resume_from_sleep(std::time::Instant::now());
        self.apply_idle(transitions)
    }

    /// Whether the panel of the output named `name` is lit (not blanked,
    /// switched off by a tool or turned off).
    pub fn output_lit(&self, name: &str) -> bool {
        self.data.state.output_power.lit(name)
    }

    /// Apply (or test) the display configurations clients sent and tell
    /// them how it went, then show every manager the outputs as they are.
    fn configure_from_clients(&mut self) {
        let state = &mut self.data.state;
        for pending in state.output_management.take_pending() {
            match self
                .data
                .backend
                .configure_checked(state, &pending.changes, pending.test)
            {
                Ok(()) => pending.configuration.succeeded(),
                Err(error) => {
                    tracing::info!(%error, "display configuration refused");
                    pending.configuration.failed();
                }
            }
        }
        let display = state.display.clone();
        let outputs: Vec<(smithay::output::Output, bool)> = state
            .space
            .outputs()
            .map(|output| (output.clone(), true))
            .chain(
                state
                    .disabled_outputs
                    .iter()
                    .map(|output| (output.clone(), false)),
            )
            .collect();
        state.output_management.publish(&display, outputs.iter());
    }

    /// Change outputs' modes, scales, rotations and places (as a display
    /// tool would through `wlr-output-management`). All or nothing.
    pub fn configure_outputs(&mut self, changes: &[OutputChange]) -> Result<(), Error> {
        self.data
            .backend
            .configure_checked(&mut self.data.state, changes, false)?;
        self.configure_from_clients();
        self.display.flush_clients()?;
        Ok(())
    }

    /// Apply the person's input preferences (pointer, touchpad, tablets,
    /// accessibility filters) to the backend's devices, now and as they are
    /// plugged in. Backends without devices of their own ignore them.
    pub fn set_input_config(&mut self, config: InputConfig) {
        let (delay, rate) = config.key_repeat_params();
        self.set_key_repeat(KeyRepeat { delay, rate });
        let LoopData { state, backend } = &mut self.data;
        backend.set_input_config(state, config);
    }

    /// Route one input event (see [`InputEvent`]) and flush what it sent.
    pub fn input(&mut self, event: InputEvent) -> Result<(), Error> {
        self.data.state.handle_input(event);
        // Input wakes blanked panels at once.
        self.sync_power();
        self.display.flush_clients()?;
        Ok(())
    }

    /// Draw a frame on every output that can take one and tell clients it
    /// was shown.
    pub fn render(&mut self) -> Result<(), Error> {
        for host in &mut self.data.state.hosts {
            host.prepare_frame();
        }
        // A frame can move windows (a strip panning, a group resizing): the
        // policy places them where the surfaces just laid them out.
        self.data.state.rearrange();
        self.data.backend.render(&mut self.data.state)?;
        self.data.state.confirm_lock();
        self.display.flush_clients()?;
        Ok(())
    }

    /// The last frame drawn on the output named `name`.
    pub fn screenshot(&mut self, name: &str) -> Result<Screenshot, Error> {
        Ok(self.data.backend.screenshot(name)?)
    }

    /// The mapped windows and their logical rectangles `(x, y, w, h)`,
    /// bottom first.
    pub fn windows(&self) -> Vec<(WindowInfo, (i32, i32, i32, i32))> {
        self.data
            .state
            .windows()
            .into_iter()
            .map(|(info, rect)| (info, (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h)))
            .collect()
    }

    /// Whether a client keeps the session from idling (idle-inhibit).
    pub fn idle_inhibited(&self) -> bool {
        self.data.state.idle_inhibited()
    }

    /// The window with keyboard focus.
    pub fn focused_window(&self) -> Option<WindowInfo> {
        self.data.state.focused_window()
    }
}

impl std::os::fd::AsFd for Compositor {
    /// The loop's descriptor, readable when [`Compositor::dispatch`] has
    /// work, for embedding in another loop.
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.event_loop.as_fd()
    }
}
