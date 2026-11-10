//! Tilo's Wayland compositor.
//!
//! It shows buffers and routes input; it draws no interface of its own.
//! Client surfaces and the layers of hosted surface apps ([`SurfaceHost`])
//! are composited per output ([`render`]); where windows go and who has focus
//! is a [`WindowPolicy`]'s decision; a [`Backend`] supplies outputs and
//! input ([`backend::headless`]: offscreen composition with no display, for
//! tests and screenshots; [`backend::nested`]: a window inside another
//! desktop; [`backend::drm`]: a bare TTY).
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

pub mod a11y;
pub mod app_liveness;
pub mod backdrop;
pub mod backend;
pub mod capture;
pub mod clipboard_history;
pub mod color_filter;
mod color_management;
mod compose;
pub mod config;
mod cursor;
pub mod dim;
mod display_change;
mod dnd;
pub mod effects;
pub mod gamma;
pub mod gestures;
mod gpu_health;
mod hdr;
pub mod heap_trim;
pub mod host;
pub mod idle;
pub mod input;
pub mod keys;
mod layer;
mod lock;
pub mod memory_report;
pub mod output;
mod output_management;
mod output_power;
mod pacing;
mod pointer_lock;
pub mod policy;
mod power;
mod present;
mod protocols;
pub mod remote_input;
pub mod render;
mod requests;
pub mod seats;
pub mod selection;
pub mod settings_bridge;
mod share_stream;
mod snapshot;
mod state;
pub mod sticky_keys;
pub mod switch_scan;
mod tablet;
mod text_input;
mod toplevel_icon;
mod touch;
mod typing;
mod window_frame;
mod x11;
mod x11_windows;
mod xdnd;
mod xdnd_bridge;
mod xwayland;

pub use seats::{Aim, Seat, WindowAim};

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{EventLoop, Interest, LoopHandle, Mode, PostAction};
use smithay::reexports::wayland_server::{Display, ListeningSocket};
use smithay::xwayland::xwm::{X11Window, XwmId};

use crate::backend::drm::access_keys::AccessKeys;
use crate::output_management::Pending;
use smithay::wayland::socket::ListeningSocketSource;

pub use a11y::{A11y, CursorA11y, Magnifier, MagnifierView};
pub use backend::drm::access_keys::Assists;
pub use backend::drm::devices::RequestedMode;
pub use backend::{
    Backend, BackendError, DisplayConfig, DrmBackend, HeadlessBackend, HostWindow, InputConfig,
    NestedBackend, NestedConfig, Screenshot, TabletConfig,
};
pub use color_filter::ColorFilter;
pub use config::{Hdr, KeyboardLayout, Settings};
pub use dim::Backlight;
pub use display_change::{CONFIRM_FOR, DisplayEvent};
pub use effects::WindowEffect;
pub use host::{HostDrag, HostFrame, HostIme, HostLayer, HostTextInput, SurfaceHost};
pub use idle::{IdleConfig, LockedBy, SessionTransition};
pub use input::{
    GestureEvent, InputEvent, ObservedInput, PadAction, Switch, TabletEvent, TabletPadEvent,
    TabletPhase,
};
pub use keys::KeyRepeat;

/// How close to the whole timeout a park must be to have ended on the deadline rather than on
/// arriving work.
const WAKE_MARGIN: Duration = Duration::from_millis(1);
pub use output::{ModeRequest, OutputChange, OutputConfig, OutputInfo};

/// What the person is in the middle of, as the compositor sees it: an arranging change moves
/// nothing under a gesture or while they type (shell.md §1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Underway {
    /// A pointer button is down: a drag or a resize is in flight.
    pub gesture: bool,
    /// The person typed within the typing pause.
    pub typing: bool,
}

impl Underway {
    /// Whether an arrangement must wait.
    #[must_use]
    pub const fn holds(self) -> bool {
        self.gesture || self.typing
    }
}
pub use policy::{
    Arrangement, LayoutPoint, LayoutRect, ResizeEdges, StackPolicy, TouchSample, WindowInfo,
    WindowPolicy, WindowRequest,
};
pub use selection::{Selection, TEXT_MIME};
pub use share_stream::ShareStream;
/// The quarter turns an output is shown at.
pub use smithay::utils::Transform;
pub use state::State;

pub use switch_scan::{Scan, SwitchConfig, SwitchPair};
pub use text_input::{TextCaret, TextEntry, TextField, TextFieldRefusal, TextInsertion};
use tilo_session_core::surface::LayerPose;
pub use toplevel_icon::{IconArtwork, IconImage, WindowIcon};
pub use window_frame::{Edge, Shadow, WindowFrame};
pub use x11::X11Displays;
pub use x11_windows::{Identity as X11Identity, X11Icon};

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

impl From<capture::CaptureError> for Error {
    fn from(error: capture::CaptureError) -> Self {
        match error {
            capture::CaptureError::Backend(error) => Self::Backend(error),
            other @ capture::CaptureError::Buffer(_) => {
                Self::Backend(BackendError(other.to_string()))
            }
        }
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

/// The instant a risky display change's wait for the person is measured from, and against: the
/// clock a test holds, so the wait ends on the person's answer rather than on how long the machine
/// took to ask.
fn display_clock_now() -> Instant {
    tilo_clock::now()
}

/// The base name of a surface app's own display in the runtime directory.
const SURFACE_DISPLAY: &str = "tilo-surface";

/// How far up the process tree a surface app's connections are looked for.
const TREE_DEPTH: u32 = 16;

/// A display bound for a surface app that has not started yet: the socket is there for the app to
/// find, and its clients wait until the app is known.
pub struct BoundDisplay {
    socket: Option<ListeningSocket>,
    path: PathBuf,
}

impl BoundDisplay {
    /// The path the app connects to: its `WAYLAND_DISPLAY` .
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The socket, which the loop takes from here on.
    fn take(&mut self) -> Option<ListeningSocket> {
        self.socket.take()
    }
}

/// A surface app's own display, listening for its clients: while it stands,
/// connections from the app the session started — and from what that app
/// runs — are trusted clients of this compositor, standing on the surface
/// app's output. Dropped, it stops listening and its socket goes.
pub struct SurfaceDisplay {
    handle: LoopHandle<'static, LoopData>,
    token: smithay::reexports::calloop::RegistrationToken,
}

impl Drop for SurfaceDisplay {
    fn drop(&mut self) {
        self.handle.remove(self.token);
    }
}

/// Whether `pid` is `ancestor` , or was started by it, up the process tree.
fn descends_from(pid: i32, ancestor: i32) -> bool {
    let mut current = pid;
    for _ in 0..TREE_DEPTH {
        if current == ancestor {
            return true;
        }
        if current < 2 {
            return false;
        }
        match parent_of(current) {
            Some(parent) if parent != current => current = parent,
            _ => return false,
        }
    }
    false
}

/// The process `pid` was started by, as the kernel records it.
fn parent_of(pid: i32) -> Option<i32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:")?.trim().parse().ok())
}

/// What the event loop's sources reach: the state and the backend.
pub struct LoopData {
    /// The Wayland state, windows and seat.
    pub state: State,
    /// The backend that created the sources.
    pub backend: Box<dyn Backend>,
}

/// What the loop tells of each turn of its wait.
/// Something watching the process for a main loop that has stopped uses it to tell an idle loop,
/// which waits for events however long, from one that is stuck between its waits.
pub trait LoopTurns: Send + Sync {
    /// The loop is about to wait for events.
    fn parking(&self);
    /// The wait returned; the turn's work begins.
    fn unparked(&self);
}

/// A running compositor: the Wayland display, its state and a backend.
pub struct Compositor {
    event_loop: EventLoop<'static, LoopData>,
    display: Display<State>,
    data: LoopData,
    /// It runs in a window of a desktop it did not start: that desktop owns idle, blanking, locking
    /// and suspend, so this one runs no idle timers.
    hosted: bool,
    /// The outputs as Settings and the session last read them ([`Compositor::display_snapshot`]).
    displays: settings_bridge::Published,
    /// Told of every turn of the loop's wait, when someone watches for a loop that has stopped.
    turns: Option<Arc<dyn LoopTurns>>,
    /// The person's accessibility settings as they apply now.
    a11y: A11y,
    /// The typing and pointing assists for input handed to [`Compositor::input`]; a backend's own
    /// devices filter theirs before they get there.
    keys: AccessKeys,
    /// The timings of the loop's turns, when `TILO_FRAME_TRACE` or a profile asked for them.
    recorder: tilo_frame_metrics::FrameRecorder,
    /// A turn is open: [`Compositor::begin_tick`] ran and [`Compositor::end_tick`] has not.
    ticking: bool,
}

impl Compositor {
    /// A compositor on the backend `make_backend` creates, which may register its sources in the
    /// loop it is handed.
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
        // Client requests are read after the loop wakes, in `dispatch` ; the loop only needs to
        // wake when the display's socket is readable.
        let poll_fd = display.backend().poll_fd().try_clone_to_owned()?;
        event_loop
            .handle()
            .insert_source(
                Generic::new(poll_fd, Interest::READ, Mode::Level),
                |_, _, _| Ok(PostAction::Continue),
            )
            .map_err(io)?;
        state.loop_handle = Some(event_loop.handle());
        state.x11 = Some(X11Displays::new(event_loop.handle(), display.handle()));
        let backend = make_backend(&event_loop.handle(), &mut state)?;
        let mut compositor = Self {
            event_loop,
            display,
            data: LoopData { state, backend },
            hosted: false,
            displays: settings_bridge::Published::default(),
            turns: None,
            a11y: A11y::default(),
            keys: AccessKeys::default(),
            recorder: tilo_frame_metrics::FrameRecorder::from_env(),
            ticking: false,
        };
        compositor.publish_displays();
        Ok(compositor)
    }

    /// Tell `turns` of every turn of the loop's wait, or no one.
    pub fn watch_turns(&mut self, turns: Option<Arc<dyn LoopTurns>>) {
        self.turns = turns;
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

    /// A compositor in a window of the desktop it runs inside (see [`backend::nested`]), the window
    /// [`backend::NestedBackend::open_window`] opened.
    /// The caller opens the window: a session opens it before it builds anything else, so the one
    /// connection its desktop was handed goes to the window and not to whatever the desk starts.
    pub fn nested(
        policy: Box<dyn WindowPolicy>,
        config: &NestedConfig,
        window: backend::HostWindow,
    ) -> Result<Self, Error> {
        let mut compositor = Self::new(policy, |handle, state| {
            Ok(Box::new(NestedBackend::start(
                handle, state, config, window,
            )?))
        })?;
        compositor.set_hosted(true);
        Ok(compositor)
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

    /// Start a recorded turn of the loop. The session calls it before [`Compositor::dispatch`] and
    /// ends it after the work the frame it drew answered.
    pub fn begin_tick(&mut self) {
        self.ticking = true;
        // A frame the display retired since the last turn closes the input measurement of the frame
        // that carried it.
        if let Some((serial, at)) = self.data.state.take_retired() {
            self.recorder.note_retirement(serial, at);
        }
        self.recorder.begin_tick(Instant::now());
    }

    /// The session's own share of the turn before the frame: the desk, the apps and the overlays it
    /// laid out.
    pub fn mark_pump(&mut self, took: Duration) {
        self.recorder.mark_pump(took);
    }

    /// Answering the session's control socket after the frame was drawn.
    pub fn mark_testctl(&mut self, took: Duration) {
        self.recorder.mark_testctl(took);
    }

    /// The workspace in front changed: its frames are timed until the desk rests.
    pub fn workspace_switched(&mut self) {
        self.data.state.workspace_switched();
    }

    /// Whether the workspace switch in flight still owes the desk's motion.
    pub fn set_switch_holding(&mut self, holding: bool) {
        self.data.state.set_switch_holding(holding);
    }

    /// How many frames the displays presented since this session began.
    pub fn frames_presented(&self) -> u64 {
        self.data.state.frames_presented()
    }

    /// The turn is over: its timings go to the frame trace.
    pub fn end_tick(&mut self) {
        if !self.ticking {
            return;
        }
        self.ticking = false;
        self.recorder.end_tick();
    }

    /// Whether a client has drawn something no frame has shown yet: the frame the desk owes answers
    /// it, and until it is drawn a client is waiting on this turn.
    pub fn owes_frame(&self) -> bool {
        !self.data.state.redraw.is_empty()
    }

    /// Take in what arrived while the caller worked out this frame — a
    /// commit with the frame a surface just laid out, a configure it
    /// acknowledged, a door message — and tell the clients what the caller
    /// queued for them, without waiting for any of it.
    ///
    /// Whether an output then owes a frame: what arrived is drawn now and
    /// answers its client in this turn rather than the next.
    ///
    /// # Errors
    /// The display is gone.
    pub fn catch_up(&mut self) -> Result<bool, Error> {
        self.display.flush_clients()?;
        self.event_loop
            .dispatch(Some(Duration::ZERO), &mut self.data)
            .map_err(io)?;
        self.display.flush_clients()?;
        Ok(self.owes_frame())
    }

    /// When a display builds the frame it owes: one lead before the blank that shows it. `None`
    /// when no output owes one, so the loop waits for an event.
    #[must_use]
    pub fn frame_deadline(&self) -> Option<Instant> {
        self.data.backend.next_deadline(&self.data.state)
    }

    /// Answer the clients until it is time to build the frame a display owes:
    /// what arrives while it waits joins that frame instead of each arrival
    /// taking a turn of its own.
    ///
    /// # Errors
    /// The compositor's clients failed.
    pub fn await_frame(&mut self) -> Result<(), Error> {
        self.await_frame_arrivals(|| {})
    }

    /// [`Self::await_frame`] that also runs `arrived` after each wait: what a host serves at once
    /// rather than after the frame — an app someone is waiting on starts while the frame the desk
    /// owes is still ahead, so its own start runs beside that frame.
    ///
    /// # Errors
    /// The compositor's clients failed.
    pub fn await_frame_arrivals(&mut self, mut arrived: impl FnMut()) -> Result<(), Error> {
        while let Some(due) = self.frame_deadline() {
            let wait = due.saturating_duration_since(std::time::Instant::now());
            if wait.is_zero() {
                break;
            }
            self.dispatch(wait)?;
            arrived();
        }
        Ok(())
    }

    /// One turn of a display's loop: wait until the frame it owes is due,
    /// then build it.
    ///
    /// # Errors
    /// The compositor's clients or its backend failed.
    pub fn draw_frame(&mut self) -> Result<(), Error> {
        self.await_frame()?;
        self.render()
    }

    /// How long a loop may wait for clients, devices and its own work before the next frame is due;
    /// `None` when nothing is due, and it waits for an event.
    pub fn next_wait(&self) -> Option<Duration> {
        let now = std::time::Instant::now();
        let backend = self.frame_deadline();
        // A running visual alert fades frame by frame.
        let alert = a11y::flashing(&self.data.state, now)
            .then(|| now.checked_add(a11y::FLASH_STEP).unwrap_or(now));
        backend
            .into_iter()
            .chain(
                self.data
                    .state
                    .hosts
                    .iter()
                    .filter_map(|host| host.next_deadline()),
            )
            .chain(self.keys.next_deadline())
            .chain(self.data.state.app_liveness_deadline())
            .chain(
                self.data
                    .state
                    .x11
                    .as_ref()
                    .and_then(X11Displays::next_check),
            )
            .chain(alert)
            .min()
            .map(|at| at.saturating_duration_since(now))
    }

    /// Add an output the backend simulates.
    pub fn add_output(&mut self, config: &OutputConfig) -> Result<(), Error> {
        self.data.backend.add_output(&mut self.data.state, config)?;
        self.publish_displays();
        Ok(())
    }

    /// Drop an output the backend simulates, as unplugging its cable does: it leaves the layout,
    /// clients stop seeing it and the desktop hears the outputs it has now and places its windows
    /// on those left.
    pub fn remove_output(&mut self, name: &str) -> Result<(), Error> {
        self.data
            .backend
            .remove_output(&mut self.data.state, name)?;
        self.sync_power();
        self.publish_displays();
        self.display.flush_clients()?;
        Ok(())
    }

    /// Host a surface app above the windows (and above earlier hosts), or under them when it says
    /// so ([`SurfaceHost::below_windows`]).
    pub fn add_host(&mut self, mut host: Box<dyn SurfaceHost>) {
        let outputs = self.data.state.hosted_outputs();
        host.outputs_changed(&outputs);
        self.data.state.hosts.push(host);
        self.data.state.mark_redraw_all();
    }

    /// Connect a client over a socket pair; the returned end is the client's (`wl_display`
    /// connection).
    pub fn connect_client(&mut self) -> Result<UnixStream, Error> {
        self.connect_client_as(false, None)
    }

    /// Connect a client the session trusts with privileged protocols (locking the session);
    /// otherwise as [`Compositor::connect_client`].
    pub fn connect_trusted_client(&mut self) -> Result<UnixStream, Error> {
        self.connect_client_as(true, None)
    }

    /// Connect a client the session trusts, whose layer surfaces stand on `output` whatever output
    /// they name themselves: the on-screen keyboard belongs on the display the person types on, and
    /// it asks for none.
    pub fn connect_trusted_client_on(&mut self, output: &str) -> Result<UnixStream, Error> {
        self.connect_client_as(true, Some(output.to_owned()))
    }

    /// Connect a client whose layer surfaces stand on `output` , when that is named, as
    /// [`Compositor::connect_client`] otherwise.
    fn connect_client_as(
        &self,
        trusted: bool,
        output: Option<String>,
    ) -> Result<UnixStream, Error> {
        let (ours, theirs) = UnixStream::pair()?;
        self.display
            .handle()
            .insert_client(
                ours,
                state::client_data(
                    trusted,
                    std::sync::Arc::clone(&self.data.state.exited),
                    output,
                ),
            )
            .map_err(io)?;
        Ok(theirs)
    }

    /// The session's X11 displays, by workspace: the app launcher asks one for the display it
    /// starts an app with. `None` only when the compositor was built without its loop.
    #[must_use]
    pub fn x11_displays(&self) -> Option<X11Displays> {
        self.data.state.x11.clone()
    }

    /// The GPU clients import their dma-bufs on, as the render node a process that allocates for
    /// them opens (`/dev/dri/renderD128`): `None` on a backend with no GPU to import from.
    pub fn render_node(&self) -> Option<PathBuf> {
        self.data
            .state
            .render_node
            .as_ref()
            .and_then(smithay::backend::drm::DrmNode::dev_path)
    }

    /// Whether the session is locked.
    pub fn locked(&self) -> bool {
        self.data.state.locked()
    }

    /// Whether the display is blanked: powered down, with only input waking it.
    pub fn blanked(&self) -> bool {
        self.data.state.idle.session.is_blanked()
    }

    /// Whether the session locked itself — idle, a key or a command — rather than a lock client
    /// holding it: its own lock screen is what the person sees.
    pub fn session_locked(&self) -> bool {
        self.data.state.idle.session.is_locked()
    }

    /// Whether the lock is what the outputs show: the lock client's own frame is on one of them, or
    /// the session draws the lock itself because the lock screen's app could not show it.
    pub fn lock_drawn(&self) -> bool {
        self.data.state.lock_drawn() || self.data.state.lock_view_shows()
    }

    /// Whether a surface of the session's own takes the keyboard now: a palette, the overview or
    /// the shortcut guide, which see keys before any client does.
    #[must_use]
    pub fn host_takes_keys(&self) -> bool {
        self.data
            .state
            .hosts
            .iter()
            .any(|host| host.wants_keyboard())
    }

    /// Whether a fullscreen window is up: it owns its display, and with it the controller in the
    /// person's hands.
    #[must_use]
    pub fn fullscreen_window(&self) -> bool {
        self.data.state.fullscreen()
    }

    /// Wake [`Compositor::dispatch`] when `fd` is readable, so what arrives on it (a controller's
    /// presses, a device's hotplug, a socket a client connects to or writes on) ends the wait
    /// rather than waiting for the next turn. Dropping the watch stops it.
    /// What the descriptor says is read by the caller's own source, not here: this only ends the
    /// wait.
    pub fn watch_readable(&mut self, fd: std::os::fd::OwnedFd) -> Result<ReadableWatch, Error> {
        let handle = self.event_loop.handle();
        let token = handle
            .insert_source(Generic::new(fd, Interest::READ, Mode::Level), |_, _, _| {
                Ok(PostAction::Continue)
            })
            .map_err(io)?;
        Ok(ReadableWatch {
            handle: handle.clone(),
            token,
        })
    }

    /// Bind a display of a surface app's own in `$XDG_RUNTIME_DIR`, before
    /// the app starts so it finds it: one connection to this compositor is
    /// its window's, and an app that speaks the compositor's input protocols
    /// — the on-screen keyboard does — reaches it on more.
    ///
    /// # Errors
    /// The runtime directory is not there, or every name is taken.
    pub fn bind_surface_display(&mut self) -> Result<BoundDisplay, Error> {
        let socket = ListeningSocket::bind_auto(SURFACE_DISPLAY, 1..64).map_err(io)?;
        let name = socket
            .socket_name()
            .ok_or_else(|| io("a surface display without a name"))?;
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .ok_or_else(|| io("no runtime directory for a surface app's display"))?;
        Ok(BoundDisplay {
            path: PathBuf::from(runtime).join(name),
            socket: Some(socket),
        })
    }

    /// Accept the clients of the surface app `owner` on `bound`, trusted as
    /// the app the session started and standing on `output` when it is
    /// named. Every connection must come from that process or one it starts:
    /// a socket another program of this login finds is of no use to it.
    ///
    /// # Errors
    /// The loop did not take the socket.
    pub fn accept_surface_clients(
        &self,
        mut bound: BoundDisplay,
        owner: u32,
        output: Option<String>,
    ) -> Result<SurfaceDisplay, Error> {
        let Some(socket) = bound.take() else {
            return Err(io("the surface display was already accepted"));
        };
        let owner = i32::try_from(owner).unwrap_or(-1);
        let handle = self.event_loop.handle();
        let token = handle
            .insert_source(
                Generic::new(socket, Interest::READ, Mode::Level),
                move |_, socket, data| {
                    while let Some(stream) = socket.accept()? {
                        let peer = rustix::net::sockopt::socket_peercred(&stream)
                            .map_or(-1, |credentials| credentials.pid.as_raw_pid());
                        if !descends_from(peer, owner) {
                            tracing::warn!(
                                peer,
                                owner,
                                "a surface app's display refused a connection that is not the app's"
                            );
                            continue;
                        }
                        let exited = std::sync::Arc::clone(&data.state.exited);
                        if let Err(error) = data
                            .state
                            .display
                            .insert_client(stream, state::client_data(true, exited, output.clone()))
                        {
                            tracing::warn!(%error, "a surface app's client was not accepted");
                        }
                    }
                    Ok(PostAction::Continue)
                },
            )
            .map_err(io)?;
        Ok(SurfaceDisplay { handle, token })
    }

    /// Listen for clients on a fresh `wayland-N` socket in `$XDG_RUNTIME_DIR` and return its name
    /// (for `WAYLAND_DISPLAY` ).
    pub fn listen(&mut self) -> Result<String, Error> {
        let source = ListeningSocketSource::new_auto().map_err(io)?;
        let name = source.socket_name().to_string_lossy().into_owned();
        self.event_loop
            .handle()
            .insert_source(source, |stream, (), data| {
                if let Err(error) = data.state.display.insert_client(
                    stream,
                    state::client_data(false, std::sync::Arc::clone(&data.state.exited), None),
                ) {
                    tracing::warn!(%error, "client not accepted");
                }
            })
            .map_err(io)?;
        Ok(name)
    }

    /// Wait up to `timeout` for client requests or backend events, handle them and flush the
    /// answers; with `None` , wait for an event however long that takes.
    pub fn dispatch(&mut self, timeout: impl Into<Option<Duration>>) -> Result<(), Error> {
        let timeout: Option<Duration> = timeout.into();
        let started = Instant::now();
        // A held key's next repeat for a hosted surface wakes the loop.
        let timeout = self.data.state.host_keys.next_due().map_or(timeout, |due| {
            let held = due.saturating_duration_since(std::time::Instant::now());
            Some(timeout.map_or(held, |timeout| timeout.min(held)))
        });
        if let Some(turns) = &self.turns {
            turns.parking();
        }
        let waiting = Instant::now();
        let dispatched = self
            .event_loop
            .dispatch(timeout, &mut self.data)
            .map_err(io);
        let parked = waiting.elapsed();
        if let Some(turns) = &self.turns {
            turns.unparked();
        }
        // A wait that ran its time out ended on the deadline; one cut short was ended by work
        // arriving.
        self.recorder.mark_sleep(parked);
        let expired = timeout.is_some_and(|timeout| parked.saturating_add(WAKE_MARGIN) >= timeout);
        self.recorder.mark_wake(if expired {
            "idle"
        } else {
            tilo_frame_metrics::WAKE_IO
        });
        dispatched?;
        self.recorder
            .mark_input(started.elapsed().saturating_sub(parked));
        self.data.state.repeat_keys();
        let ticked = Instant::now();
        let due = self.keys.tick(ticked);
        for event in due {
            self.data.state.note_typing(&event, ticked);
            self.data.state.handle_input(event);
        }
        if self.keys.take_alert() {
            self.flash();
        }
        // The visual alert fades frame by frame, so every frame it covers is a new one to draw.
        if a11y::flashing(&self.data.state, Instant::now()) {
            self.data.state.mark_redraw_all();
        }
        self.display.dispatch_clients(&mut self.data.state)?;
        self.sync_text_input();
        self.data.state.refresh();
        if let Some(x11) = self.data.state.x11.clone() {
            x11.tick(&mut self.data, Instant::now());
        }
        // A constraint or an inhibitor takes effect for what is focused and under the pointer now,
        // so it is settled once the clients' requests and this turn's focus changes were handled.
        self.data.state.sync_pointer_constraint();
        self.data.state.sync_shortcut_inhibit();
        self.sync_x11();
        self.configure_from_clients();
        // What answered in this turn is answered; what stayed silent past its time is not
        // responding.
        self.data.state.advance_app_liveness_at(Instant::now());
        if let Some(undo) = self.data.state.display_changes.due(display_clock_now()) {
            self.put_back(&undo);
        }
        self.data.state.tick_idle();
        self.sync_power();
        self.publish_displays();
        self.publish_output_colors();
        self.display.flush_clients()?;
        self.data.state.lift_lock_of_departed_client();
        Ok(())
    }

    /// Magnify the whole scene `zoom` times about the pointer, which stays on what it points at; 1
    /// (or less) turns the magnifier off. The settings in force follow, so the step a shortcut
    /// takes and what they hold is one zoom, not two.
    pub fn set_zoom(&mut self, zoom: f64) {
        let zoom = if zoom.is_finite() { zoom.max(1.0) } else { 1.0 };
        self.data.state.zoom = zoom;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the zoom steps are whole numbers of a small range"
        )]
        {
            self.a11y.magnifier.zoom = zoom.min(f64::from(u8::MAX)) as u8;
        }
        self.data.state.mark_redraw_all();
    }

    /// The magnifier's zoom; 1 is off.
    pub fn zoom(&self) -> f64 {
        self.data.state.zoom
    }

    /// Apply the person's accessibility settings: typing and pointing
    /// assists, the pointer's theme, size and crosshair, the colour filter,
    /// the magnifier and visual alerts. Everything applies live, and to
    /// outputs and devices that appear later.
    pub fn set_a11y(&mut self, a11y: A11y) -> Result<(), Error> {
        let now = Instant::now();
        let released = self.keys.configure(&a11y.assists, now);
        let LoopData { state, backend, .. } = &mut self.data;
        backend.set_assists(state, &a11y.assists);
        state.zoom = a11y.magnifier.factor();
        state.cursor.set_theme(&a11y.cursor.theme);
        state.cursor.set_size(a11y.cursor.size);
        for event in released {
            state.handle_input(event);
        }
        self.a11y = a11y;
        self.seed_visuals();
        // X clients draw their own pointers: they hear the theme at once.
        self.sync_x11();
        self.data.state.mark_redraw_all();
        self.display.flush_clients()?;
        Ok(())
    }

    /// Tell every running X server what the session holds for it: which X11 window has the keyboard
    /// (Xwayland leaves the X input focus to the window manager), how dense the display is and
    /// which pointer to draw.
    fn sync_x11(&mut self) {
        let scale = x11_windows::display_scale(&self.data.state);
        let theme = self.data.state.cursor.theme_name().to_owned();
        // The X11 window each display's keyboard focus is on, resolved before the servers are
        // borrowed.
        let focus = self
            .data
            .state
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus());
        let focused: Vec<(XwmId, X11Window)> = match &focus {
            Some(focus) => self
                .data
                .state
                .space
                .elements()
                .filter_map(|window| {
                    let surface = window.x11_surface()?;
                    surface
                        .wl_surface()
                        .is_some_and(|wl| &wl == focus)
                        .then(|| Some((surface.xwm_id()?, surface.window_id())))
                })
                .flatten()
                .collect(),
            None => Vec::new(),
        };
        for server in &mut self.data.state.xwms {
            let xwm = server.id();
            let target = focused
                .iter()
                .find(|(of, _)| *of == xwm)
                .map(|(_, window)| *window);
            let state::X11Server { wm, windows, .. } = server;
            let Some(windows) = windows.as_mut() else {
                continue;
            };
            windows.follow_keyboard(target);
            windows.publish_x_hints(scale, &theme, wm);
        }
    }

    /// Bring every output's visuals in line with the settings, outputs a backend found on its own
    /// since the last frame included.
    fn seed_visuals(&self) {
        for output in self.data.state.space.outputs() {
            a11y::seed(output, &self.a11y);
        }
    }

    /// The accessibility settings in force.
    #[must_use]
    pub fn a11y(&self) -> &A11y {
        &self.a11y
    }

    /// Whether a text field waits for typing, and whether the person is typing on the glass: what
    /// the on-screen keyboard follows.
    #[must_use]
    pub fn text_entry(&self) -> TextEntry {
        self.data.state.text_entry()
    }

    /// The text field the person is typing into, as dictation names it: its identity and the
    /// revision of what its client last committed, or why dictated text may not be written there.
    pub fn dictation_field(&self) -> Result<TextField, TextFieldRefusal> {
        self.data.state.dictation_field()
    }

    /// Insert committed text into `field` once, if the person is still typing into that same
    /// revision of it. The text goes through the trusted input method, never as keystrokes.
    pub fn insert_dictation(&mut self, field: TextField, text: &str) -> TextInsertion {
        self.data.state.insert_dictation(field, text)
    }

    /// Where the focused text field's caret stands in the desk, in logical coordinates, from the
    /// cursor rectangle its client reported; `None` when no field takes text or it never said where
    /// its caret is.
    #[must_use]
    pub fn text_caret(&self) -> Option<TextCaret> {
        self.data.state.text_caret()
    }

    /// Insert `text` into the field the person is typing into, whatever that field asks of its
    /// content: an emoji keeps none of its secret. The text goes through the trusted input method,
    /// never as keystrokes.
    /// Answers whether the field declared its content secret, which decides whether the text is
    /// remembered; `None` when no field takes text now.
    pub fn insert_active_text(&mut self, text: &str) -> Option<bool> {
        self.data.state.insert_active_text(text)
    }

    /// Type `ch` as the person's keyboard would on this seat's layout: the
    /// key that types it, with the modifiers it needs held. Answers whether
    /// a key of the layout types it; a character the layout has no key for
    /// is written as text instead ([`Self::insert_active_text`]).
    ///
    /// # Errors
    /// The input funnel refused one of the events.
    pub fn type_char(&mut self, ch: char) -> Result<bool, Error> {
        let Some(stroke) = self.data.state.stroke_for(ch) else {
            return Ok(false);
        };
        for modifier in &stroke.modifiers {
            self.input(InputEvent::Key {
                keycode: modifier.key,
                pressed: true,
            })?;
        }
        for pressed in [true, false] {
            self.input(InputEvent::Key {
                keycode: stroke.key,
                pressed,
            })?;
        }
        for modifier in stroke.modifiers.iter().rev() {
            self.input(InputEvent::Key {
                keycode: modifier.key,
                pressed: false,
            })?;
        }
        Ok(true)
    }

    /// Flash every output: a visual alert for what a person who cannot hear the alert sound would
    /// otherwise miss.
    pub fn flash(&mut self) {
        a11y::flash_all(&self.data.state, Instant::now());
        self.data.state.mark_redraw_all();
    }

    /// How held keys repeat, for clients (the seat's repeat info) and hosted surfaces alike.
    pub fn set_key_repeat(&mut self, repeat: KeyRepeat) {
        self.data.state.host_keys.repeat = repeat;
        if let Some(keyboard) = self.data.state.seat.get_keyboard() {
            let delay = i32::try_from(repeat.delay.as_millis()).unwrap_or(i32::MAX);
            keyboard.change_repeat_info(i32::try_from(repeat.rate).unwrap_or(i32::MAX), delay);
        }
    }

    /// Switch the panels whose wanted power changed and write the gamma ramps that changed.
    fn sync_power(&mut self) {
        let LoopData { state, backend, .. } = &mut self.data;
        backend.sync_power(state);
        backend.sync_gamma(state);
    }

    /// Turn night light on at a colour temperature in kelvin (clamped to
    /// [`gamma::NIGHT_LIGHT_KELVIN`]; 6500 changes nothing, lower is warmer) or off (`None`), on
    /// every output whose display engine carries a gamma ramp. A gamma tool holding an output's
    /// ramp keeps it until it lets go.
    pub fn set_night_light(&mut self, kelvin: Option<u32>) -> Result<(), Error> {
        self.data.state.gamma.set_night_light(kelvin);
        self.sync_power();
        self.display.flush_clients()?;
        Ok(())
    }

    /// Show `light` of their linear light on the displays without a backlight or DDC/CI, by
    /// connector (`1.0` or absent: not dimmed), on top of the tint they already have. A monitor
    /// whose brightness is its own hardware is not among them: its brightness is set there.
    pub fn set_software_dim(&mut self, dims: &[(String, f32)]) -> Result<(), Error> {
        self.data.state.gamma.set_software_dim(dims);
        self.sync_power();
        self.display.flush_clients()?;
        Ok(())
    }

    /// How much of its linear light each display is dimmed to in software,
    /// by connector, for the displays that are.
    pub fn software_dim(&self) -> Vec<(String, f32)> {
        self.data.state.gamma.software_dim()
    }

    /// Draw the window `id` with `effect` (moved, scaled about its centre,
    /// faded) from the next frame on, or as it stands again (`None`). The
    /// desktop animates windows by setting this every frame; input still
    /// lands where the window stands.
    pub fn set_window_effect(&mut self, id: u64, effect: Option<WindowEffect>) {
        match effect {
            Some(effect) => self.data.state.effects.insert(id, effect),
            None => self.data.state.effects.remove(&id),
        };
        self.data.state.mark_redraw_all();
    }

    /// How every window is framed (its corners, its shadow, its edge) from the next frame on. A
    /// window that fills the display asks for [`WindowFrame::square`] through
    /// [`Compositor::set_window_frame_for`].
    pub fn set_window_frame(&mut self, frame: WindowFrame) {
        self.data.state.frame = frame;
        self.data.state.mark_redraw_all();
    }

    /// Frame the window `id` with `frame` instead of the desktop's, or as the desktop frames
    /// windows again (`None`).
    pub fn set_window_frame_for(&mut self, id: u64, frame: Option<WindowFrame>) {
        match frame {
            Some(frame) => self.data.state.window_frames.insert(id, frame),
            None => self.data.state.window_frames.remove(&id),
        };
        self.data.state.mark_redraw_all();
    }

    /// Lay the windows out again, as the policy asks: for a card an app draws, whose place the
    /// floating layer moved.
    pub fn rearrange(&mut self) {
        self.data.state.rearrange();
        self.data.state.mark_redraw_all();
    }

    /// The windows that closed and are still drawn from the last buffer they showed, with the place
    /// each one stood at, front first. A window stays here until the desktop lets it go with
    /// [`Compositor::release_snapshot`], the same id maps again, or closes push it out.
    pub fn closing_windows(&self) -> Vec<(WindowInfo, (i32, i32, i32, i32))> {
        self.data
            .state
            .closing_windows()
            .into_iter()
            .map(|(info, rect)| (info, (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h)))
            .collect()
    }

    /// Let go of the snapshot of the window `id` : it is gone from the next frame on.
    pub fn release_snapshot(&mut self, id: u64) {
        self.data.state.release_snapshot(id);
        self.data.state.mark_redraw_all();
    }

    /// Night light's colour temperature, while it is on.
    pub fn night_light(&self) -> Option<u32> {
        self.data.state.gamma.night_light()
    }

    /// Apply idle transitions a session request caused, at once: what the session asked for is the
    /// person's, and their devices follow it.
    fn apply_idle(&mut self, transitions: Vec<SessionTransition>) -> Result<(), Error> {
        self.apply_idle_by(transitions, LockedBy::Person)
    }

    /// Apply idle transitions, `locked_by` saying whose the lock is when one is among them.
    fn apply_idle_by(
        &mut self,
        transitions: Vec<SessionTransition>,
        locked_by: LockedBy,
    ) -> Result<(), Error> {
        self.data.state.apply_idle(transitions, locked_by);
        self.sync_power();
        self.display.flush_clients()?;
        Ok(())
    }

    /// When the displays dim, blank, and the session locks or suspends with no input. Applied to
    /// the current idle episode: a shorter timeout can fire at the next dispatch. A hosted session
    /// runs no idle timers whatever it is given: the desktop it runs in owns idle.
    pub fn set_idle_config(&mut self, config: IdleConfig) {
        let config = if self.hosted {
            IdleConfig::never()
        } else {
            config
        };
        let session = &mut self.data.state.idle.session;
        session.set_blank_timeout(config.blank_timeout);
        session.set_lock_timeout(config.lock_timeout);
        session.set_suspend_timeout(config.suspend_timeout);
    }

    /// Whether this session runs in a window of a desktop it did not start;
    /// setting it drops every idle timer the session was given.
    pub fn set_hosted(&mut self, hosted: bool) {
        self.hosted = hosted;
        if hosted {
            self.set_idle_config(IdleConfig::never());
        }
    }

    /// Whether this session runs in a window of a desktop it did not start.
    #[must_use]
    pub const fn hosted(&self) -> bool {
        self.hosted
    }

    /// The idle timeouts the session runs on now.
    #[must_use]
    pub fn idle_config(&self) -> IdleConfig {
        self.data.state.idle.session.config()
    }

    /// The panel's backlight, which the idle dim lowers; without one the scene is darkened instead.
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

    /// Whether the lock the session has is the idle timer's rather than the person's: theirs is the
    /// one their other devices follow, while the idle timer's lock stays on this device.
    #[must_use]
    pub fn lock_by_idle(&self) -> bool {
        self.data.state.lock_by_idle()
    }

    /// Whether the idle dim holds the backlight lowered, so nothing else — the session's ambient
    /// light loop — raises it behind the person's back.
    #[must_use]
    pub fn dim_holds_backlight(&self) -> bool {
        self.data.state.idle.dimmer.holds_backlight()
    }

    /// How long since the person last touched this device.
    #[must_use]
    pub fn input_idle(&self) -> std::time::Duration {
        self.data
            .state
            .idle
            .session
            .input_idle_for(std::time::Instant::now())
    }

    /// Whether the person was using this device, here or on one of their devices nearby, when the
    /// idle lock would have fired.
    #[must_use]
    pub fn in_use(&self) -> bool {
        self.data
            .state
            .idle
            .session
            .in_use(std::time::Instant::now())
    }

    /// The person used another of their devices at `at` : idle time here counts from then, so
    /// neither device locks while they are on the other.
    pub fn note_use_elsewhere(&mut self, at: std::time::Instant) {
        self.data.state.idle.session.note_use_elsewhere(at);
    }

    /// Something outside the compositor (a download, a remote session, a keep-awake lease) holds
    /// the machine awake: it does not suspend when idle, though the screen still dims, blanks and
    /// locks.
    pub fn hold_awake(&mut self, held: bool) {
        self.data.state.idle.suspend_held = held;
    }

    /// Something outside the compositor says the person is there without touching anything (an
    /// app's `Inhibit IDLE` , a presentation): nothing dims, blanks, locks or suspends while it
    /// holds, and the idle period restarts when it lets go.
    pub fn hold_idle(&mut self, held: bool) {
        self.data.state.idle.held = held;
    }

    /// The suspend the idle timers decided on will not happen after all (a hold appeared between
    /// the tick that decided it and now): the idle period restarts rather than a sleep that never
    /// came.
    pub fn cancel_suspend(&mut self) {
        self.data.state.idle.session.cancel_suspend();
    }

    /// Lock now (a key or a command): reported as [`SessionTransition::Locked`] for the session to
    /// start its lock client.
    pub fn lock_now(&mut self) -> Result<(), Error> {
        let transitions = self
            .data
            .state
            .idle
            .session
            .request_lock(std::time::Instant::now());
        self.apply_idle(transitions)
    }

    /// Lift the session's own lock: the lock screen verified the person. A lock client's lock is
    /// the client's to lift.
    pub fn unlock(&mut self) -> Result<(), Error> {
        let transitions = self
            .data
            .state
            .idle
            .session
            .request_unlock(std::time::Instant::now());
        self.apply_idle(transitions)
    }

    /// Switch the machine to virtual terminal `vt` (numbered from 1), as
    /// Ctrl+Alt+F1..F12 does: the way out of a desktop that cannot give the
    /// machine back, to log in on a text console, end a stuck program or
    /// start Tilo again.
    ///
    /// # Errors
    /// The backend drives no virtual terminals, or the seat refused the
    /// switch.
    pub fn change_vt(&mut self, vt: i32) -> Result<(), Error> {
        self.data.backend.switch_vt(vt)?;
        Ok(())
    }

    /// Bind the liveness door beside the control socket at `control` : a heartbeat answered on the
    /// loop, and every thread's state and backtrace when asked. The socket is `control` 's with
    /// `.live` after it ([`tilo_liveness::socket_path`]). `None` when this compositor has no loop
    /// to answer from, or the socket could not be bound.
    pub fn serve_liveness(
        &self,
        control: &std::path::Path,
        bind: impl FnOnce(&std::path::Path) -> std::io::Result<std::os::unix::net::UnixListener>,
    ) -> Option<tilo_liveness::Server> {
        let handle = self.data.state.loop_handle.as_ref()?;
        tilo_liveness::serve(control, handle, bind)
    }

    /// Switch every panel off now, as the idle timeout would (the power key); input lights them
    /// again.
    pub fn blank_now(&mut self) -> Result<(), Error> {
        let transitions = self
            .data
            .state
            .idle
            .session
            .blank_now(std::time::Instant::now());
        self.apply_idle(transitions)
    }

    /// The machine is about to sleep: lock first when `lock_on_sleep` .
    pub fn prepare_for_sleep(
        &mut self,
        lock_on_sleep: bool,
        locked_by: LockedBy,
    ) -> Result<(), Error> {
        let transitions = self
            .data
            .state
            .idle
            .session
            .prepare_for_sleep(std::time::Instant::now(), lock_on_sleep);
        self.apply_idle_by(transitions, locked_by)
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

    /// Offer `offer` (bytes per format) as `selection` for a hosted surface app, as a client's copy
    /// would; Wayland and X11 clients read it from the compositor. An empty offer drops a hosted
    /// app's selection.
    pub fn set_selection(
        &mut self,
        selection: Selection,
        offer: Vec<(String, Vec<u8>)>,
    ) -> Result<(), Error> {
        self.data.state.set_host_selection(selection, offer);
        self.display.flush_clients()?;
        Ok(())
    }

    /// Start dragging `offer` (bytes per format; files as `text/uri-list` ) out of the hosted
    /// surface app the pointer is pressed on, as a client's drag would: Wayland clients, X11
    /// clients and hosted apps under the pointer are offered it until the button comes up, and the
    /// app learns how it went ([`SurfaceHost::drag_ended`]). False when there is nothing to drag or
    /// no hosted surface under the pointer.
    pub fn start_drag(&mut self, offer: Vec<(String, Vec<u8>)>) -> Result<bool, Error> {
        self.start_host_drag(HostDrag { offer, icon: None })
    }

    /// [`Compositor::start_drag`] with an icon under the pointer, or a drag that follows the finger
    /// holding the app when a touch holds it.
    pub fn start_host_drag(&mut self, drag: HostDrag) -> Result<bool, Error> {
        let started = self.data.state.start_host_drag(drag.offer, drag.icon);
        self.display.flush_clients()?;
        Ok(started)
    }

    /// Start the drags hosted apps asked for while handling input ([`SurfaceHost::take_drag`]).
    fn start_asked_drags(&mut self) {
        let asked: Vec<HostDrag> = self
            .data
            .state
            .hosts
            .iter_mut()
            .filter_map(|host| host.take_drag())
            .collect();
        for drag in asked {
            if !self.data.state.start_host_drag(drag.offer, drag.icon) {
                tracing::debug!("a hosted app's drag could not start");
            }
        }
    }

    /// How often `selection` changed hands: with [`Compositor::selection_formats`], tells a
    /// selection that was replaced by one offering the same formats from one that stayed.
    #[must_use]
    pub fn selection_changes(&self, selection: Selection) -> u64 {
        self.data.state.selections.changes(selection)
    }

    /// The formats `selection` offers now, whoever owns it.
    pub fn selection_formats(&self, selection: Selection) -> Vec<String> {
        self.data.state.selections.offered(selection).to_vec()
    }

    /// Read `selection` as `mime`: the returned pipe yields the bytes and
    /// ends when the owner is done (a client writes it as the loop runs,
    /// so read it without blocking the loop).
    pub fn receive_selection(
        &mut self,
        selection: Selection,
        mime: &str,
    ) -> Result<std::io::PipeReader, Error> {
        let (reader, writer) = std::io::pipe()?;
        self.data
            .state
            .receive_selection(selection, mime, writer.into())
            .map_err(|error| Error::Io(std::io::Error::other(error)))?;
        self.display.flush_clients()?;
        Ok(reader)
    }

    /// [`Compositor::receive_selection`] for the loop: the read never blocks,
    /// the bytes are taken as the owner writes them ([`clipboard_history::Pull`]).
    pub fn pull_selection(
        &mut self,
        selection: Selection,
        mime: &str,
    ) -> Result<clipboard_history::Pull, Error> {
        let reader = self.receive_selection(selection, mime)?;
        clipboard_history::Pull::new(reader, mime).map_err(Error::Io)
    }

    /// The executables of display tools the person trusts: their risky changes apply without asking
    /// (see [`DisplayEvent::Confirm`]).
    pub fn set_trusted_display_tools(
        &mut self,
        tools: impl IntoIterator<Item = std::path::PathBuf>,
    ) {
        self.data.state.display_changes.set_trusted(tools);
    }

    /// What happened to display tools' changes since the last call.
    pub fn take_display_events(&mut self) -> Vec<DisplayEvent> {
        self.data.state.display_changes.take_events()
    }

    /// The person keeps the change they were asked about; false when `id` is no longer the open
    /// question.
    pub fn keep_display_change(&mut self, id: u64) -> bool {
        let heads = self.data.state.heads_now();
        self.data.state.display_changes.keep(id, &heads)
    }

    /// The person reverts the change they were asked about; false when `id` is no longer the open
    /// question.
    pub fn revert_display_change(&mut self, id: u64) -> Result<bool, Error> {
        let Some(undo) = self
            .data
            .state
            .display_changes
            .revert(id, display_clock_now())
        else {
            return Ok(false);
        };
        self.put_back(&undo);
        self.display.flush_clients()?;
        Ok(true)
    }

    /// Put outputs back after a reverted change.
    fn put_back(&mut self, undo: &[OutputChange]) {
        let LoopData { state, backend, .. } = &mut self.data;
        if let Err(error) = backend.configure_checked(state, undo, false) {
            tracing::warn!(%error, "reverting a display change failed");
        }
        self.configure_from_clients();
    }

    /// Every known output, on or off, with the colour each shows.
    pub fn outputs(&self) -> Vec<output::OutputInfo> {
        let backend = &*self.data.backend;
        self.data
            .state
            .output_infos(|output| backend.output_colour(output))
    }

    /// The monitors of the displays a fullscreen window owns: the bar there steps aside and leaves
    /// the display to the window.
    pub fn fullscreen_monitors(&self) -> Vec<String> {
        self.data.state.fullscreen_monitors()
    }

    /// The outputs as Settings reads them: each one's connector, modes,
    /// scale, place and what its sink and the pipeline driving it can do.
    /// Updated by [`Compositor::publish_displays`], which the loop runs on
    /// every pass; `None` before any output exists.
    pub fn display_snapshot(&self) -> Option<&settings_store::hardware::DisplayHardwareSnapshot> {
        self.displays.snapshot()
    }

    /// A counter that changes exactly when the published displays do, for a reader that watches
    /// them without copying the snapshot every pass.
    pub fn displays_generation(&self) -> u64 {
        self.displays.generation()
    }

    /// Take the outputs as they are now for Settings and the session: what changed since the last
    /// pass is written where Settings reads it.
    pub fn publish_displays(&mut self) {
        let state = &self.data.state;
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
        let backend = &*self.data.backend;
        let capabilities = backend.output_capabilities();
        self.displays
            .refresh(&outputs, &capabilities, backend.describe());
    }

    /// The outputs a fullscreen window owns, by connector name: the shell's bar there steps aside
    /// and leaves the display to the window, on the same displays as
    /// [`Compositor::fullscreen_monitors`].
    pub fn fullscreen_outputs(&self) -> Vec<String> {
        self.data.state.fullscreen_outputs()
    }

    /// Whether the panel of the output named `name` is lit (not blanked,
    /// switched off by a tool or turned off); `false` when no output is
    /// there to light.
    pub fn output_lit(&self, name: &str) -> bool {
        self.data
            .state
            .space
            .outputs()
            .any(|output| output.name() == name)
            && self.data.state.output_power.lit(name)
    }

    /// Apply (or test) the display configurations clients sent and tell them how it went, then show
    /// every manager the outputs as they are.
    fn configure_from_clients(&mut self) {
        let state = &mut self.data.state;
        for pending in state.output_management.take_pending() {
            let Pending {
                configuration,
                changes,
                test,
                tool,
            } = pending;
            let now = display_clock_now();
            if !test
                && state
                    .display_changes
                    .refuses_reapply(tool.as_ref(), &changes, now)
            {
                configuration.failed();
                continue;
            }
            let risk =
                state
                    .display_changes
                    .risk(&changes, &state.heads_now(), &state.watched_outputs());
            let undo = state.undo_for(&changes);
            if let Err(error) = self.data.backend.configure_checked(state, &changes, test) {
                tracing::info!(%error, "display configuration refused");
                configuration.failed();
                continue;
            }
            configuration.succeeded();
            if test {
                continue;
            }
            if state.display_changes.keeps_unasked(risk, tool.as_ref()) {
                let heads = state.heads_now();
                state.display_changes.kept(changes, &heads);
            } else {
                state.display_changes.ask(tool, changes, undo, now);
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

    /// Change outputs' modes, scales, rotations and places (as a display tool would through
    /// `wlr-output-management` ). All or nothing.
    pub fn configure_outputs(&mut self, changes: &[OutputChange]) -> Result<(), Error> {
        self.data
            .backend
            .configure_checked(&mut self.data.state, changes, false)?;
        self.configure_from_clients();
        self.display.flush_clients()?;
        Ok(())
    }

    /// Apply everything the person configured (see [`Settings`]), now and for outputs and devices
    /// that appear later.
    pub fn apply_settings(&mut self, settings: &Settings) -> Result<(), Error> {
        self.set_display_config(&settings.display)?;
        self.set_input_config(settings.input.clone());
        self.set_keyboard_layout(&settings.keyboard);
        self.set_idle_config(settings.idle);
        self.set_a11y(settings.a11y.clone())?;
        Ok(())
    }

    /// Apply the person's display configuration to the outputs there are and keep it for those
    /// plugged in later. A configuration the outputs cannot take is refused whole.
    pub fn set_display_config(&mut self, config: &DisplayConfig) -> Result<(), Error> {
        self.data.backend.set_display_config(config);
        if config.night_light != self.night_light() {
            self.set_night_light(config.night_light)?;
        }
        let changes = config.changes(&self.data.state.output_states());
        if !changes.is_empty() {
            self.configure_outputs(&changes)?;
        }
        // Which display is primary is told to the desk after the outputs are as the configuration
        // asked, so it is resolved against the displays that are lit now.
        self.data.state.set_primary_output(config.primary.clone());
        Ok(())
    }

    /// Type with the person's keyboard layout.
    pub fn set_keyboard_layout(&mut self, layout: &KeyboardLayout) {
        self.data.state.keyboard = Some(layout.clone());
        // X11 clients type in the seat's layout too, and everything that asks which layout is in
        // force hears the same.
        for server in &mut self.data.state.xwms {
            if let Some(windows) = server.windows.as_mut() {
                windows.publish_keyboard(layout);
            }
        }
        let LoopData { state, .. } = &mut self.data;
        let Some(keyboard) = state.seat.get_keyboard() else {
            return;
        };
        let xkb = smithay::input::keyboard::XkbConfig {
            // Every name is stated: an empty one would take the machine's (`XKB_DEFAULT_*`), so a
            // person's chosen layout would type with the host's rules and options mixed in.
            rules: "evdev",
            model: &layout.model,
            layout: &layout.layout,
            variant: &layout.variant,
            options: Some(layout.options.clone()),
        };
        if let Err(error) = keyboard.set_xkb_config(state, xkb) {
            tracing::warn!(
                ?error,
                layout = layout.layout,
                "keyboard layout did not compile"
            );
        }
    }

    /// The layout the keyboard types in now, as XKB names it; `None` without a keyboard.
    pub fn keyboard_layout(&mut self) -> Option<String> {
        let LoopData { state, .. } = &mut self.data;
        let keyboard = state.seat.get_keyboard()?;
        Some(keyboard.with_xkb_state(state, |context| {
            context.xkb().lock().map_or_else(
                |_| String::new(),
                |xkb| xkb.layout_name(xkb.active_layout()).to_owned(),
            )
        }))
    }

    /// Apply the person's input preferences (pointer, touchpad, tablets,
    /// accessibility filters) to the backend's devices, now and as they are
    /// plugged in. Backends without devices of their own ignore them.
    pub fn set_input_config(&mut self, config: InputConfig) {
        let (delay, rate) = config.key_repeat_params();
        self.set_key_repeat(KeyRepeat { delay, rate });
        let LoopData { state, backend, .. } = &mut self.data;
        backend.set_input_config(state, config);
    }

    /// Where the pointer is in the layout.
    #[must_use]
    pub fn pointer_location(&self) -> (f64, f64) {
        let pointer = self.data.state.pointer;
        (pointer.x, pointer.y)
    }

    /// Give every input to `observer` before the desktop sees it; what it takes (returns `true`
    /// for) is not routed. An input capture takes the input while it holds the person's keyboard
    /// and pointer.
    pub fn set_input_observer(
        &mut self,
        observer: Option<Arc<dyn Fn(ObservedInput) -> bool + Send + Sync>>,
    ) {
        self.data.state.input_observer = observer;
    }

    /// The mapped windows an app may capture: its foreign-toplevel identifier, app id and title,
    /// bottom first.
    #[must_use]
    pub fn shareable_windows(&self) -> Vec<(String, Option<String>, String)> {
        self.data
            .state
            .capture
            .shareable_windows()
            .into_iter()
            .map(|(identifier, info)| (identifier, info.app_id, info.title.unwrap_or_default()))
            .collect()
    }

    /// Where a shareable window (`Compositor::shareable_windows`'s identifier) is in the layout.
    #[must_use]
    pub fn shareable_window_rect(&self, identifier: &str) -> Option<(i32, i32, i32, i32)> {
        self.data.state.shareable_window_rect(identifier)
    }

    /// Where the mapped window whose [`WindowInfo::id`] is `id` stands in the layout:
    /// `(x, y, width, height)` of the content, which is what a tool driving the desk aims at.
    /// `None` for a window that is not mapped or not placed on a display.
    #[must_use]
    pub fn window_rect(&self, id: u64) -> Option<(i32, i32, i32, i32)> {
        self.data.state.window_rect(id)
    }

    /// The foreign-toplevel identifier of the mapped window whose [`WindowInfo::id`] is `id` : what
    /// a capture names a window by, and how what the desk knows of a window reaches what it shows.
    #[must_use]
    pub fn capture_identifier(&self, id: u64) -> Option<String> {
        self.data
            .state
            .capture
            .shareable_windows()
            .into_iter()
            .find(|(_, info)| info.id == id)
            .map(|(identifier, _)| identifier)
    }

    /// The evdev code of a key of the active layout that produces `keysym` ; `None` when the layout
    /// cannot type it.
    #[must_use]
    pub fn keycode_for_keysym(&mut self, keysym: u32) -> Option<u32> {
        self.data.state.keycode_for_keysym(keysym)
    }

    /// Whether the person is in the middle of something an arrangement must not move under, and
    /// why. Shell.md §1's one fixed rule reads it.
    #[must_use]
    pub fn underway(&self) -> Underway {
        self.data.state.underway()
    }

    /// Route one input event (see [`InputEvent`]) and flush what it sent. Route one input event
    /// (see [`InputEvent`]) through the person's assists and flush what it sent.
    pub fn input(&mut self, event: InputEvent) -> Result<(), Error> {
        let now = Instant::now();
        self.recorder.note_input(now);
        let events = self.keys.filter(event, now);
        for event in events {
            self.data.state.note_typing(&event, now);
            self.data.state.handle_input(event);
        }
        if self.keys.take_alert() {
            self.flash();
        }
        self.start_asked_drags();
        self.sync_text_input();
        // Input wakes blanked panels at once.
        self.sync_power();
        self.display.flush_clients()?;
        Ok(())
    }

    /// Keep the text fields the person can type into on the keyboard's focus: clients hear about
    /// the field that has it and the input method that serves it, whether the field is a client's
    /// or the session's own.
    fn sync_text_input(&mut self) {
        self.data.state.sync_text_input();
    }

    /// Whether an input method — the on-screen keyboard, a composing engine — is connected now, and
    /// so serves the field the person types into.
    #[must_use]
    pub fn input_method_connected(&self) -> bool {
        self.data.state.input_method_connected()
    }

    /// Draw a frame on every output that can take one and tell clients it was shown.
    pub fn render(&mut self) -> Result<(), Error> {
        self.sync_text_input();
        self.seed_visuals();
        // An alert asked for between frames runs from the frame that shows it,
        // however late that frame is, and an alert whose run is over goes away
        // on the frame that draws the scene without it.
        a11y::begin_frame(&mut self.data.state, Instant::now());
        // The visual alert fades frame by frame, so each frame it covers is a new one to draw.
        if a11y::flashing(&self.data.state, Instant::now()) {
            self.data.state.mark_redraw_all();
        }
        let mut drew = false;
        // The displays hold each frame for this long: a surface whose content only moves draws one
        // frame per one of these, so what the person sees runs at the screens' rate rather than at
        // the loop's.
        let interval = self.data.state.frame_interval();
        for host in &mut self.data.state.hosts {
            host.set_frame_interval(interval);
            drew |= host.prepare_frame();
        }
        if drew {
            self.data.state.mark_redraw_all();
        }
        // What the surfaces draw over the windows, by the window each part is of: a floating window
        // stands over the ones beneath it whole, so they are drawn around what the desk covers them
        // with for it, and a press there is the desk's.
        let mut covers = std::collections::BTreeMap::new();
        for host in &self.data.state.hosts {
            for (window, cover) in host.drawn_over_windows() {
                covers.insert(window, cover);
            }
        }
        if self.data.state.covers != covers {
            self.data.state.covers = covers;
            self.data.state.mark_redraw_all();
        }
        // A frame can move windows (a strip panning, a group resizing): the policy places them
        // where the surfaces just laid them out.
        self.data.state.rearrange();
        let drawn_before = self.data.state.frames_drawn();
        let submitting = Instant::now();
        self.data.backend.render(&mut self.data.state)?;
        let drawn = self.data.state.frames_drawn();
        if drawn > drawn_before {
            self.recorder.mark_submit(
                submitting.elapsed(),
                self.data.backend.presents_async(),
                Instant::now(),
                Some(drawn),
            );
        } else {
            // Nothing was drawn: no frame will answer this turn's input.
            self.recorder.settle_unpainted_input();
        }
        // The frame is on the output now, so the clients that drew it hear about it before anything
        // the frame itself asked for is served: a capture to read back, a blur of what lies below a
        // window. Those go with the frame but are not what answers it, so they run behind it and
        // the client starts its next frame while they are made.
        self.display.flush_clients()?;
        self.data.backend.serve_captures(&mut self.data.state);
        self.data.backend.serve_backdrops(&mut self.data.state);
        // Answers read back after the frame's own pictures, and whatever is still queued for
        // clients, reach them here.
        self.data.state.confirm_lock();
        self.display.flush_clients()?;
        Ok(())
    }

    /// What an output (or part of one) or a window shows now, read back as rows of R, G, B, A: what
    /// a share stream pushes.
    pub fn read_back(&mut self, target: &capture::Target) -> Result<Screenshot, Error> {
        Ok(self
            .data
            .backend
            .read_back(&mut self.data.state, target, capture::Held::Back)?)
    }

    /// What a display shows now as the person in front of it sees it — the
    /// session's own look at its displays, for a check, not a copy that
    /// leaves the session: a locked display is read as the lock screen, not
    /// as the placeholder a screenshot of it gets.
    ///
    /// # Errors
    /// The display is gone or asleep, or nothing could be rendered.
    pub fn read_back_shown(&mut self, target: &capture::Target) -> Result<Screenshot, Error> {
        Ok(self
            .data
            .backend
            .read_back(&mut self.data.state, target, capture::Held::Nothing)?)
    }

    /// What `region` of the output `name` shows now — its top-left corner
    /// and size in that output's physical pixels, as shown — read back as
    /// rows of R, G, B, A.
    ///
    /// # Errors
    /// The display is gone or asleep, or nothing could be rendered.
    pub fn read_back_region(
        &mut self,
        name: &str,
        region: (i32, i32, i32, i32),
    ) -> Result<Screenshot, Error> {
        let (x, y, width, height) = region;
        self.read_back(&capture::Target::Output {
            name: name.to_owned(),
            region: smithay::utils::Rectangle::new(
                (x, y).into(),
                (width.max(1), height.max(1)).into(),
            ),
        })
    }

    /// What `target` shows now, drawn to fit `max` — its widest and tallest
    /// frame — read back as rows of R, G, B, A: one frame of the stream a
    /// wireless display is sent.
    ///
    /// # Errors
    /// The target is gone or asleep, the session is locked, or nothing could
    /// be rendered.
    pub fn read_back_scaled(
        &mut self,
        target: &capture::Target,
        max: (u32, u32),
    ) -> Result<Screenshot, Error> {
        Ok(self
            .data
            .backend
            .read_back_scaled(&mut self.data.state, target, max)?)
    }

    /// The last frame drawn on the output named `name` .
    pub fn screenshot(&mut self, name: &str) -> Result<Screenshot, Error> {
        Ok(self.data.backend.screenshot(&mut self.data.state, name)?)
    }

    /// Route one input event from another device — a portal `RemoteDesktop` or `InputCapture`
    /// session, a share, a remote-typing link — through the firewall ([`remote_input`]): it reaches
    /// the seat only while the person allows it.
    pub fn input_remote(&mut self, event: InputEvent) -> Result<(), Error> {
        self.data.state.handle_remote_input(event);
        self.start_asked_drags();
        self.sync_power();
        self.display.flush_clients()?;
        Ok(())
    }

    /// Why input from another device is refused now — the person cut it, they are using this
    /// machine's own keyboard or pointer, or the session is locked. `None` when it would be let in.
    /// What a share checks before it acts for a remote viewer at the viewer's own seat.
    #[must_use]
    pub fn remote_input_refusal(&self) -> Option<&'static str> {
        self.data
            .state
            .remote_input
            .refusal(std::time::Instant::now(), self.data.state.locked())
    }

    /// Open a seat of its own: a pointer and a keyboard beside the person's,
    /// which only what it aims at hears (see [`seats`]).
    pub fn open_seat(&mut self) -> Seat {
        self.data.state.seats.open()
    }

    /// End `seat` : its window sees the release of every key and button it still held, and the seat
    /// is gone.
    pub fn close_seat(&mut self, seat: Seat) {
        self.data.state.close_seat(seat);
    }

    /// Aim `seat` at the window of `app`, or at nothing for the whole screen:
    /// its coordinates are that window's pixels from its top-left, its keys
    /// go to it, and its screenshots show it.
    ///
    /// # Errors
    /// No window of that app is open, or the compositor has no screen.
    pub fn seat_aim_app(&mut self, seat: Seat, app: Option<&str>) -> Result<Aim, String> {
        self.data.state.seat_aim_app(seat, app)
    }

    /// Aim `seat` at the window a foreign-toplevel identifier names, as a
    /// shared window is named.
    ///
    /// # Errors
    /// No such window is open on this desk.
    pub fn seat_aim_window(&mut self, seat: Seat, identifier: &str) -> Result<Aim, String> {
        self.data.state.seat_aim_window(seat, identifier)
    }

    /// What `seat` aims at now.
    #[must_use]
    pub fn seat_view(&self, seat: Seat) -> Option<Aim> {
        self.data.state.seat_view(seat)
    }

    /// Where `seat` left its pointer, in the layout.
    #[must_use]
    pub fn seat_pointer(&self, seat: Seat) -> Option<(f64, f64)> {
        self.data
            .state
            .seats
            .get(seat)
            .and_then(|held| held.pointer)
    }

    /// Route one input event as `seat`: its pointer, its keys, in the window it
    /// aims at, with the person's pointer, focus and holds put back after.
    /// Whether the seat took it (it takes none while the screen is locked).
    ///
    /// # Errors
    /// The event could not be flushed to the clients.
    pub fn seat_input(&mut self, seat: Seat, event: &InputEvent) -> Result<bool, Error> {
        if let InputEvent::Key {
            keycode,
            pressed: false,
        } = &event
        {
            // The seat lets go of the key before its release is delivered: it is not pressed into
            // it again for the release.
            self.data.state.seat_forget_key(seat, *keycode);
        }
        let Some(person) = self.data.state.seat_begin(seat) else {
            return Ok(false);
        };
        self.data.state.seat_deliver(seat, event);
        self.data.state.seat_end(seat, person);
        self.sync_power();
        self.display.flush_clients()?;
        Ok(true)
    }

    /// Type `text` as `seat`, by the keys the layout puts its characters on:
    /// what the person's keyboard would type into that window. Whether every
    /// character was a key of the layout (the rest are written as text).
    ///
    /// # Errors
    /// The keys could not be flushed to the client.
    pub fn seat_text(&mut self, seat: Seat, text: &str) -> Result<bool, Error> {
        let Some(person) = self.data.state.seat_begin(seat) else {
            return Ok(false);
        };
        self.sync_text_input();
        let mut typed = true;
        for ch in text.chars() {
            let Some(stroke) = self.data.state.stroke_for(ch) else {
                typed = false;
                let _ = self.data.state.insert_active_text(&ch.to_string());
                continue;
            };
            let time = self.data.state.now_ms();
            for modifier in &stroke.modifiers {
                let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                self.data.state.key(modifier.key, true, serial, time);
            }
            for pressed in [true, false] {
                let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                self.data.state.key(stroke.key, pressed, serial, time);
            }
            for modifier in stroke.modifiers.iter().rev() {
                let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                self.data.state.key(modifier.key, false, serial, time);
            }
        }
        self.data.state.seat_end(seat, person);
        self.sync_power();
        self.display.flush_clients()?;
        Ok(typed)
    }

    /// Ask the window `seat` aims at to close, as its own close button would. Whether it had a
    /// window to ask.
    pub fn seat_close_window(&mut self, seat: Seat) -> bool {
        let Some(aim) = self
            .data
            .state
            .seats
            .get(seat)
            .and_then(|held| held.aim.clone())
        else {
            return false;
        };
        self.data.state.close_window(aim.id);
        true
    }

    /// Cut every remote input: nothing from another device reaches the seat until the person allows
    /// it again ([`Compositor::allow_remote_input`]). The person's own Ctrl+Alt+Escape does this
    /// too.
    pub fn cut_remote_input(&mut self) {
        self.data.state.remote_input.cut();
    }

    /// Let remote input in again, after a cut.
    pub fn allow_remote_input(&mut self) {
        self.data.state.remote_input.allow();
    }

    /// Whether remote input is cut.
    #[must_use]
    pub fn remote_input_cut(&self) -> bool {
        self.data.state.remote_input.is_cut()
    }

    /// Mark every window of `app_id` as private, or no longer: screenshots,
    /// recordings and share streams show a placeholder where they are,
    /// while the person's own display keeps showing them as they are.
    pub fn protect_app(&mut self, app_id: &str, private: bool) {
        self.data.state.capture.protection.app(app_id, private);
        self.data.state.mark_redraw_all();
    }

    /// The processes whose clients disconnected since the last look: the apps that exited.
    pub fn take_exited_pids(&mut self) -> Vec<u32> {
        self.data.state.take_exited_pids()
    }

    /// The areas the layer surfaces on `output` keep for themselves at its edges, as logical
    /// rectangles `(x, y, w, h)` : what the desktop keeps windows and the desk itself out of (the
    /// bar's height along the top).
    pub fn reserved(&self, output: &str) -> Vec<(i32, i32, i32, i32)> {
        self.data
            .state
            .space
            .outputs()
            .find(|known| known.name() == output)
            .map(|output| self.data.state.layer_reserved(output))
            .unwrap_or_default()
            .into_iter()
            .map(|rect| (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h))
            .collect()
    }

    /// Show the session's layer surface `namespace` (`tilo-quick-settings`),
    /// or hide it: a surface whose app the session keeps running between
    /// openings stands hidden instead — drawing nothing, taking no input and
    /// keeping no room — so opening it again costs no start.
    pub fn show_layer(&mut self, namespace: &str, shown: bool) {
        self.data.state.set_layer_shown(namespace, shown);
    }

    /// Whether the session shows its layer surface `namespace` , hid it, or never said (shown).
    #[must_use]
    pub fn layer_shown(&self, namespace: &str) -> Option<bool> {
        self.data.state.layer_shown(namespace)
    }

    /// Whether the app of the layer surface `namespace` has drawn it: a surface shows nothing
    /// before its first buffer, so a motion about it starts with that frame.
    #[must_use]
    pub fn layer_drawn(&self, namespace: &str) -> bool {
        self.data.state.layer_drawn(namespace)
    }

    /// Draw the layer surface `namespace` as `pose` says from the next frame on, or as it stands
    /// again for `None` : how the session moves a panel of its own, whose app cannot know it was
    /// shown or hidden.
    pub fn set_layer_pose(&mut self, namespace: &str, pose: Option<LayerPose>) {
        self.data.state.set_layer_pose(namespace, pose);
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

    /// The colour the client of the window `id` says its content is in: the image description it
    /// attached through `wp_color_management_v1` , or `None` when it attached none. A share stream
    /// tags its frames with it.
    #[must_use]
    pub fn window_color(
        &self,
        id: u64,
    ) -> Option<(
        neighbour_wire::colorimetry::Colorimetry,
        Option<neighbour_wire::hdr_metadata::Hdr10Metadata>,
    )> {
        self.data.state.window_color(id)
    }

    /// What the X11 window `id` says about its program: its process, class,
    /// desktop start-up and icon. `None` for a Wayland window.
    pub fn x11_identity(&self, id: u64) -> Option<&X11Identity> {
        let window = self.data.state.mapped_window(id)?;
        let surface = window.x11_surface()?;
        let server = self.data.state.x11_server(surface.xwm_id()?)?;
        server.windows.as_ref()?.identity(surface.window_id())
    }

    /// Ask the window with `id` to close, as its own close button would: the client decides what
    /// closing means, and does it.
    pub fn close_window(&mut self, id: u64) {
        self.data.state.close_window(id);
    }

    /// Judge every app the person sent input to: one that has not answered its ping within
    /// [`app_liveness::UNANSWERED`] is not responding. Returns whether the set of such windows
    /// changed. The loop calls this on its own when [`Compositor::next_wait`] says a verdict is
    /// due; tests pass the instant to judge at.
    pub fn advance_app_liveness_at(&mut self, now: Instant) -> bool {
        self.data.state.advance_app_liveness_at(now)
    }

    /// The windows whose app is not responding now, by id.
    #[must_use]
    pub fn not_responding_windows(&self) -> &[u64] {
        self.data.state.not_responding()
    }

    /// The person chose "Wait" on window `id` : its app gets another period to answer. Returns
    /// whether the windows that say "Not responding" changed.
    pub fn wait_window(&mut self, id: u64) -> bool {
        self.data.state.wait_for_window(id)
    }

    /// The person chose "Force quit" on window `id` : the client's connection ends, so its windows
    /// go now, and the process it was is returned for the session to end.
    pub fn force_quit_window(&mut self, id: u64) -> Option<u32> {
        self.data.state.force_quit_window(id)
    }

    /// Whether a client keeps the session from idling (idle-inhibit).
    pub fn idle_inhibited(&self) -> bool {
        self.data.state.idle_inhibited()
    }

    /// The windows whose idle inhibitor is on screen: what holds the machine awake, for the session
    /// to say who it is.
    pub fn visible_idle_inhibitors(&self) -> Vec<WindowInfo> {
        self.data.state.visible_idle_inhibitors()
    }

    /// The window with keyboard focus.
    pub fn focused_window(&self) -> Option<WindowInfo> {
        self.data.state.focused_window()
    }

    /// The identifier the focused window can be captured under, when it has focus and can be
    /// captured at all: a capture that names no window takes this one.
    #[must_use]
    pub fn focused_shareable_window(&self) -> Option<String> {
        let focused = self.focused_window()?.id;
        self.data
            .state
            .capture
            .shareable_windows()
            .into_iter()
            .find(|(_, info)| info.id == focused)
            .map(|(identifier, _)| identifier)
    }

    /// An activation token for something the session itself did (the person chose a notification,
    /// so its app should come to the front): handing it to the app lets that app raise its window.
    pub fn activation_token(&mut self) -> String {
        self.data.state.protocols.external_activation_token()
    }

    /// A waker for the loop, from any thread: waking it ends the wait of [`Compositor::dispatch`],
    /// so news of a surface app's background work is drawn without waiting for the next input or
    /// due frame.
    #[must_use]
    pub fn waker(&self) -> std::task::Waker {
        std::task::Waker::from(std::sync::Arc::new(LoopWaker(self.event_loop.get_signal())))
    }
}

/// A descriptor [`Compositor::watch_readable`] polls for the loop: its readiness ends the wait, and
/// dropping the watch stops the polling.
#[derive(Debug)]
pub struct ReadableWatch {
    handle: LoopHandle<'static, LoopData>,
    token: smithay::reexports::calloop::RegistrationToken,
}

impl Drop for ReadableWatch {
    fn drop(&mut self) {
        self.handle.remove(self.token);
    }
}

/// Wakes the compositor's loop.
struct LoopWaker(smithay::reexports::calloop::LoopSignal);

impl std::task::Wake for LoopWaker {
    fn wake(self: std::sync::Arc<Self>) {
        self.0.wakeup();
    }

    fn wake_by_ref(self: &std::sync::Arc<Self>) {
        self.0.wakeup();
    }
}

impl std::os::fd::AsFd for Compositor {
    /// The loop's descriptor, readable when [`Compositor::dispatch`] has work, for embedding in
    /// another loop.
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.event_loop.as_fd()
    }
}
