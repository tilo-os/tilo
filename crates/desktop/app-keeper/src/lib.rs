//! The app keeper: keeps one app's Wayland windows open while the desktop restarts.
//!
//! A program that is not a Tilo app (a Linux VM's app forwarder, a Wine or Proton program, a
//! remote app, any Wayland app) loses its windows, and usually its life, when its compositor
//! goes away. The desktop restarts in place to apply an update, so such a program is started
//! behind a keeper: to the program the keeper is the Wayland display, a real server holding all
//! of its surface state; to the desktop it is one ordinary client mirroring the program's
//! windows. When the desktop goes away the keeper keeps the program's connections, its state and
//! its last buffers; when the next desktop hands it a new connection
//! ([`wire::ToKeeper::Attach`]) it recreates the windows from what it holds, showing their last
//! pictures at once. The program sees no more than a pause in frame callbacks and a `suspended`
//! window state.
//!
//! The keeper is small on purpose: a running keeper is never replaced by an update (new
//! launches use the new binary, running ones finish with their app), so it should change
//! rarely.
//!
//! # How state travels
//!
//! There is one path up. A client's commit is taken into the surface's held record
//! (`State::absorb`), and `State::push` sends the held state to the desktop, compared with what
//! that same desktop was sent before. A fresh desktop was sent nothing, so attaching pushes
//! every held surface in creation order, and its objects are made on the way
//! (`State::ensure_up`). Buffers are never copied: a shared-memory buffer is made again on the
//! desktop from the app's own pool descriptor, a dma-buf from its own planes.
//!
//! # Adding a protocol
//!
//! A global the keeper does not forward is not offered to the app. To forward one more:
//!
//! 1. offer it to the app (smithay's state for it in `State::new`, or a `GlobalDispatch` of
//!    the keeper's own as `seat.rs` does for `wl_seat`), and keep what its requests set in the
//!    held record (`surfaces::Kept` for per-surface state, a field of `State` otherwise);
//! 2. bind the desktop's global in the registry handler (`upstream.rs`), at a version no
//!    newer than the app is offered;
//! 3. make its desktop objects in `State::ensure_up` (per surface) or after the globals are
//!    bound, and send what changed in `State::push`, with a `surfaces::Pushed` field per
//!    value so a fresh desktop gets all of it and a commit only what changed;
//! 4. forward the desktop's events in its `wayland_client::Dispatch`, and decide what the app
//!    is told while no desktop is attached (`State::hold_all`).
//!
//! Linux only.

#![cfg(target_os = "linux")]

mod activation;
mod child;
mod color;
mod control;
mod cursor_shape;
mod data_device;
mod decorations;
mod dialog;
mod dmabuf;
mod fractional;
mod idle_inhibit;
mod keyboard_shortcuts_inhibit;
pub mod output;
mod outputs;
mod pings;
mod pointer_constraints;
mod pointer_gestures;
mod presentation;
mod primary_selection;
pub mod registry;
mod relative_pointer;
mod seat;
mod state;
mod surfaces;
mod tablet;
mod text_input;
mod toplevel_icon;
mod upstream;
mod viewporter;
pub mod wire;
mod x11;
mod x11_windows;
mod xdnd;

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::os::fd::AsFd as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{
    EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken,
};
use smithay::reexports::wayland_server::Display;
use smithay::reexports::wayland_server::backend::DisconnectReason;
use smithay::reexports::wayland_server::backend::protocol::ProtocolError;
use wayland_client::backend::WaylandError;
use wayland_client::{DispatchError, EventQueue};

use control::Conn;
pub use control::peer_allowed;
use state::{ClientState, State};
use upstream::Upstream;
use wire::{Description, End, FromKeeper, INTERFACE_VERSION, ToKeeper};

/// How long a keeper whose app ended waits for a session to hear it, and how long one waits
/// for its first desktop before it gives up.
pub const GRACE: Duration = Duration::from_secs(60);

/// How long a running app waits for a desktop to come back. An in-place restart, even one that
/// fails and rolls back to the build before, brings a desktop back within a few minutes; one
/// that has not come back by then is not restarting: the login ended (a desktop crash ends it),
/// and the app ends with it instead of running on with no window anyone can see.
pub const DESKTOP_GONE: Duration = Duration::from_secs(5 * 60);

/// What a keeper keeps.
#[derive(Clone, Debug)]
pub struct Options {
    /// The keeper's private directory (mode 0700): its Wayland socket, its control socket and
    /// the record of how the app ended.
    pub dir: PathBuf,
    /// What the session calls this launch, opaque to the keeper.
    pub launch: String,
    /// Prefixed to every app id the desktop is given.
    pub app_id_prefix: Option<String>,
    /// The app's command; none starts nothing (an app connects to the socket itself).
    pub command: Vec<OsString>,
    /// Serve the app an X11 display of its own (`x11`), its server a client of the keeper.
    pub x11: bool,
    /// [`GRACE`], shorter in a test.
    pub grace: Duration,
    /// [`DESKTOP_GONE`], shorter in a test.
    pub desktop_gone: Duration,
    /// Test only: once this file exists the keeper speaks as one of an older build, at version 0;
    /// it holds its app for a session at another version unless the file says `closes`, as a
    /// keeper from before that rule does.
    #[cfg(feature = "test-support")]
    pub aged_by: Option<PathBuf>,
}

impl Options {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>, launch: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            launch: launch.into(),
            app_id_prefix: None,
            command: Vec::new(),
            x11: false,
            grace: GRACE,
            desktop_gone: DESKTOP_GONE,
            #[cfg(feature = "test-support")]
            aged_by: None,
        }
    }

    /// The options a command line names, past the program's own name:
    /// `--dir <private dir> --launch <name> [--app-id-prefix <prefix>] -- <command...>`.
    ///
    /// # Errors
    /// An argument the keeper does not take, a missing directory or launch, or no command.
    pub fn from_args(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut args = args.into_iter();
        let mut dir = None;
        let mut launch = None;
        let mut prefix = None;
        let mut x11 = false;
        let mut command: Vec<OsString> = Vec::new();
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--dir") => dir = args.next(),
                Some("--launch") => launch = args.next().and_then(|value| value.into_string().ok()),
                Some("--app-id-prefix") => {
                    prefix = args.next().and_then(|value| value.into_string().ok());
                }
                Some("--x11") => x11 = true,
                Some("--") => command.extend(args.by_ref()),
                _ => return Err(format!("unexpected argument {}", arg.to_string_lossy())),
            }
        }
        let (Some(dir), Some(launch)) = (dir, launch) else {
            return Err(
                "usage: tilo-app-keeper --dir <private dir> --launch <name> [--app-id-prefix <prefix>] [--x11] -- <command...>"
                    .to_owned(),
            );
        };
        if command.is_empty() {
            return Err("no command after --".to_owned());
        }
        let mut options = Self::new(dir, launch);
        options.app_id_prefix = prefix;
        options.command = command;
        options.x11 = x11;
        Ok(options)
    }
}

/// One keeper: its loop and everything it holds.
pub struct Keeper {
    event_loop: EventLoop<'static, Data>,
    data: Data,
}

struct Data {
    state: State,
    display: Display<State>,
    handle: LoopHandle<'static, Data>,
    up_queue: Option<EventQueue<State>>,
    up_token: Option<RegistrationToken>,
    conns: HashMap<u64, (Conn, RegistrationToken)>,
    next_conn: u64,
    own_uid: u32,
    options: Options,
    socket: PathBuf,
    created: Instant,
    app: Option<child::App>,
    app_token: Option<RegistrationToken>,
    /// The launch's X11 display, when it serves one.
    x11: Option<x11::XDisplay>,
    /// The keeper was asked for an X11 display and has none: the app gets no `DISPLAY`.
    x11_missing: bool,
    ended: Option<End>,
    ended_at: Option<Instant>,
    /// Since when the running app has had no desktop, and whether it was asked to end for it.
    desktop_lost: Option<(Instant, Option<Instant>)>,
    delivered: bool,
    finished: bool,
    /// When the keeper was told to end (SIGTERM, SIGHUP or SIGINT, as a login's end sends them):
    /// it ends once its app did, so what it holds of the display (its X11 lock and socket) goes
    /// with it.
    told_to_end: Option<Instant>,
}

impl Keeper {
    /// Bind the keeper's sockets in `options.dir`. The app starts once the first desktop is
    /// attached, so it never sees a display without outputs or a seat.
    pub fn new(options: Options) -> io::Result<Self> {
        // A write to a socket or pipe whose reader went (the desktop, a session, the launcher
        // relaying the keeper's output) is an error to handle, never a signal that ends the
        // keeper and with it the app. The app starts with the default again (`child.rs`).
        // SAFETY: setting a signal's disposition takes plain integers.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        }
        private_dir(&options.dir)?;
        let display = Display::<State>::new().map_err(io::Error::other)?;
        let dh = display.handle();
        let state = State::new(&dh, options.app_id_prefix.clone());
        let event_loop = EventLoop::<Data>::try_new().map_err(io::Error::other)?;
        let handle = event_loop.handle();
        let mut display = display;
        let display_fd = display.backend().poll_fd().try_clone_to_owned()?;
        insert(&handle, display_fd, |_, data: &mut Data| {
            if let Err(error) = data.display.dispatch_clients(&mut data.state) {
                tracing::warn!(%error, "the app's connections could not be read");
            }
        })?;
        let socket = options.dir.join("wayland-0");
        let wayland = listen(&socket)?;
        insert(&handle, wayland, |listener, data: &mut Data| {
            data.accept_app(listener);
        })?;
        // Bound and made private under another name, then named `control`: a session that sees
        // `control` appear finds it listening.
        let binding = options.dir.join(".control");
        let control = listen(&binding)?;
        std::fs::set_permissions(&binding, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&binding, options.dir.join("control"))?;
        insert(&handle, control, |listener, data: &mut Data| {
            data.accept_session(listener);
        })?;
        let x11 = if options.x11 {
            x11::XDisplay::bind(&options.dir)
                .inspect_err(|error| tracing::warn!(%error, "no X11 display: no X11 programs"))
                .ok()
        } else {
            None
        };
        let x11_missing = options.x11 && x11.is_none();
        let mut keeper = Self {
            event_loop,
            data: Data {
                state,
                display,
                handle,
                up_queue: None,
                up_token: None,
                conns: HashMap::new(),
                next_conn: 0,
                own_uid: control::own_uid(),
                options,
                socket,
                created: Instant::now(),
                app: None,
                app_token: None,
                x11,
                x11_missing,
                ended: None,
                ended_at: None,
                desktop_lost: None,
                delivered: false,
                finished: false,
                told_to_end: None,
            },
        };
        x11::watch(&mut keeper.data);
        termination::watch(&keeper.data.handle)?;
        Ok(keeper)
    }

    /// Run until the app ended and that was heard (or nobody came to hear it).
    pub fn run(mut self) -> io::Result<()> {
        while !self.data.finished {
            let timeout = self.data.wake_in();
            self.dispatch(timeout)?;
        }
        Ok(())
    }

    /// Wait up to `timeout` for something to do, and do it.
    pub fn dispatch(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.event_loop
            .dispatch(timeout, &mut self.data)
            .map_err(io::Error::other)?;
        self.data.after_dispatch();
        Ok(())
    }

    /// Show the app on the desktop at the other end of `socket`.
    pub fn attach(&mut self, socket: UnixStream) -> io::Result<()> {
        self.data.attach(socket)
    }

    /// Stop showing the app: hold its windows.
    pub fn detach(&mut self) {
        self.data.hold();
    }

    #[must_use]
    pub fn describe(&self) -> Description {
        self.data.describe()
    }

    /// The Wayland socket the app connects to.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.data.socket
    }

    /// `N` of the X11 display `:N` the app is given, when it is given one.
    #[must_use]
    pub fn x11_display(&self) -> Option<u32> {
        self.data.x11.as_ref().map(x11::XDisplay::number)
    }

    /// The app ended and that was heard, or nobody came to hear it in time.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.data.finished
    }

    /// Make the keeper send the desktop a request the protocol forbids, on the window `tag`.
    #[cfg(feature = "test-support")]
    pub fn break_protocol(&mut self, tag: &str) -> bool {
        let broke = self.data.state.break_protocol(tag);
        self.data.after_dispatch();
        broke
    }
}

/// Make the keeper's private directory at `dir` if it is not there yet, before anything of the
/// launch is written into it.
///
/// # Errors
/// The directory could not be made, or one at `dir` is not this user's own.
pub fn prepare_dir(dir: &Path) -> io::Result<()> {
    private_dir(dir)
}

/// The keeper's directory: made if missing, and refused unless only its user can enter it.
fn private_dir(dir: &Path) -> io::Result<()> {
    match std::fs::create_dir(dir) {
        Ok(()) => std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = std::fs::metadata(dir)?;
    if !metadata.is_dir() || metadata.uid() != control::own_uid() || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is not a directory only this user can enter",
                dir.display()
            ),
        ));
    }
    Ok(())
}

/// A listening socket at `path`, replacing one a dead keeper left.
fn listen(path: &Path) -> io::Result<UnixListener> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = tilo_files::native::socket::bind(path)?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// End, with the app, when the process is told to (SIGTERM, SIGHUP or SIGINT, as a login's end
/// sends them), rather than at once: a keeper killed by one would leave its X11 lock and socket
/// in `/tmp` behind. The handlers are the process's own, so only the keeper program installs
/// them, before it makes its keeper.
///
/// # Errors
/// The handlers could not be installed.
pub fn end_when_told() -> io::Result<()> {
    termination::install()
}

/// The signals that end a keeper, heard in its event loop.
mod termination {
    use std::io;
    use std::os::fd::{AsRawFd as _, OwnedFd};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI32, Ordering};

    use smithay::reexports::calloop::LoopHandle;

    use crate::Data;

    /// The write end of the pipe the handler wakes the loop through.
    static WAKE: AtomicI32 = AtomicI32::new(-1);
    /// Its read end, until a keeper's loop takes it.
    static HEARD: Mutex<Option<OwnedFd>> = Mutex::new(None);

    extern "C" fn heard(_: libc::c_int) {
        let fd = WAKE.load(Ordering::Relaxed);
        if fd >= 0 {
            // SAFETY: write(2) is async-signal-safe; the byte only wakes the loop, and a full
            // pipe already holds one.
            unsafe { libc::write(fd, [1_u8].as_ptr().cast(), 1) };
        }
    }

    pub(crate) fn install() -> io::Result<()> {
        let (read, write) = rustix::pipe::pipe_with(
            rustix::pipe::PipeFlags::CLOEXEC | rustix::pipe::PipeFlags::NONBLOCK,
        )?;
        WAKE.store(write.as_raw_fd(), Ordering::Relaxed);
        // The write end stays open for the life of the process: a signal may come until it exits.
        std::mem::forget(write);
        *HEARD.lock().map_err(|_| io::Error::other("poisoned"))? = Some(read);
        for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
            // SAFETY: the handler only loads an atomic and writes to a pipe.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = heard as *const () as libc::sighandler_t;
                action.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&raw mut action.sa_mask);
                if libc::sigaction(signal, &raw const action, std::ptr::null_mut()) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
        }
        Ok(())
    }

    /// Hear the signals in the keeper's loop, when the program installed the handlers.
    pub(crate) fn watch(handle: &LoopHandle<'static, Data>) -> io::Result<()> {
        let Some(read) = HEARD.lock().ok().and_then(|mut heard| heard.take()) else {
            return Ok(());
        };
        let read: OwnedFd = read;
        super::insert(handle, read, |read, data: &mut Data| {
            let mut drained = [0_u8; 16];
            while rustix::io::read(read, &mut drained).is_ok_and(|count| count > 0) {}
            data.terminate();
        })?;
        Ok(())
    }
}

fn insert<F: std::os::fd::AsFd + 'static>(
    handle: &LoopHandle<'static, Data>,
    source: F,
    ready: impl FnMut(&F, &mut Data) + 'static,
) -> io::Result<RegistrationToken> {
    insert_for(handle, source, Interest::READ, ready)
}

fn insert_for<F: std::os::fd::AsFd + 'static>(
    handle: &LoopHandle<'static, Data>,
    source: F,
    interest: Interest,
    mut ready: impl FnMut(&F, &mut Data) + 'static,
) -> io::Result<RegistrationToken> {
    handle
        .insert_source(
            Generic::new(source, interest, Mode::Level),
            move |_, source, data| {
                ready(source, data);
                Ok(PostAction::Continue)
            },
        )
        .map_err(|error| io::Error::other(error.error))
}

impl Data {
    fn accept_app(&mut self, listener: &UnixListener) {
        while let Some(stream) = accept(listener) {
            let server = self.x11.as_ref().and_then(x11::XDisplay::server_pid);
            let from_server = server.is_some_and(|server| {
                control::peer_pid(&stream).is_ok_and(|pid| pid == Some(server))
            });
            match self
                .display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))
            {
                Ok(client) if from_server => self.state.xdnd.server_connected(client.id()),
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "an app connection could not be taken"),
            }
        }
    }

    fn accept_session(&mut self, listener: &UnixListener) {
        while let Some(stream) = accept(listener) {
            self.admit(stream);
        }
    }

    fn admit(&mut self, stream: UnixStream) {
        match control::peer_uid(&stream) {
            Ok(uid) if peer_allowed(uid, self.own_uid) => {}
            Ok(uid) => {
                tracing::warn!(uid, "refused a control connection of another user");
                return;
            }
            Err(error) => {
                tracing::warn!(%error, "refused a control connection whose peer is unknown");
                return;
            }
        }
        let conn = Conn::new(stream);
        let id = self.next_conn;
        self.next_conn = self.next_conn.wrapping_add(1);
        let token = match conn.stream.as_fd().try_clone_to_owned().and_then(|fd| {
            insert(&self.handle, fd, move |_, data: &mut Data| {
                data.read_session(id);
            })
        }) {
            Ok(token) => token,
            Err(error) => {
                tracing::warn!(%error, "a control connection could not be watched");
                return;
            }
        };
        self.conns.insert(id, (conn, token));
    }

    fn read_session(&mut self, id: u64) {
        let Some((conn, _)) = self.conns.get_mut(&id) else {
            return;
        };
        let (frames, open) = match conn.receive() {
            Ok(received) => received,
            Err(error) => {
                tracing::warn!(%error, "a control connection broke");
                (Vec::new(), false)
            }
        };
        for frame in frames {
            self.session_message(id, &frame);
        }
        if !open {
            self.close_session(id);
        }
    }

    fn close_session(&mut self, id: u64) {
        if let Some((conn, token)) = self.conns.remove(&id) {
            self.handle.remove(token);
            if let Some(writing) = conn.writing {
                self.handle.remove(writing);
            }
        }
    }

    /// Send `message` to the session `id` without waiting: whether it reached the socket whole.
    /// What does not fit waits for room; a session gone or too far behind is closed.
    fn send<T: serde::Serialize>(&mut self, id: u64, message: &T) -> bool {
        let Some((conn, _)) = self.conns.get_mut(&id) else {
            return false;
        };
        let sent = conn.send(message);
        self.sent(id, sent)
    }

    /// Write what the session `id` has pending, once its socket has room.
    fn flush_session(&mut self, id: u64) {
        let Some((conn, _)) = self.conns.get_mut(&id) else {
            return;
        };
        let flushed = conn.flush();
        self.sent(id, flushed);
    }

    /// Act on how a write to the session `id` went: whether all of it was written.
    fn sent(&mut self, id: u64, sent: io::Result<bool>) -> bool {
        let Some((conn, _)) = self.conns.get_mut(&id) else {
            return false;
        };
        match sent {
            Ok(true) => {
                if let Some(writing) = conn.writing.take() {
                    self.handle.remove(writing);
                }
                true
            }
            Ok(false) => {
                if conn.writing.is_none() {
                    let watched = conn.stream.as_fd().try_clone_to_owned().and_then(|fd| {
                        insert_for(
                            &self.handle,
                            fd,
                            Interest::WRITE,
                            move |_, data: &mut Data| {
                                data.flush_session(id);
                            },
                        )
                    });
                    match watched {
                        Ok(token) => conn.writing = Some(token),
                        Err(error) => {
                            tracing::warn!(%error, "a control connection cannot be written later; closing it");
                            self.close_session(id);
                        }
                    }
                }
                false
            }
            Err(error) => {
                tracing::warn!(%error, "a session stopped reading the keeper; closing it");
                self.close_session(id);
                false
            }
        }
    }

    fn session_message(&mut self, id: u64, frame: &[u8]) {
        let (speaks, holds) = self.speaks();
        let Some((conn, _)) = self.conns.get_mut(&id) else {
            return;
        };
        // A session at another version was answered once: nothing more it says is read.
        if conn.holds {
            return;
        }
        if !conn.greeted {
            if wire::hello_version(frame) == Some(speaks) {
                conn.greeted = true;
                self.send(id, &FromKeeper::Hello { version: speaks });
                if let Some(end) = self.ended
                    && self.send(id, &FromKeeper::Ended { end })
                {
                    self.delivered = true;
                }
            } else {
                tracing::warn!(
                    "a session spoke another version of the control interface; holding the app for it"
                );
                self.send(id, &FromKeeper::VersionMismatch { version: speaks });
                if holds && self.send(id, &wire::HOLDING) {
                    if let Some((conn, _)) = self.conns.get_mut(&id) {
                        conn.holds = true;
                    }
                } else {
                    self.close_session(id);
                }
            }
            return;
        }
        let message: ToKeeper = match serde_json::from_slice(frame) {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(%error, "a session sent a message the keeper does not read");
                return;
            }
        };
        match message {
            ToKeeper::Hello { .. } => {}
            ToKeeper::Attach => {
                let fd = self.conns.get_mut(&id).and_then(|(conn, _)| conn.take_fd());
                let Some(fd) = fd else {
                    tracing::warn!("an attach came without its Wayland connection");
                    return;
                };
                if let Err(error) = self.attach(UnixStream::from(fd)) {
                    tracing::warn!(%error, "the desktop's connection could not be used");
                }
            }
            ToKeeper::Detach => self.hold(),
            ToKeeper::Describe => {
                let described = FromKeeper::Described(self.describe());
                self.send(id, &described);
            }
            ToKeeper::SetPlacement { tag, placement } => {
                if self.state.windows().iter().any(|window| window.tag == tag) {
                    self.state.placements.insert(tag, placement);
                    self.state.windows_changed = true;
                }
            }
            ToKeeper::End { signal } => {
                if let Some(app) = &self.app {
                    app.signal(signal);
                }
            }
        }
    }

    /// The version this keeper speaks, and whether it holds its app for a session at another.
    #[cfg(not(feature = "test-support"))]
    #[expect(clippy::unused_self, reason = "the test build reads its options")]
    const fn speaks(&self) -> (u32, bool) {
        (INTERFACE_VERSION, true)
    }

    #[cfg(feature = "test-support")]
    fn speaks(&self) -> (u32, bool) {
        match self.options.aged_by.as_ref().map(std::fs::read_to_string) {
            Some(Ok(said)) => (0, said.trim() != "closes"),
            _ => (INTERFACE_VERSION, true),
        }
    }

    /// Whether a session that cannot show the app is connected and holds it.
    fn held(&self) -> bool {
        self.conns.values().any(|(conn, _)| conn.holds)
    }

    fn describe(&self) -> Description {
        Description {
            launch: self.options.launch.clone(),
            app_pid: self.app.as_ref().map(|app| app.pid),
            started_at: self.app.as_ref().and_then(|app| app.started_at),
            windows: self.state.windows(),
            attached: self.state.up.as_ref().is_some_and(|up| up.ready),
            ended: self.ended,
            fault: self.state.fault.clone(),
        }
    }

    fn attach(&mut self, socket: UnixStream) -> io::Result<()> {
        self.hold();
        self.state.generation = self.state.generation.wrapping_add(1);
        let (up, queue) = Upstream::connect(socket, self.state.generation)?;
        let fd = up.conn.backend().poll_fd().try_clone_to_owned()?;
        self.up_token = Some(insert(&self.handle, fd, |_, data: &mut Self| {
            data.read_upstream();
        })?);
        self.state.up = Some(up);
        self.up_queue = Some(queue);
        self.desktop_lost = None;
        self.flush_upstream();
        Ok(())
    }

    fn read_upstream(&mut self) {
        let Some(queue) = self.up_queue.as_mut() else {
            return;
        };
        if let Err(error) = pump_upstream(queue, &mut self.state) {
            self.upstream_ended(&error);
        }
    }

    fn flush_upstream(&mut self) {
        let Some(up) = self.state.up.as_ref() else {
            return;
        };
        match up.conn.flush() {
            Ok(()) => {}
            Err(WaylandError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => self.upstream_ended(&error),
        }
    }

    /// The desktop's connection ended: a hangup is the desktop going away; a protocol error is
    /// the keeper's own fault, reported, and fatal to the app whose state caused it twice.
    fn upstream_ended(&mut self, error: &WaylandError) {
        let protocol = self
            .state
            .up
            .as_ref()
            .and_then(|up| up.conn.protocol_error())
            .or_else(|| match error {
                WaylandError::Protocol(error) => Some(error.clone()),
                WaylandError::Io(_) => None,
            });
        if let Some(error) = protocol {
            self.fault(&error);
        } else {
            tracing::info!(%error, "the desktop went away; holding the app's windows");
        }
        self.hold();
    }

    fn fault(&mut self, error: &wayland_client::backend::protocol::ProtocolError) {
        let fault = format!(
            "{}@{}: error {}: {}",
            error.object_interface, error.object_id, error.code, error.message
        );
        tracing::warn!(%fault, "the keeper broke the desktop's protocol");
        let owner = self
            .state
            .up
            .as_ref()
            .and_then(|up| up.owner_of(error.object_id));
        if let Some(owner) = owner
            && !self.state.faulted.insert(owner.clone())
        {
            tracing::warn!("an app's windows broke the desktop's protocol twice; closing it");
            self.state.dh.backend_handle().kill_client(
                owner.clone(),
                DisconnectReason::ProtocolError(ProtocolError {
                    code: 0,
                    object_id: 1,
                    object_interface: "wl_display".to_owned(),
                    message: format!("its state broke the desktop's protocol twice: {fault}"),
                }),
            );
            // A killed connection is closed once the display dispatches it.
            if let Err(error) = self
                .display
                .backend()
                .dispatch_single_client(&mut self.state, owner)
            {
                tracing::warn!(%error, "the closed app connection could not be cleaned up");
            }
        }
        self.state.fault = Some(fault.clone());
        self.state.outbox.push(FromKeeper::Fault { fault });
    }

    /// Stop mirroring: drop the desktop's connection and hold everything.
    fn hold(&mut self) {
        if let Some(token) = self.up_token.take() {
            self.handle.remove(token);
        }
        self.up_queue = None;
        if self.state.up.take().is_some() {
            self.desktop_lost = Some((Instant::now(), None));
            self.state.hold_all();
            self.state.hold_color();
            self.state.hold_xdnd();
            self.state.hold_data_device();
            self.state.hold_primary_selection();
            self.state.hold_pointer_constraints();
            self.state.hold_pointer_gestures();
            self.state.hold_relative_pointers();
            self.state.hold_text_input();
            self.state.seat.hold();
            self.state.tablets.hold();
            self.state.pings.clear();
            self.state.windows_changed = true;
        }
    }

    fn after_dispatch(&mut self) {
        self.flush_upstream();
        if self.app.is_none()
            && self.ended.is_none()
            && self.state.ready_once
            && !self.options.command.is_empty()
        {
            self.start_app();
        }
        if self.state.windows_changed {
            self.state.windows_changed = false;
            let windows = self.state.windows();
            self.state.outbox.push(FromKeeper::Windows { windows });
        }
        let outbox = std::mem::take(&mut self.state.outbox);
        for message in outbox {
            let ids: Vec<u64> = self
                .conns
                .iter()
                .filter(|(_, (conn, _))| conn.greeted)
                .map(|(id, _)| *id)
                .collect();
            for id in ids {
                if self.send(id, &message) && matches!(message, FromKeeper::Ended { .. }) {
                    self.delivered = true;
                }
            }
        }
        if let Err(error) = self.display.flush_clients() {
            tracing::warn!(%error, "the app's connections could not be written");
        }
        self.end_without_desktop();
        if self.told_to_end.is_some() && (self.app.is_none() || self.ended.is_some()) {
            self.finished = true;
            return;
        }
        let grace = self.options.grace;
        match self.ended_at {
            Some(at) => self.finished = self.delivered || at.elapsed() >= grace,
            None => {
                if self.app.is_none()
                    && !self.options.command.is_empty()
                    && self.created.elapsed() >= grace
                {
                    tracing::warn!("no desktop attached in time; the app was not started");
                    self.finished = true;
                }
            }
        }
    }

    /// The keeper was told to end: its app is asked to as well, and the keeper ends with it.
    fn terminate(&mut self) {
        if self.told_to_end.is_some() {
            return;
        }
        tracing::info!("the keeper was told to end; its app ends too");
        self.told_to_end = Some(Instant::now());
        if let (Some(app), None) = (&self.app, self.ended) {
            app.signal(libc::SIGTERM);
        }
    }

    /// A running app whose desktop stayed away for [`Options::desktop_gone`] is asked to end
    /// (SIGTERM to its process group), and made to after [`Options::grace`] more. A session that
    /// cannot show it holds it: the time counts from when the last such session went.
    fn end_without_desktop(&mut self) {
        if self.held() {
            if let Some((lost, None)) = &mut self.desktop_lost {
                *lost = Instant::now();
            }
            return;
        }
        let (Some(app), None, Some((lost, asked))) =
            (&self.app, self.ended, &mut self.desktop_lost)
        else {
            return;
        };
        match asked {
            None if lost.elapsed() >= self.options.desktop_gone => {
                tracing::warn!(
                    "no desktop came back for the app; the login ended, so the app ends too"
                );
                app.signal(libc::SIGTERM);
                *asked = Some(Instant::now());
            }
            Some(at) if at.elapsed() >= self.options.grace => {
                app.signal(libc::SIGKILL);
                *asked = Some(Instant::now());
            }
            _ => {}
        }
    }

    fn wake_in(&self) -> Option<Duration> {
        let gone = match (&self.app, self.ended, self.desktop_lost) {
            (Some(_), None, Some((_, None))) if self.held() => None,
            (Some(_), None, Some((lost, None))) => {
                Some(self.options.desktop_gone.saturating_sub(lost.elapsed()))
            }
            (Some(_), None, Some((_, Some(at)))) => {
                Some(self.options.grace.saturating_sub(at.elapsed()))
            }
            _ => None,
        };
        let grace = self.options.grace;
        let ending = match self.ended_at {
            Some(at) => Some(grace.saturating_sub(at.elapsed())),
            None if self.app.is_none() && !self.options.command.is_empty() => {
                Some(grace.saturating_sub(self.created.elapsed()))
            }
            None => None,
        };
        match (gone, ending) {
            (Some(gone), Some(ending)) => Some(gone.min(ending)),
            (gone, ending) => gone.or(ending),
        }
    }

    fn start_app(&mut self) {
        let x11 = match (&self.x11, self.x11_missing) {
            (Some(display), _) => {
                child::X11::Display(display.number(), display.authority().to_path_buf())
            }
            (None, true) => child::X11::Missing,
            (None, false) => child::X11::Unchanged,
        };
        let app =
            match child::App::spawn(&self.options.command, &self.socket, x11, &self.options.dir) {
                Ok(app) => app,
                Err(error) => {
                    tracing::warn!(%error, "the app could not be started");
                    self.app_ended(End::Exited { code: 127 });
                    return;
                }
            };
        match app
            .pidfd
            .try_clone()
            .and_then(|fd| insert(&self.handle, fd, |_, data: &mut Self| data.check_app()))
        {
            Ok(token) => self.app_token = Some(token),
            Err(error) => tracing::warn!(%error, "the app's end cannot be watched"),
        }
        if let Some(started_at) = app.started_at
            && let Err(error) = write_app(&self.options.dir, app.pid, started_at)
        {
            tracing::warn!(%error, "the app's process could not be recorded");
        }
        self.app = Some(app);
    }

    fn check_app(&mut self) {
        let Some(end) = self.app.as_mut().and_then(child::App::try_end) else {
            return;
        };
        if let Some(token) = self.app_token.take() {
            self.handle.remove(token);
        }
        self.app_ended(end);
    }

    /// The app ended: recorded in the directory at once, so a session that comes later still
    /// hears how, and told to every session connected now.
    fn app_ended(&mut self, end: End) {
        tracing::info!(?end, "the app ended");
        self.ended = Some(end);
        self.ended_at = Some(Instant::now());
        if let Err(error) = write_ended(&self.options.dir, end) {
            tracing::warn!(%error, "how the app ended could not be recorded");
        }
        self.state.outbox.push(FromKeeper::Ended { end });
    }
}

/// Write the app's process to `<dir>/app` ([`wire::AppProcess`]), whole or not at all.
fn write_app(dir: &Path, pid: u32, started_at: u64) -> io::Result<()> {
    tilo_files::native::atomic::write(
        &dir.join("app"),
        &serde_json::to_vec(&wire::AppProcess { pid, started_at })?,
    )
}

fn write_ended(dir: &Path, end: End) -> io::Result<()> {
    tilo_files::native::atomic::write(&dir.join("ended"), &serde_json::to_vec(&end)?)
}

/// One pending connection, without waiting.
fn accept(listener: &UnixListener) -> Option<UnixStream> {
    match listener.accept() {
        Ok((stream, _)) => Some(stream),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => None,
        Err(error) => {
            tracing::warn!(%error, "a connection could not be accepted");
            None
        }
    }
}

/// Read the desktop's events and dispatch them.
fn pump_upstream(queue: &mut EventQueue<State>, state: &mut State) -> Result<(), WaylandError> {
    dispatch(queue, state)?;
    let Some(conn) = state.up.as_ref().map(|up| up.conn.clone()) else {
        return Ok(());
    };
    if let Some(guard) = conn.prepare_read() {
        match guard.read() {
            Ok(_) => {}
            Err(WaylandError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
    }
    dispatch(queue, state)
}

fn dispatch(queue: &mut EventQueue<State>, state: &mut State) -> Result<(), WaylandError> {
    match queue.dispatch_pending(state) {
        Ok(_) => Ok(()),
        Err(DispatchError::Backend(error)) => Err(error),
        Err(DispatchError::BadMessage {
            sender_id,
            interface,
            opcode,
        }) => Err(WaylandError::Protocol(
            wayland_client::backend::protocol::ProtocolError {
                code: 0,
                object_id: sender_id.protocol_id(),
                object_interface: interface.to_owned(),
                message: format!(
                    "the desktop sent a message the keeper cannot read (opcode {opcode})"
                ),
            },
        )),
    }
}
