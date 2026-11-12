//! Starting the desktop's surface apps as processes of their own.
//! The bar, the desk, the panels and the sheets are ordinary apps, and the session runs each in a
//! process of its own: it hands the app one end of a socket it trusts
//! ([`Compositor::connect_trusted_client`]) and names the layer surface its window is to become in
//! its environment. What the app draws reaches the compositor as a client's buffer, shown and given
//! input like any other client's; nothing of the app runs in the session.
//! A surface is still an app that does things, so it is also handed one end of an actions socket:
//! on it the session answers the actions it publishes ([`ActionDoor`]) and asks the surface to run
//! one of its own, both as [`RemoteActionMessage`]s of the protocol every app speaks to its host.
//! Those are the only messages: what a surface shows it reads from the resources it holds, and what
//! was done is said in actions.
//!
//! [`Compositor::connect_trusted_client`]: tilo_compositor::Compositor::connect_trusted_client

use std::collections::{HashMap, VecDeque};
use std::os::fd::AsRawFd as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Instant;

use tilo_actions::InvocationUpdate;
use tilo_actions::outcome::InvocationId;
use tilo_actions::remote::{
    ActionHostError, CataloguePage, CatalogueQuery, CatalogueRow, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame,
    RemoteActionMessage, RemoteActionTarget,
};
use tilo_compositor::Compositor;
use ui::collaboration::ExportedAction;
use ui::host_operation::ResourceOperationStart;
use ui::surface_peer::FRAMING;
use ui::surface_services::{ACTIONS_FD_ENV, APP_ID_ENV, SOCKET_ENV, WORKSPACE_ENV};

/// Where on an output a surface app's window stands, lowest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    /// Below the desk's own content: a wallpaper or a stand-in background.
    Background,
    /// Below every window, above the background.
    Bottom,
    /// Above the windows, below the overlays.
    Top,
    /// Above everything, including a fullscreen window.
    Overlay,
}

impl Layer {
    const fn name(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Bottom => "bottom",
            Self::Top => "top",
            Self::Overlay => "overlay",
        }
    }
}

/// Which edges of the output a surface app's window is pinned to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Anchor(u8);

impl Anchor {
    /// The top edge.
    pub const TOP: Self = Self(1);
    /// The bottom edge.
    pub const BOTTOM: Self = Self(1 << 1);
    /// The left edge.
    pub const LEFT: Self = Self(1 << 2);
    /// The right edge.
    pub const RIGHT: Self = Self(1 << 3);

    /// Whether none of the edges is pinned: the surface says its own size and place itself.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The edges, as the app reads them.
    #[must_use]
    pub fn names(self) -> String {
        const ALL: [(Anchor, &str); 4] = [
            (Anchor::TOP, "top"),
            (Anchor::BOTTOM, "bottom"),
            (Anchor::LEFT, "left"),
            (Anchor::RIGHT, "right"),
        ];
        let names: Vec<&str> = ALL
            .into_iter()
            .filter(|(edge, _)| self.0 & edge.0 != 0)
            .map(|(_, name)| name)
            .collect();
        if names.is_empty() {
            String::from("none")
        } else {
            names.join(",")
        }
    }
}

impl std::ops::BitOr for Anchor {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Whether a surface app's window takes the keyboard, and how.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Keyboard {
    /// Never: the window takes the pointer only (the bar, the desk).
    #[default]
    None,
    /// When the person clicks it (a panel with a field in it).
    OnDemand,
    /// Always: the surface is the session's keyboard focus (the lock screen).
    Exclusive,
}

impl Keyboard {
    const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::OnDemand => "on-demand",
            Self::Exclusive => "exclusive",
        }
    }
}

/// One surface app, and the layer surface the session makes its window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Surface {
    /// What the session calls it: its name in logs and its key when it stops.
    pub name: String,
    /// The app it is started as: what the broker admits its connection to the session's services
    /// by, and what its own actions are published under.
    pub app_id: String,
    /// The program to start.
    pub program: PathBuf,
    /// Where its window stands on the output.
    pub layer: Layer,
    /// The kind of surface it is, as the compositor and a person's tools name it (`tilo-bar`,
    /// `tilo-desk` , …).
    pub namespace: String,
    /// The edges it is pinned to.
    pub anchor: Anchor,
    /// Its size in logical pixels; a side pinned to two edges takes the room between them, so 0
    /// means "all of it".
    pub size: (u32, u32),
    /// The margin from each edge it is pinned to, in logical pixels: top, right, bottom, left.
    pub margin: (i32, i32, i32, i32),
    /// The band along the output edge the desktop leaves free for it;
    /// 0 reserves nothing.
    pub exclusive: i32,
    /// How it takes the keyboard.
    pub keyboard: Keyboard,
    /// Whether its window becomes the session's lock screen instead of a layer surface.
    pub lock: bool,
    /// The output it stands on when its window names none of its own: the display its client is
    /// connected for, and any output otherwise.
    pub output: Option<String>,
}

impl Surface {
    /// A surface app named `name` and started from `program` .
    #[must_use]
    pub fn new(name: impl Into<String>, program: impl Into<PathBuf>) -> Self {
        let name = name.into();
        Self {
            app_id: format!("{}.{name}", tilo_desktop_model::APP_ID_PREFIX),
            program: program.into(),
            namespace: namespace(&name),
            name,
            layer: Layer::Top,
            anchor: Anchor::default(),
            size: (0, 0),
            margin: (0, 0, 0, 0),
            exclusive: 0,
            keyboard: Keyboard::None,
            lock: false,
            output: None,
        }
    }

    /// The surface app `name` installed beside the session (or named by
    /// `TILO_SURFACE_<NAME>`, or found on `PATH`).
    ///
    /// # Errors
    /// No program of that name was found.
    pub fn locate(name: &str) -> Result<Self, StartError> {
        Ok(Self::new(name, program_path(name)?))
    }

    /// Pin the window to `edges` .
    #[must_use]
    pub fn anchor(mut self, edges: Anchor) -> Self {
        self.anchor = edges;
        self
    }

    /// Stand on `layer` .
    #[must_use]
    pub fn layer(mut self, layer: Layer) -> Self {
        self.layer = layer;
        self
    }

    /// Take `size` logical pixels; 0 along an axis fits its anchors.
    #[must_use]
    pub fn size(mut self, size: (u32, u32)) -> Self {
        self.size = size;
        self
    }

    /// Stand `margin` (top, right, bottom, left) logical pixels from the edges it is pinned to.
    #[must_use]
    pub fn margin(mut self, margin: (i32, i32, i32, i32)) -> Self {
        self.margin = margin;
        self
    }

    /// Reserve a band of `pixels` along the pinned edge.
    #[must_use]
    pub fn exclusive(mut self, pixels: i32) -> Self {
        self.exclusive = pixels;
        self
    }

    /// Name the kind of surface, for the compositor and a person's tools.
    #[must_use]
    pub fn namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    /// Start it as the app `app_id` instead of [`Self::new`]'s first-party id for its name.
    #[must_use]
    pub fn app_id(mut self, app_id: impl Into<String>) -> Self {
        self.app_id = app_id.into();
        self
    }

    /// Let the window take the keyboard `how` .
    #[must_use]
    pub fn keyboard(mut self, how: Keyboard) -> Self {
        self.keyboard = how;
        self
    }

    /// Make the window the session's lock screen.
    #[must_use]
    pub fn lock(mut self) -> Self {
        self.lock = true;
        self
    }

    /// Stand on the display named `output` , for a window that names no output of its own: the
    /// person's keyboard goes on the display they type on.
    #[must_use]
    pub fn on(mut self, output: impl Into<String>) -> Self {
        self.output = Some(output.into());
        self
    }

    /// What the app reads of the surface it is to become and of the desktop it runs on, beside the
    /// `WAYLAND_SOCKET` it is handed.
    #[must_use]
    pub fn environment(&self) -> Vec<(String, String)> {
        let mut environment = vec![
            (
                String::from("TILO_SURFACE_LAYER"),
                self.layer.name().to_owned(),
            ),
            (
                String::from("TILO_SURFACE_NAMESPACE"),
                self.namespace.clone(),
            ),
            (String::from("TILO_SURFACE_ANCHOR"), self.anchor.names()),
            (
                String::from("TILO_SURFACE_SIZE"),
                format!("{}x{}", self.size.0, self.size.1),
            ),
            (
                String::from("TILO_SURFACE_MARGIN"),
                format!(
                    "{},{},{},{}",
                    self.margin.0, self.margin.1, self.margin.2, self.margin.3
                ),
            ),
            (
                String::from("TILO_SURFACE_EXCLUSIVE"),
                self.exclusive.to_string(),
            ),
            (
                String::from("TILO_SURFACE_KEYBOARD"),
                self.keyboard.name().to_owned(),
            ),
        ];
        if self.lock {
            environment.push((String::from("TILO_SURFACE_LOCK"), String::from("1")));
        }
        // A surface app is Tilo's own app, whatever desktop this session was started inside: its
        // appearance and its menu entries come from the session's settings, never from a host
        // desktop around it.
        environment.extend(
            tilo_app_launch::env::desktop_variables()
                .map(|(name, value)| (name.to_owned(), value.to_owned())),
        );
        environment
    }
}

/// The descriptor an app's end of the trusted socket is handed as.
const SOCKET_DESCRIPTOR: i32 = 3;

/// The descriptor an app's end of the actions socket is handed as: the number [`ACTIONS_FD_ENV`]
/// names in the app's environment.
const ACTIONS_DESCRIPTOR: i32 = 4;

/// How many invocations' outcomes one surface's connection remembers, so an answer asked after it
/// ended is still given.
const ANSWERED_KEPT: usize = 256;

/// How often a surface that has listed nothing yet is asked again.
const LISTING_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// The provider surface apps address the desktop's actions under.
pub const SESSION_PROVIDER: &str = tilo_runtime::session_door::SESSION_PROVIDER;

/// What the session answers its surface apps' actions from: the published side every caller of the
/// desktop reaches, so a surface invokes what a person's palette or an agent invokes.
pub trait ActionDoor {
    /// What the desktop publishes now: its resources, each with its actions.
    fn resources(&mut self) -> Vec<tilo_actions::ResourceSpec>;

    /// Run the published action `address` with `arguments`.
    ///
    /// # Errors
    /// Nothing publishes it, or the surface publishing it refused.
    fn invoke(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, tilo_actions::DoorError>;
}

/// Why a surface app did not start.
#[derive(Debug)]
pub enum StartError {
    /// No program by that name is installed beside the session or on `PATH` .
    NotFound {
        /// The name asked for.
        name: String,
    },
    /// The session could not make the socket it trusts the app over.
    Socket(tilo_compositor::Error),
    /// The process could not be started.
    Spawn {
        /// The program that failed to start.
        program: PathBuf,
        /// The system's refusal.
        source: std::io::Error,
    },
    /// The gate that holds the app until the session recorded its launch could not be made: without
    /// it the app could reach the session ahead of its record, so it is not started at all.
    Gate(std::io::Error),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { name } => write!(formatter, "no surface app named {name} installed"),
            Self::Socket(error) => write!(formatter, "the trusted socket: {error}"),
            Self::Spawn { program, source } => {
                write!(formatter, "{} did not start: {source}", program.display())
            }
            Self::Gate(error) => write!(
                formatter,
                "the gate holding the app until its launch is recorded: {error}"
            ),
        }
    }
}

impl std::error::Error for StartError {}

/// The name a surface app of `name` asks the compositor for, which is what the session shows and
/// hides it by.
#[must_use]
pub fn namespace(name: &str) -> String {
    format!("tilo-{name}")
}

/// The program a surface app of `name` runs: the one
/// `TILO_SURFACE_<NAME>` names, else the one in `TILO_SURFACE_PROGRAMS`,
/// else the one beside the session, else the one on `PATH`.
///
/// # Errors
/// None of them is there.
pub fn program_path(name: &str) -> Result<PathBuf, StartError> {
    let variable = format!(
        "TILO_SURFACE_{}",
        name.to_ascii_uppercase().replace('-', "_")
    );
    if let Some(program) = std::env::var_os(&variable) {
        return Ok(PathBuf::from(program));
    }
    let mut directories: Vec<PathBuf> = Vec::new();
    if let Some(directory) = std::env::var_os("TILO_SURFACE_PROGRAMS") {
        directories.push(PathBuf::from(directory));
    }
    if let Some(beside) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        directories.push(beside);
    }
    directories.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    for directory in directories {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(StartError::NotFound {
        name: name.to_owned(),
    })
}

/// A surface app the session started.
struct Running {
    name: String,
    child: Child,
    started: Instant,
    /// The actions socket to it: what it asks, what it reports, and the invocations the session
    /// asked it to run.
    actions: Connection,
    /// The display of the app's own, while it listens: what the app reaches the compositor on
    /// beside its window's connection. It listens until the app is gone, and nothing else reads it.
    _display: Option<tilo_compositor::SurfaceDisplay>,
}

/// The session's end of a surface app's actions socket.
struct Connection {
    outgoing: mpsc::Sender<RemoteActionMessage>,
    incoming: mpsc::Receiver<RemoteActionMessage>,
    /// How each invocation the session asked the surface to run stands.
    asked: HashMap<u64, InvocationState>,
    /// The invocation id the app named each ask with, back to the session's id for it: an answer
    /// names the invocation, never the ask.
    by_invocation: HashMap<InvocationId, u64>,
    /// The provider the surface answers its actions under.
    provider: String,
    /// The correlation the next catalogue request is sent under.
    listing: u64,
    /// The correlation of the request whose page is due, if one is: a page that answers any other
    /// request is a listing the session has given up on, and reading it into this one would repeat
    /// rows.
    awaited: Option<u64>,
    /// When the last catalogue request went out, so a surface that has published nothing yet is
    /// asked again rather than never.
    listed_at: Option<Instant>,
    /// What the surface offers: the actions its catalogue lists.
    published: Vec<ExportedAction>,
    /// The rows of the listing being read, page by page, before they are what the session answers
    /// from. A listing is paged: a surface with more rows than fit says so, and the pages are
    /// followed until the last is in.
    reading: Vec<CatalogueRow>,
    /// How the last invocations its app run ended, newest last, so one the session answered as
    /// accepted can be asked after.
    answered: VecDeque<(InvocationId, InvocationUpdate)>,
}

impl Connection {
    /// The socket pair `stream` is one end of, with a reader and a writer thread: neither side's
    /// news waits on the other's writing. The reader wakes the loop as each message lands.
    fn of(stream: UnixStream, wake: Option<tilo_service::Wake>) -> Self {
        let (outgoing, writes) = mpsc::channel();
        let (incoming_sender, incoming) = mpsc::channel();
        match stream.try_clone() {
            Ok(reader) => {
                if let Err(error) = std::thread::Builder::new()
                    .name("surface-actions".into())
                    .spawn(move || read_messages(reader, &incoming_sender, wake.as_ref()))
                {
                    tracing::warn!(%error, "a surface's news is not read");
                }
                if let Err(error) = std::thread::Builder::new()
                    .name("surface-actions-writes".into())
                    .spawn(move || write_messages(stream, &writes))
                {
                    tracing::warn!(%error, "a surface's news is not sent");
                }
            }
            Err(error) => {
                tracing::warn!(%error, "a surface's connection could not be split; nothing is sent to or heard from it");
            }
        }
        Self {
            outgoing,
            incoming,
            asked: HashMap::new(),
            by_invocation: HashMap::new(),
            provider: String::new(),
            listing: 0,
            awaited: None,
            listed_at: None,
            published: Vec::new(),
            reading: Vec::new(),
            answered: VecDeque::new(),
        }
    }

    /// What the surface offers now, as it last listed it: the rows it offers, and the provider it
    /// offers them under.
    /// A page the surface has more after is followed by the next one; the copy the session answers
    /// from moves once the last page is in.
    /// Only the page the session is waiting for continues the listing: a listing the session gave
    /// up on is asked for again, and its pages arrive beside the new one's. Reading those would
    /// repeat rows, and a listing that never runs out.
    fn listed(&mut self, correlation: u64, page: CataloguePage) {
        if self.awaited != Some(correlation) {
            return;
        }
        self.awaited = None;
        self.read_page(page);
    }

    /// The page the session is waiting for, into the listing being read.
    fn read_page(&mut self, page: CataloguePage) {
        // A cursor from another revision is the surface saying to start over: these rows do not
        // continue what was read before them.
        if page.stale_cursor {
            self.list();
            return;
        }
        if let Some(row) = page.rows.first() {
            self.provider.clone_from(&row.provider);
        }
        let more = !page.exhausted && !page.next_cursor.is_empty();
        self.reading.extend(page.rows);
        if more {
            let correlation = self.next_listing();
            self.send(RemoteActionMessage::CatalogueRequest {
                correlation,
                query: CatalogueQuery {
                    revision: Some(page.revision),
                    cursor: page.next_cursor,
                    ..CatalogueQuery::default()
                },
            });
            return;
        }
        self.published = std::mem::take(&mut self.reading)
            .iter()
            .filter_map(|row| row.action.as_ref())
            .map(ExportedAction::from)
            .collect();
    }

    /// What the surface offers changed, so its listing is read from the start again: whatever the
    /// session was part way through is superseded.
    fn relisted(&mut self, page: CataloguePage) {
        self.reading.clear();
        self.awaited = None;
        self.read_page(page);
    }

    /// Ask the surface what it offers now, from the start.
    fn list(&mut self) {
        self.reading.clear();
        let correlation = self.next_listing();
        self.send(RemoteActionMessage::CatalogueRequest {
            correlation,
            query: CatalogueQuery::default(),
        });
    }

    /// The correlation the next catalogue request is sent under, and when it went out: its page is
    /// the one the listing waits for.
    fn next_listing(&mut self) -> u64 {
        let correlation = self.listing;
        self.listing = self.listing.wrapping_add(1);
        self.awaited = Some(correlation);
        self.listed_at = Some(Instant::now());
        correlation
    }

    /// When this surface is asked to list itself again, while it has published nothing yet or its
    /// listing is part way through.
    fn next_deadline(&self) -> Option<Instant> {
        if !self.published.is_empty() && self.reading.is_empty() {
            return None;
        }
        Some(
            self.listed_at
                .and_then(|asked| asked.checked_add(LISTING_INTERVAL))
                .unwrap_or_else(Instant::now),
        )
    }

    /// Ask again when the surface has listed nothing yet and the last ask is old enough: an app
    /// that mounts late is still heard.
    fn list_if_silent(&mut self) {
        // A whole listing with nothing being read is what the surface offers, and it asks again
        // only when it says its offer moved. One part way through whose next page has not come for
        // a listing interval is asked for again from the start, and the pages of the one given up
        // on are then ignored.
        if !self.published.is_empty() && self.reading.is_empty() {
            return;
        }
        let due = self
            .listed_at
            .is_none_or(|asked| asked.elapsed() >= LISTING_INTERVAL);
        if due {
            self.list();
        }
    }

    /// Record that the invocation the app named has `update` as its state.
    fn settle(&mut self, invocation: InvocationId, update: &InvocationUpdate) {
        let Some(id) = self.by_invocation.get(&invocation).copied() else {
            return;
        };
        if let Some(state) = self.asked.get_mut(&id) {
            state.update = update.clone();
        }
        if update.is_terminal() {
            self.by_invocation.remove(&invocation);
        }
    }

    fn send(&self, message: RemoteActionMessage) {
        if self.outgoing.send(message).is_err() {
            // The app went away: its exit is reported by the process side.
            tracing::debug!("a surface app's actions socket is closed");
        }
    }
}

/// Read messages from the surface off `stream` until it closes, waking the session's loop for each.
fn read_messages(
    mut stream: UnixStream,
    incoming: &mpsc::Sender<RemoteActionMessage>,
    wake: Option<&tilo_service::Wake>,
) {
    loop {
        let Some(body) = FRAMING.read(&mut stream).ok().flatten() else {
            return;
        };
        let Ok(frame) = RemoteActionFrame::decode(&body) else {
            tracing::warn!("a surface app sent a message that is not one");
            return;
        };
        if incoming.send(frame.message).is_err() {
            return;
        }
        if let Some(wake) = wake {
            wake();
        }
    }
}

/// Write messages for the surface to `stream` until the session closes it.
fn write_messages(mut stream: UnixStream, outgoing: &mpsc::Receiver<RemoteActionMessage>) {
    while let Ok(message) = outgoing.recv() {
        let Ok(body) = RemoteActionFrame::new(message).encode() else {
            tracing::warn!("a message to a surface app did not encode");
            return;
        };
        if FRAMING.write(&mut stream, &body).is_err() {
            return;
        }
    }
}

/// The surface apps the session runs, each in a process of its own.
#[derive(Default)]
pub struct Surfaces {
    running: Vec<Running>,
    /// The id the next invocation asked of a surface app is named by.
    asked: u64,
    /// The session's launch records, once it has lent them: each surface it starts is written into
    /// them, so its connection to the session's services is admitted for the launch that made it.
    /// `None` before the session lends them, and in a session that serves no services.
    launches: Option<std::sync::Arc<crate::Launches>>,
    /// The socket the session serves its services on, once it has lent it: what a surface app
    /// connects to read and change what the session publishes, as any app of the session does.
    services: Option<PathBuf>,
    /// What wakes the session's loop when a surface app says something, so an app's answer is taken
    /// the instant it arrives.
    wake: Option<tilo_service::Wake>,
}

impl Surfaces {
    /// The session's launch records and the socket it serves its services on, which every surface
    /// app started from here on is told of. Without a socket, a surface reads nothing of the
    /// session: it is a window only. `wake` ends the loop's wait when a surface says something.
    pub fn lend(
        &mut self,
        launches: std::sync::Arc<crate::Launches>,
        services: Option<PathBuf>,
        wake: tilo_service::Wake,
    ) {
        self.launches = Some(launches);
        self.services = services;
        self.wake = Some(wake);
    }

    /// Start `surface` as a trusted client of `compositor`, on the layer
    /// surface it names, as the app it runs for in `workspace`.
    ///
    /// # Errors
    /// The socket or the process could not be made.
    pub fn start(
        &mut self,
        compositor: &mut Compositor,
        surface: &Surface,
        workspace: &str,
    ) -> Result<(), StartError> {
        let socket = match &surface.output {
            Some(output) => compositor.connect_trusted_client_on(output),
            None => compositor.connect_trusted_client(),
        }
        .map_err(StartError::Socket)?;
        let descriptor = socket.as_raw_fd();
        let (ours, theirs) = UnixStream::pair().map_err(|source| StartError::Spawn {
            program: surface.program.clone(),
            source,
        })?;
        let actions = theirs.as_raw_fd();
        // A display of the app's own, beside the connection its window is drawn on: what speaks the
        // compositor's input protocols — the on-screen keyboard's input method — reaches it there,
        // as the app the session started.
        let display = match compositor.bind_surface_display() {
            Ok(display) => Some(display),
            Err(error) => {
                tracing::warn!(
                    surface = %surface.name,
                    %error,
                    "a surface app has no display of its own: it cannot speak the input protocols"
                );
                None
            }
        };
        let mut command = Command::new(&surface.program);
        command
            .envs(surface.environment())
            // What the app is of this session: the launch the session records for the process it
            // starts here, and the connection to the session's services it makes for itself, as any
            // app of the session does.
            .env(APP_ID_ENV, &surface.app_id)
            .env(WORKSPACE_ENV, workspace);
        if let Some(display) = &display {
            command.env("WAYLAND_DISPLAY", display.path());
        }
        if let Some(services) = &self.services {
            command.env(SOCKET_ENV, services);
        }
        command
            // The app's end of the socket becomes a fixed descriptor in its process; the compositor
            // answers on the other end here.
            .env("WAYLAND_SOCKET", SOCKET_DESCRIPTOR.to_string())
            // The other end of the actions socket, beside it.
            .env(ACTIONS_FD_ENV, ACTIONS_DESCRIPTOR.to_string())
            // A surface app shows on this session's displays only: an X display of the desktop
            // around a nested session is not one.
            .env_remove("DISPLAY")
            .stdin(Stdio::null());
        // The launch is recorded once the process exists, and the app must not reach the session
        // before it is: this holds it at its stdin until then, as the launcher is held for an app
        // the session opens.
        let registration =
            launcher::registration_gate::install(&mut command).map_err(StartError::Gate)?;
        // Safety: this runs in the child between `fork` and `exec` , where the app has done nothing
        // yet; it touches no memory the parent shares with other threads. The actions descriptor is
        // moved out of the range the fixed ones take before either is placed, so handing over one
        // cannot overwrite the other.
        unsafe {
            command.pre_exec(move || {
                let actions = libc::fcntl(actions, libc::F_DUPFD_CLOEXEC, 10);
                if actions < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::dup2(descriptor, SOCKET_DESCRIPTOR) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::dup2(actions, ACTIONS_DESCRIPTOR) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(actions);
                Ok(())
            });
        }
        let child = command.spawn().map_err(|source| StartError::Spawn {
            program: surface.program.clone(),
            source,
        })?;
        // The app is known now: what connects to its display from here on is the app and what it
        // runs, and is trusted as the surface it is.
        let display = display.and_then(|display| {
            compositor
                .accept_surface_clients(display, child.id(), surface.output.clone())
                .map_err(|error| {
                    tracing::warn!(
                        surface = %surface.name,
                        %error,
                        "a surface app's own display did not listen"
                    );
                })
                .ok()
        });
        // The process that will connect the session's services is the one started here: its launch
        // is written before the app has anything to say, as it is for any app the session starts.
        if let Some(launches) = &self.launches {
            // A surface app is the session's own program, run in its own place: no package sandbox
            // has the documents store bound in.
            launches.started(child.id(), &surface.app_id, workspace, false);
        }
        // The record exists: the app may reach what it published.
        if let Err(error) = registration.release() {
            tracing::warn!(
                surface = %surface.name,
                %error,
                "a surface app was not released to its session"
            );
        }
        // The child has its own copies: these would only keep the connections' other ends open
        // after the app is gone.
        drop(socket);
        drop(theirs);
        tracing::info!(
            surface = %surface.name,
            program = %surface.program.display(),
            "started a surface app"
        );
        let mut actions = Connection::of(ours, self.wake.clone());
        // What the surface offers the desktop is what it lists when asked,
        // as any host asks a directory it serves.
        actions.list();
        self.running.push(Running {
            name: surface.name.clone(),
            child,
            started: Instant::now(),
            actions,
            _display: display,
        });
        Ok(())
    }

    /// Start `surface` if the session can, and say so in the log if not: a missing surface app
    /// loses its surface, never the session.
    pub fn start_or_log(
        &mut self,
        compositor: &mut Compositor,
        surface: &Surface,
        workspace: &str,
    ) {
        if let Err(error) = self.start(compositor, surface, workspace) {
            tracing::warn!(surface = %surface.name, %error, "a surface app did not start");
        }
    }

    /// Whether a surface app named `name` is running now.
    #[must_use]
    pub fn running(&self, name: &str) -> bool {
        self.running.iter().any(|running| running.name == name)
    }

    /// The names of the surface apps running now.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.running
            .iter()
            .map(|running| running.name.as_str())
            .collect()
    }

    /// When a surface app that published nothing yet is asked to list itself again, so an app that
    /// mounts late is still heard.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.running
            .iter()
            .filter_map(|app| app.actions.next_deadline())
            .min()
    }

    /// Take in the surface apps that exited, and give the session their names. A surface app is
    /// meant to outlive the session's run: one that exited is reported, never restarted on the
    /// session's behalf.
    pub fn poll(&mut self) -> Vec<String> {
        let mut exited = Vec::new();
        let mut running = Vec::with_capacity(self.running.len());
        for mut app in self.running.drain(..) {
            match app.child.try_wait() {
                Ok(Some(status)) => {
                    tracing::warn!(
                        surface = %app.name,
                        %status,
                        lived = ?app.started.elapsed(),
                        "a surface app exited"
                    );
                    exited.push(app.name);
                }
                Ok(None) => running.push(app),
                Err(error) => {
                    tracing::warn!(surface = %app.name, %error, "a surface app could not be waited for");
                    running.push(app);
                }
            }
        }
        self.running = running;
        exited
    }

    /// Answer what the surface apps asked since the last turn, and take in how the invocations they
    /// were asked to run stand.
    pub fn serve(&mut self, door: &mut dyn ActionDoor) {
        for app in &mut self.running {
            // A surface publishes its actions once it has mounted: until it has listed them, the
            // session asks again.
            app.actions.list_if_silent();
            while let Ok(message) = app.actions.incoming.try_recv() {
                match message {
                    // What the surface offers: the session asked for it.
                    RemoteActionMessage::CatalogueResponse {
                        correlation,
                        result,
                    } => {
                        let Ok(page) = result else {
                            continue;
                        };
                        app.actions.listed(correlation, page);
                    }
                    // What the surface offers changed, and it says so as it does: a panel's
                    // controls are there while the device can do them, which the session reads as
                    // it moves.
                    RemoteActionMessage::CatalogueChanged { page } => {
                        app.actions.relisted(page);
                    }
                    // How an invocation the session asked for stands.
                    RemoteActionMessage::Receipt(receipt) => {
                        if let Some(update) = outcome(receipt.invocation, &receipt.status) {
                            app.actions.settle(receipt.invocation, &update);
                        }
                    }
                    RemoteActionMessage::Update { invocation, state } => {
                        if let Some(state) = state {
                            app.actions.settle(invocation, &state.update);
                        }
                    }
                    // The app runs what it publishes: what it asks of its own actions comes back
                    // over the same socket, as the broker hands a provider the invocation of its
                    // own actions, and nothing of it is the session's.
                    RemoteActionMessage::Invoke(request)
                        if request.target.provider == tilo_actions::remote::OWN_PROVIDER =>
                    {
                        let invocation = request.invocation;
                        app.actions.send(RemoteActionMessage::Invoke(request));
                        app.actions
                            .send(RemoteActionMessage::Receipt(InvocationReceipt {
                                invocation,
                                status: ReceiptStatus::Accepted,
                            }));
                    }
                    request => {
                        if let Some(answer) = answer(door, &mut app.actions.answered, request) {
                            app.actions.send(answer);
                        }
                    }
                }
            }
        }
    }

    /// Ask the surface app `name` to run `action` of `resource` , as one of the actions it
    /// publishes, with `argument` , and name the invocation `id` ; `None` when no such app runs.
    pub fn ask(
        &mut self,
        name: &str,
        resource: &str,
        action: &str,
        argument: InvokeArgument,
    ) -> Option<u64> {
        let id = self.asked;
        let invocation = InvocationId::new();
        self.asked = self.asked.wrapping_add(1);
        let app = self
            .running
            .iter_mut()
            .find(|running| running.name == name)?;
        app.actions.asked.insert(
            id,
            InvocationState::new(invocation, InvocationUpdate::Accepted { invocation }),
        );
        app.actions.by_invocation.insert(invocation, id);
        app.actions.send(RemoteActionMessage::Invoke(InvokeRequest {
            invocation,
            target: RemoteActionTarget {
                provider: app.actions.provider.clone(),
                address: tilo_actions::ActionAddress::new(resource, action),
            },
            argument,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        }));
        Some(id)
    }

    /// The actions the surface app `name` offers the desktop now.
    #[must_use]
    pub fn published(&self, name: &str) -> Vec<ExportedAction> {
        self.running
            .iter()
            .find(|running| running.name == name)
            .map(|running| running.actions.published.clone())
            .unwrap_or_default()
    }

    /// How the invocation asked for as `id` stands now.
    #[must_use]
    pub fn outcome(&self, id: u64) -> Option<&InvocationState> {
        self.running
            .iter()
            .find_map(|running| running.actions.asked.get(&id))
    }

    /// Stop `name` , if it runs, and wait for it: its socket closes and the compositor forgets its
    /// surfaces.
    pub fn stop(&mut self, name: &str) {
        let Some(place) = self.running.iter().position(|running| running.name == name) else {
            return;
        };
        let mut app = self.running.remove(place);
        let _ = app.child.kill();
        let _ = app.child.wait();
        tracing::info!(surface = %app.name, "stopped a surface app");
    }

    /// Stop every surface app the session runs, and wait for them.
    pub fn stop_all(&mut self) {
        for mut app in self.running.drain(..) {
            let _ = app.child.kill();
            let _ = app.child.wait();
        }
    }
}

impl Drop for Surfaces {
    fn drop(&mut self) {
        self.stop_all();
    }
}

/// The session's answer to one message from a surface app: `None` for a message no host answers (an
/// answer arriving at the host, and anything the desktop does not offer).
fn answer(
    door: &mut dyn ActionDoor,
    answered: &mut VecDeque<(InvocationId, InvocationUpdate)>,
    message: RemoteActionMessage,
) -> Option<RemoteActionMessage> {
    match message {
        RemoteActionMessage::CatalogueRequest {
            correlation,
            ref query,
        } => {
            let resources = door.resources();
            let revision = revision(&resources);
            let rows: Vec<CatalogueRow> = tilo_actions::door::survey_placed(&resources)
                .into_iter()
                .map(|placed| {
                    CatalogueRow::for_placed_action(SESSION_PROVIDER, SESSION_PROVIDER, placed)
                })
                .collect();
            Some(RemoteActionMessage::CatalogueResponse {
                correlation,
                result: Ok(CataloguePage::from_rows(revision, rows, query)),
            })
        }
        RemoteActionMessage::Invoke(request) => {
            let invocation = request.invocation;
            let status = if request.target.provider == SESSION_PROVIDER {
                let arguments = match request.argument {
                    InvokeArgument::Json(arguments) => arguments,
                    InvokeArgument::Text(_) | InvokeArgument::None => serde_json::Map::default(),
                };
                match door.invoke(&request.target.address, &arguments) {
                    Ok(ResourceOperationStart::Completed(output)) => {
                        ReceiptStatus::Completed { output }
                    }
                    Ok(ResourceOperationStart::Accepted) => ReceiptStatus::Accepted,
                    Err(error) => ReceiptStatus::Rejected {
                        error: refusal(&error),
                    },
                }
            } else {
                ReceiptStatus::Rejected {
                    error: ActionHostError::UnknownAction,
                }
            };
            if let Some(update) = outcome(invocation, &status) {
                if answered.len() >= ANSWERED_KEPT {
                    answered.pop_front();
                }
                answered.push_back((invocation, update));
            }
            Some(RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status,
            }))
        }
        // The desktop runs every action it offers to its end, so an invocation stands where its
        // receipt left it.
        RemoteActionMessage::Observe { invocation, .. } => Some(RemoteActionMessage::Update {
            invocation,
            state: answered
                .iter()
                .rev()
                .find(|(id, _)| *id == invocation)
                .map(|(_, update)| InvocationState::new(invocation, update.clone())),
        }),
        RemoteActionMessage::Cancel { invocation } => Some(RemoteActionMessage::CancelResponse {
            invocation,
            result: Err(ActionHostError::UnknownInvocation),
        }),
        _ => None,
    }
}

/// Where what `resources` publishes stands: equal revisions publish the same actions.
fn revision(resources: &[tilo_actions::ResourceSpec]) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for descriptor in tilo_actions::door::survey(resources) {
        descriptor.address.resource.hash(&mut hasher);
        descriptor.address.action.hash(&mut hasher);
    }
    format!("{:x}", hasher.finish())
}

/// The latest update of an invocation whose receipt is `status` ; `None` for a receipt that left it
/// running and so has nothing new to say.
pub(crate) fn outcome(
    invocation: InvocationId,
    status: &ReceiptStatus,
) -> Option<InvocationUpdate> {
    match status {
        ReceiptStatus::Accepted => None,
        ReceiptStatus::Completed { output } => Some(InvocationUpdate::Succeeded {
            invocation,
            output: output.clone(),
        }),
        ReceiptStatus::Failed { error } => Some(InvocationUpdate::Failed {
            invocation,
            error: error.clone(),
        }),
        ReceiptStatus::Rejected { error } => Some(InvocationUpdate::Failed {
            invocation,
            error: tilo_actions::ActionFailure {
                // An action that waits for the person's word is told apart
                // from one that ran and was refused: what a person reads is
                // that their answer is missing, not that it failed.
                code: match error {
                    ActionHostError::ConfirmationRequired { .. } => {
                        tilo_actions::remote::CONFIRMATION_REQUIRED_CODE.to_owned()
                    }
                    _ => String::from("refused"),
                },
                message: error.to_string(),
                details: None,
                retryable: false,
            },
        }),
        ReceiptStatus::Cancelled { message } => Some(InvocationUpdate::Cancelled {
            invocation,
            message: message.clone(),
        }),
    }
}

/// A door's refusal as the action protocol says it.
pub(crate) fn refusal(error: &tilo_actions::DoorError) -> ActionHostError {
    match error {
        tilo_actions::DoorError::Unknown(_) => ActionHostError::UnknownAction,
        other => ActionHostError::Refused {
            reason: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bar_tells_the_app_where_its_window_goes() {
        let bar = Surface::new("topbar", "/usr/bin/tilo-topbar")
            .layer(Layer::Top)
            .namespace("tilo-bar")
            .anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT)
            .size((0, 32))
            .exclusive(32)
            .keyboard(Keyboard::None);
        let environment: std::collections::BTreeMap<String, String> =
            bar.environment().into_iter().collect();
        let expected = [
            ("TILO_SURFACE_LAYER", "top"),
            ("TILO_SURFACE_NAMESPACE", "tilo-bar"),
            ("TILO_SURFACE_ANCHOR", "top,left,right"),
            ("TILO_SURFACE_SIZE", "0x32"),
            ("TILO_SURFACE_MARGIN", "0,0,0,0"),
            ("TILO_SURFACE_EXCLUSIVE", "32"),
            ("TILO_SURFACE_KEYBOARD", "none"),
        ];
        for (name, value) in expected {
            assert_eq!(
                environment.get(name).map(String::as_str),
                Some(value),
                "{name}"
            );
        }
        assert!(
            !environment.contains_key("TILO_SURFACE_LOCK"),
            "the bar is no lock screen"
        );
    }

    #[test]
    fn a_surface_app_starts_as_an_app_of_this_desktop() {
        // A surface reads its appearance and its menu entries from the session's settings: told
        // nothing of the desktop, it would take them from a host desktop the session happens to run
        // inside.
        let environment: std::collections::BTreeMap<String, String> =
            Surface::new("quick-settings", "/usr/bin/tilo-quick-settings")
                .environment()
                .into_iter()
                .collect();
        for (name, value) in tilo_app_launch::env::desktop_variables() {
            assert_eq!(
                environment.get(name).map(String::as_str),
                Some(value),
                "{name}"
            );
        }
    }

    #[test]
    fn a_lock_screen_asks_for_the_session_lock() {
        let lock = Surface::new("lock", "/usr/bin/tilo-lock-screen")
            .lock()
            .keyboard(Keyboard::Exclusive)
            .anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        let environment: std::collections::BTreeMap<String, String> =
            lock.environment().into_iter().collect();
        assert_eq!(
            environment.get("TILO_SURFACE_LOCK").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            environment.get("TILO_SURFACE_KEYBOARD").map(String::as_str),
            Some("exclusive")
        );
        assert_eq!(
            environment.get("TILO_SURFACE_ANCHOR").map(String::as_str),
            Some("top,bottom,left,right")
        );
    }

    #[test]
    fn no_anchor_is_said_as_none() {
        assert_eq!(Anchor::default().names(), "none");
        assert!(Anchor::default().is_empty());
        assert!(!(Anchor::TOP | Anchor::BOTTOM).is_empty());
    }

    /// One row of a surface's listing, offering `action` : enough for a page without a surface app
    /// to answer.
    fn row(action: &str) -> CatalogueRow {
        let resource = tilo_actions::ResourceSpec::new(
            "quick-settings.panel",
            "quick-settings.panel",
            "Quick Settings",
        )
        .with_action(tilo_actions::ActionSpec::immediate_encoded(
            action,
            action,
            tilo_actions::ExecutionLocus::ClientLocal,
            tilo_actions::EncodedAction {
                type_id: "quick-settings.panel".into(),
                payload: Vec::new(),
            },
        ));
        let descriptor = tilo_actions::door::survey(&[resource])
            .into_iter()
            .next()
            .expect("an action");
        CatalogueRow::for_action(
            "so.tilo.quick-settings",
            "so.tilo.quick-settings",
            descriptor,
        )
    }

    /// The message the session sent the surface off `session` .
    fn told(session: &mut UnixStream) -> RemoteActionMessage {
        let body = FRAMING.read(session).expect("a read").expect("a message");
        RemoteActionFrame::decode(&body).expect("a frame").message
    }

    #[test]
    fn a_listing_is_read_to_its_last_page_before_it_is_the_offer() {
        let (app, mut session) = UnixStream::pair().expect("a socket");
        let mut connection = Connection::of(app, None);
        connection.list();
        let RemoteActionMessage::CatalogueRequest { correlation, query } = told(&mut session)
        else {
            panic!("the first page is asked for");
        };
        assert!(query.cursor.is_empty(), "from the start");
        connection.listed(
            correlation,
            CataloguePage {
                revision: "v1:so.tilo.quick-settings".into(),
                rows: vec![row("toggle-wifi"), row("set-volume")],
                next_cursor: vec![2],
                ..CataloguePage::default()
            },
        );
        assert!(
            connection.published.is_empty(),
            "half a listing is not what the surface offers"
        );
        let RemoteActionMessage::CatalogueRequest { correlation, query } = told(&mut session)
        else {
            panic!("the next page is asked for");
        };
        assert_eq!(query.cursor, vec![2]);
        assert_eq!(query.revision.as_deref(), Some("v1:so.tilo.quick-settings"));
        connection.listed(
            correlation,
            CataloguePage {
                revision: "v1:so.tilo.quick-settings".into(),
                rows: vec![row("play-pause")],
                exhausted: true,
                ..CataloguePage::default()
            },
        );
        assert_eq!(connection.published.len(), 3, "every page, once");
        assert!(connection.reading.is_empty());
    }

    /// A listing the session asked for again supersedes the one it was part
    /// way through: the pages of the old one arrive beside the new one's,
    /// and reading them would repeat rows for ever.
    #[test]
    fn a_page_of_a_listing_the_session_gave_up_on_is_ignored() {
        let (app, mut session) = UnixStream::pair().expect("a socket");
        let mut connection = Connection::of(app, None);
        connection.list();
        let RemoteActionMessage::CatalogueRequest {
            correlation: given_up,
            ..
        } = told(&mut session)
        else {
            panic!("the first page is asked for");
        };
        connection.list();
        let RemoteActionMessage::CatalogueRequest { correlation, .. } = told(&mut session) else {
            panic!("the listing is asked for again");
        };
        connection.listed(
            given_up,
            CataloguePage {
                revision: "v1:so.tilo.quick-settings".into(),
                rows: vec![row("toggle-wifi")],
                exhausted: true,
                ..CataloguePage::default()
            },
        );
        assert!(
            connection.published.is_empty(),
            "the page of a listing given up on is not what the surface offers"
        );
        assert!(connection.reading.is_empty(), "and it continues nothing");
        connection.listed(
            correlation,
            CataloguePage {
                revision: "v1:so.tilo.quick-settings".into(),
                rows: vec![row("set-volume")],
                exhausted: true,
                ..CataloguePage::default()
            },
        );
        assert_eq!(
            connection.published.len(),
            1,
            "the listing it waits for is what it reads"
        );
    }

    #[test]
    fn a_page_from_another_revision_is_read_from_the_start_again() {
        let (app, mut session) = UnixStream::pair().expect("a socket");
        let mut connection = Connection::of(app, None);
        connection.published = vec![ExportedAction::from(
            &row("toggle-wifi").action.expect("an action"),
        )];
        connection.list();
        let RemoteActionMessage::CatalogueRequest { correlation, .. } = told(&mut session) else {
            panic!("the first page is asked for");
        };
        connection.listed(
            correlation,
            CataloguePage {
                revision: "v2:so.tilo.quick-settings".into(),
                stale_cursor: true,
                ..CataloguePage::default()
            },
        );
        assert!(connection.reading.is_empty(), "the old pages are dropped");
        assert_eq!(
            connection.published.len(),
            1,
            "what it offered before stays until the new listing is whole"
        );
        let RemoteActionMessage::CatalogueRequest { query, .. } = told(&mut session) else {
            panic!("the listing is asked for again");
        };
        assert!(query.cursor.is_empty());
        assert!(query.revision.is_none());
    }
}
