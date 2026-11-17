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

mod child;
mod control;
mod data_device;
mod dmabuf;
mod fractional;
mod outputs;
mod pings;
mod pointer_constraints;
mod pointer_gestures;
mod primary_selection;
mod relative_pointer;
mod seat;
mod state;
mod surfaces;
mod text_input;
mod upstream;
mod viewporter;
pub mod wire;

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
    /// [`GRACE`], shorter in a test.
    pub grace: Duration,
}

impl Options {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>, launch: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            launch: launch.into(),
            app_id_prefix: None,
            command: Vec::new(),
            grace: GRACE,
        }
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
    ended: Option<End>,
    ended_at: Option<Instant>,
    delivered: bool,
    finished: bool,
}

impl Keeper {
    /// Bind the keeper's sockets in `options.dir`. The app starts once the first desktop is
    /// attached, so it never sees a display without outputs or a seat.
    pub fn new(options: Options) -> io::Result<Self> {
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
        let control = listen(&options.dir.join("control"))?;
        std::fs::set_permissions(
            options.dir.join("control"),
            std::fs::Permissions::from_mode(0o600),
        )?;
        insert(&handle, control, |listener, data: &mut Data| {
            data.accept_session(listener);
        })?;
        Ok(Self {
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
                ended: None,
                ended_at: None,
                delivered: false,
                finished: false,
            },
        })
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

fn insert<F: std::os::fd::AsFd + 'static>(
    handle: &LoopHandle<'static, Data>,
    source: F,
    mut ready: impl FnMut(&F, &mut Data) + 'static,
) -> io::Result<RegistrationToken> {
    handle
        .insert_source(
            Generic::new(source, Interest::READ, Mode::Level),
            move |_, source, data| {
                ready(source, data);
                Ok(PostAction::Continue)
            },
        )
        .map_err(|error| io::Error::other(error.error))
}

impl Data {
    fn accept_app(&self, listener: &UnixListener) {
        while let Some(stream) = accept(listener) {
            if let Err(error) = self
                .display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))
            {
                tracing::warn!(%error, "an app connection could not be taken");
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
        let conn = match Conn::new(stream) {
            Ok(conn) => conn,
            Err(error) => {
                tracing::warn!(%error, "a control connection could not be set up");
                return;
            }
        };
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
        if let Some((_, token)) = self.conns.remove(&id) {
            self.handle.remove(token);
        }
    }

    fn send(&mut self, id: u64, message: &FromKeeper) -> bool {
        let Some((conn, _)) = self.conns.get_mut(&id) else {
            return false;
        };
        match conn.send(message) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "a session stopped reading the keeper; closing it");
                self.close_session(id);
                false
            }
        }
    }

    fn session_message(&mut self, id: u64, frame: &[u8]) {
        let Some((conn, _)) = self.conns.get_mut(&id) else {
            return;
        };
        if !conn.greeted {
            if wire::hello_version(frame) == Some(INTERFACE_VERSION) {
                conn.greeted = true;
                self.send(
                    id,
                    &FromKeeper::Hello {
                        version: INTERFACE_VERSION,
                    },
                );
                if let Some(end) = self.ended
                    && self.send(id, &FromKeeper::Ended { end })
                {
                    self.delivered = true;
                }
            } else {
                tracing::warn!("a session spoke another version of the control interface");
                self.send(
                    id,
                    &FromKeeper::VersionMismatch {
                        version: INTERFACE_VERSION,
                    },
                );
                self.close_session(id);
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
            self.state.hold_all();
            self.state.hold_data_device();
            self.state.hold_primary_selection();
            self.state.hold_pointer_constraints();
            self.state.hold_pointer_gestures();
            self.state.hold_relative_pointers();
            self.state.hold_text_input();
            self.state.seat.hold();
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

    fn wake_in(&self) -> Option<Duration> {
        let grace = self.options.grace;
        match self.ended_at {
            Some(at) => Some(grace.saturating_sub(at.elapsed())),
            None if self.app.is_none() && !self.options.command.is_empty() => {
                Some(grace.saturating_sub(self.created.elapsed()))
            }
            None => None,
        }
    }

    fn start_app(&mut self) {
        let app = match child::App::spawn(&self.options.command, &self.socket) {
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

fn write_ended(dir: &Path, end: End) -> io::Result<()> {
    let partial = dir.join(".ended.partial");
    std::fs::write(&partial, serde_json::to_vec(&end)?)?;
    std::fs::rename(partial, dir.join("ended"))
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
