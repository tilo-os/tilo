//! One Chromium host per profile, shared by every process that opens it.
//!
//! A host needs its profile directory to itself, yet every Browser process
//! browsing with a profile needs that profile's cookies and logins. So the
//! first process to open a profile becomes its owner: it holds
//! `<profile>.lock` for as long as its host runs, starts the host and serves
//! `<profile>.sock`, both beside the profile directory. Every other process
//! connects to that socket and drives its own pages through the owner's host.
//!
//! The socket speaks the host's packet protocol. A client writes exactly the
//! commands it would write to a host, plus `shared_create_tab`, for which the
//! owner allocates a tab from the host's one tab counter and capacity. The
//! owner forwards to a client only the events and frames of the tabs that
//! client created, plus the process-wide download and readiness events.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{self, BufReader, BufWriter};
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

use serde_json::{Value, json};
use tilo_process::rendezvous::{Connection, Joined, Listener, Ownership, Rendezvous};

use super::{
    ChromiumEngine, CommandQueue, Routes, Runtime, Shared, deliver_frame, fail_routes, protocol,
    queue_download_event, route_native_event, spawn_named,
};
use crate::time::Duration;
use crate::webview::{Frame, WebViewEvent};
use tilo_lock::MutexRecover as _;

/// Events a client has not read yet. Beyond this, a slow client loses its
/// oldest replaceable state first.
const OUTBOX_EVENTS: usize = 1024;

/// Per-tab state events: a newer one replaces the queued older one.
const SUPERSEDED: &[&str] = &[
    "uri",
    "title",
    "favicon",
    "audio_muted",
    "media_state",
    "fullscreen",
    "progress",
    "history",
    "zoom",
    "download_runtime",
];

/// Page audio packets a client may be behind by before the oldest is dropped.
const AUDIO_PACKETS: usize = 16;

fn is_audio_packet(value: &Value) -> bool {
    value.get("event").and_then(Value::as_str) == Some("audio")
}

fn superseded(value: &Value) -> bool {
    value
        .get("event")
        .and_then(Value::as_str)
        .is_some_and(|event| SUPERSEDED.contains(&event))
}

/// What the owner still has to send one client: its events in order, and
/// only the newest frame of each of its tabs.
#[derive(Default)]
pub(super) struct Outbox {
    state: Mutex<OutboxState>,
    ready: Condvar,
}

#[derive(Default)]
struct OutboxState {
    events: VecDeque<Value>,
    frames: BTreeMap<u32, Frame>,
    closed: bool,
}

enum Outgoing {
    Json(Value),
    Frame(u32, Frame),
}

impl Outbox {
    pub(super) fn event(&self, value: Value) {
        let mut state = self.state.lock_recover();
        if state.closed {
            return;
        }
        if superseded(&value) {
            state.events.retain(|old| {
                old.get("event") != value.get("event") || old.get("tab") != value.get("tab")
            });
        }
        if is_audio_packet(&value) {
            // PCM is real time: past 16 queued packets (~160 ms) the oldest
            // goes, so a slow client hears a gap rather than lagging behind.
            let queued = state
                .events
                .iter()
                .filter(|old| is_audio_packet(old))
                .count();
            if queued >= AUDIO_PACKETS
                && let Some(oldest) = state.events.iter().position(is_audio_packet)
            {
                state.events.remove(oldest);
            }
        }
        if state.events.len() >= OUTBOX_EVENTS {
            let oldest = state.events.iter().position(superseded).unwrap_or(0);
            state.events.remove(oldest);
        }
        state.events.push_back(value);
        drop(state);
        self.ready.notify_one();
    }

    pub(super) fn frame(&self, tab: u32, frame: Frame) {
        let mut state = self.state.lock_recover();
        if state.closed {
            return;
        }
        state.frames.insert(tab, frame);
        drop(state);
        self.ready.notify_one();
    }

    fn close(&self) {
        let mut state = self.state.lock_recover();
        state.closed = true;
        state.events.clear();
        state.frames.clear();
        drop(state);
        self.ready.notify_all();
    }

    fn next(&self) -> Option<Outgoing> {
        let mut state = self.state.lock().ok()?;
        loop {
            if state.closed {
                return None;
            }
            if let Some(value) = state.events.pop_front() {
                return Some(Outgoing::Json(value));
            }
            if let Some((tab, frame)) = state.frames.pop_first() {
                return Some(Outgoing::Frame(tab, frame));
            }
            state = self.ready.wait(state).ok()?;
        }
    }
}

/// The owner's connected clients, as the host reader sees them.
#[derive(Default)]
pub(super) struct Hub {
    clients: Mutex<(HashMap<u64, Arc<Outbox>>, bool)>,
}

impl Hub {
    /// Delivers a process-wide host event to every client.
    pub(super) fn broadcast(&self, value: &Value) {
        let clients: Vec<_> = self.clients.lock_recover().0.values().cloned().collect();
        for outbox in clients {
            outbox.event(value.clone());
        }
    }

    /// Disconnects every client and refuses new ones: the host is gone.
    pub(super) fn close_all(&self) {
        let clients: Vec<_> = {
            let mut clients = self.clients.lock_recover();
            clients.1 = true;
            clients.0.drain().map(|(_, outbox)| outbox).collect()
        };
        for outbox in clients {
            outbox.close();
        }
    }

    fn join(&self, id: u64, outbox: Arc<Outbox>) -> bool {
        let mut clients = self.clients.lock_recover();
        if clients.1 {
            return false;
        }
        clients.0.insert(id, outbox);
        true
    }

    fn leave(&self, id: u64) {
        self.clients.lock_recover().0.remove(&id);
    }
}

/// Blocks while other Browser processes still use a Chromium host this
/// process runs, then stops serving them; a host none of this process's
/// engines still uses shuts down before this returns.
///
/// Call it once the UI event loop has exited and the app has dropped its
/// engines: the process that started a profile's host must outlive every
/// process browsing through it. Returns at once when nothing is shared.
pub fn serve_shared_until_idle() {
    for server in servers() {
        server.serve_until_idle();
    }
}

/// How long a process waits for the profile's current owner: it may be
/// starting its host or waiting for its previous host to exit.
const CLAIM_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a client waits for the owner to answer a new tab.
const TAB_TIMEOUT: Duration = Duration::from_secs(20);
/// How often the accept loop looks for a stop.
const ACCEPT_POLL: Duration = Duration::from_millis(250);

/// Commands that act on the whole profile rather than on one page. A client
/// may send them without naming a tab it owns.
const PROFILE_OPS: &[&str] = &[
    "filter_rules",
    "scripts",
    "blocking_scripts",
    "clear_data",
    "clear_on_exit",
    "site_exceptions",
    "download_decision",
    "download_cancel",
    "download_retry",
    "download_cancel_all",
];

static SERVERS: Mutex<Vec<Weak<Server>>> = Mutex::new(Vec::new());

fn servers() -> Vec<Arc<Server>> {
    SERVERS
        .lock_recover()
        .iter()
        .filter_map(Weak::upgrade)
        .collect()
}

/// The outcome of opening a profile.
pub(super) enum Role {
    /// No host runs this profile: start one and serve it.
    Owner(Claim),
    /// This platform cannot share a host: start one for this process only.
    Alone,
    /// Another process's host runs this profile.
    Client(Connection),
    /// The profile's owner did not let this process in.
    Busy(String),
}

/// The right to run the profile's host: held until that host exits.
pub(super) struct Claim(Ownership);

/// Becomes the owner of the (canonical, existing) `profile`, or connects to
/// its owner.
///
/// # Errors
///
/// Returns an error when the profile's socket path is too long to bind:
/// starting a second host on the same profile is never the fallback.
pub(super) fn claim(profile: &Path) -> Result<Role, String> {
    let rendezvous = match Rendezvous::beside(profile) {
        Ok(rendezvous) => rendezvous,
        Err(error) if error.kind() == io::ErrorKind::Unsupported => return Ok(Role::Alone),
        Err(error) => return Err(format!("The Chromium profile's engine socket: {error}")),
    };
    Ok(match rendezvous.join(CLAIM_TIMEOUT, handshake) {
        Ok(Joined::Owner(ownership)) => Role::Owner(Claim(ownership)),
        Ok(Joined::Peer(connection)) => Role::Client(connection),
        Err(error) => Role::Busy(format!(
            "The Browser process running this profile's Chromium engine: {error}"
        )),
    })
}

fn handshake(mut connection: &Connection) -> io::Result<()> {
    connection.set_read_timeout(Some(Duration::from_secs(10)))?;
    // Unbuffered, so nothing after the greeting is consumed here.
    match protocol::read_packet(&mut connection)? {
        Some(protocol::Packet::Json(value))
            if value.get("event").and_then(Value::as_str) == Some("shared_hello") => {}
        _ => {
            return Err(io::Error::other(
                "the profile's owner refused the connection",
            ));
        }
    }
    connection.set_read_timeout(None)
}

impl Claim {
    /// Serves the running host on the profile's socket.
    pub(super) fn serve(
        &self,
        runtime: Weak<Runtime>,
        hub: &Arc<Hub>,
    ) -> Result<Arc<Server>, String> {
        let listener = self
            .0
            .listen()
            .map_err(|error| format!("Could not serve the Chromium engine: {error}"))?;
        let server = Arc::new(Server {
            hub: hub.clone(),
            runtime,
            listener,
            state: Mutex::default(),
            changed: Condvar::new(),
        });
        // Published before the first client is admitted: a client that has
        // connected finds its owner's server on the owner's runtime.
        if let Some(runtime) = server.runtime.upgrade() {
            let _ = runtime.server.set(server.clone());
        }
        let weak = Arc::downgrade(&server);
        if let Err(error) = spawn_named("chromium-share", move || accept_loop(&weak)) {
            server.listener.close();
            return Err(format!("Chromium engine socket: {error}"));
        }
        let mut servers = SERVERS.lock_recover();
        servers.retain(|server| server.strong_count() > 0);
        servers.push(Arc::downgrade(&server));
        drop(servers);
        Ok(server)
    }

    /// Gives the profile up once its host has exited. Until then no other
    /// process may start a host on it.
    pub(super) fn release_after_exit(self, server: Option<&Arc<Server>>) {
        if let Some(server) = server {
            server.stop();
        }
        drop(self);
        if let Some(server) = server {
            server.mark_host_exited();
        }
    }
}

#[derive(Default)]
struct ServerState {
    clients: usize,
    next_client: u64,
    /// Keeps the host running while any client is connected.
    keepalive: Option<Arc<Runtime>>,
    serving: bool,
    stopped: bool,
    host_exited: bool,
}

/// The owner's side of the profile socket.
pub(super) struct Server {
    hub: Arc<Hub>,
    runtime: Weak<Runtime>,
    listener: Listener,
    state: Mutex<ServerState>,
    changed: Condvar,
}

fn accept_loop(server: &Weak<Server>) {
    loop {
        let Some(server) = server.upgrade() else {
            break;
        };
        if server.lock_state().stopped {
            break;
        }
        match server.listener.accept_within(ACCEPT_POLL) {
            Ok(Some(connection)) => server.admit(connection),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "Chromium engine socket accept failed");
                server.stop();
                break;
            }
        }
    }
}

fn refuse(mut connection: &Connection, why: &str) {
    let _ = protocol::write_json(
        &mut connection,
        &json!({"event":"shared_refused","error":why}),
    );
}

impl Server {
    fn lock_state(&self) -> std::sync::MutexGuard<'_, ServerState> {
        self.state.lock_recover()
    }

    fn admit(self: &Arc<Self>, connection: Connection) {
        let runtime = self.runtime.upgrade();
        let admitted = {
            let mut state = self.lock_state();
            match &runtime {
                _ if state.stopped => None,
                Some(runtime) if !runtime.process.failed.load(Ordering::Acquire) => {
                    let id = state.next_client;
                    state.next_client = id.wrapping_add(1);
                    let outbox = Arc::new(Outbox::default());
                    if self.hub.join(id, outbox.clone()) {
                        state.clients = state.clients.saturating_add(1);
                        if state.keepalive.is_none() {
                            state.keepalive = Some(runtime.clone());
                        }
                        Some((id, outbox))
                    } else {
                        None
                    }
                }
                _ => None,
            }
        };
        // A temporary reference may be the last one: drop it unlocked.
        drop(runtime);
        let Some((id, outbox)) = admitted else {
            refuse(&connection, "the Chromium engine is shutting down");
            return;
        };
        outbox.event(json!({"event":"shared_hello"}));
        let started = connection.try_clone().and_then(|writer| {
            let writer_outbox = outbox.clone();
            spawn_named("chromium-share-out", move || {
                send_to_client(&writer_outbox, &writer);
            })?;
            let server = self.clone();
            let reader_outbox = outbox.clone();
            spawn_named("chromium-share-in", move || {
                serve_client(&server, id, &reader_outbox, &connection);
            })
        });
        if let Err(error) = started {
            tracing::warn!(%error, "Could not serve a Browser process on the Chromium engine");
            outbox.close();
            self.hub.leave(id);
            self.client_left();
        }
    }

    fn client_left(&self) {
        let keepalive = {
            let mut state = self.lock_state();
            state.clients = state.clients.saturating_sub(1);
            self.changed.notify_all();
            if state.clients == 0 && !state.serving {
                state.keepalive.take()
            } else {
                None
            }
        };
        // When the owner's engines are gone too, this shuts the host down.
        drop(keepalive);
    }

    /// Stops accepting, disconnects every client and removes the socket.
    pub(super) fn stop(&self) {
        self.lock_state().stopped = true;
        self.close_listener();
    }

    fn close_listener(&self) {
        self.listener.close();
        self.hub.close_all();
        self.changed.notify_all();
    }

    fn mark_host_exited(&self) {
        self.lock_state().host_exited = true;
        self.changed.notify_all();
    }

    /// Blocks until no client is connected, then stops serving. When nothing
    /// in this process still uses the host, waits for it to exit.
    pub(super) fn serve_until_idle(&self) {
        let keepalive = {
            let mut state = self.lock_state();
            state.serving = true;
            while state.clients > 0 {
                state = self
                    .changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            state.stopped = true;
            state.keepalive.take()
        };
        self.close_listener();
        drop(keepalive);
        if self.runtime.strong_count() > 0 {
            // The app still holds engines; dropping them shuts the host down.
            return;
        }
        let deadline = crate::time::now() + Duration::from_secs(40);
        let mut state = self.lock_state();
        while !state.host_exited {
            let Some(left) = deadline.checked_duration_since(crate::time::now()) else {
                tracing::warn!("The Chromium engine did not exit in time");
                return;
            };
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

fn send_to_client(outbox: &Outbox, connection: &Connection) {
    let mut writer = BufWriter::new(connection);
    while let Some(item) = outbox.next() {
        let sent = match item {
            Outgoing::Json(value) => protocol::write_json(&mut writer, &value),
            Outgoing::Frame(tab, frame) => protocol::write_tab_frame(&mut writer, tab, &frame),
        };
        if sent.is_err() {
            break;
        }
    }
    outbox.close();
    drop(writer);
    connection.close();
}

/// A client's tab on the owner. `closing` once the client closed it.
struct ClientTab {
    shared: Arc<Shared>,
    closing: bool,
}

fn serve_client(server: &Arc<Server>, id: u64, outbox: &Arc<Outbox>, connection: &Connection) {
    let mut tabs = HashMap::new();
    let mut reader = BufReader::new(connection);
    while let Ok(Some(protocol::Packet::Json(command))) = protocol::read_packet(&mut reader) {
        let Some(runtime) = server.runtime.upgrade() else {
            break;
        };
        client_command(&runtime, command, &mut tabs, outbox);
    }
    outbox.close();
    server.hub.leave(id);
    connection.close();
    if let Some(runtime) = server.runtime.upgrade() {
        close_client_tabs(&runtime, tabs);
    }
    server.client_left();
}

fn owns_route(routes: &Routes, tab: u32, shared: &Arc<Shared>) -> bool {
    routes
        .lock()
        .ok()
        .and_then(|routes| routes.get(&tab)?.upgrade())
        .is_some_and(|route| Arc::ptr_eq(&route, shared))
}

fn client_command(
    runtime: &Runtime,
    mut command: Value,
    tabs: &mut HashMap<u32, ClientTab>,
    outbox: &Arc<Outbox>,
) {
    let op = command
        .get("op")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if op == "shared_create_tab" {
        let Some(request) = command.get("request").and_then(Value::as_u64) else {
            return;
        };
        // Tabs the host already closed no longer belong to anyone.
        tabs.retain(|tab, entry| owns_route(&runtime.routes, *tab, &entry.shared));
        let shared = Arc::new(Shared {
            remote: Some(outbox.clone()),
            ..Shared::default()
        });
        match runtime.admit_tab(&shared, |tab| {
            outbox.event(json!({"event":"shared_tab","request":request,"tab":tab}));
        }) {
            Ok(tab) => {
                tabs.insert(
                    tab,
                    ClientTab {
                        shared,
                        closing: false,
                    },
                );
            }
            Err(error) => {
                outbox.event(json!({"event":"shared_tab","request":request,"error":error}));
            }
        }
        return;
    }
    let tab = command
        .get("tab")
        .and_then(Value::as_u64)
        .and_then(|tab| u32::try_from(tab).ok());
    let owned = tab.and_then(|tab| tabs.get_mut(&tab));
    match owned {
        _ if matches!(op.as_str(), "shutdown" | "create_tab") => {
            tracing::warn!(op, "Refused a Browser process's engine lifecycle command");
            return;
        }
        Some(entry) => {
            if op == "close_tab" {
                entry.closing = true;
            }
        }
        None if PROFILE_OPS.contains(&op.as_str()) => {
            if let Some(fields) = command.as_object_mut() {
                fields.remove("tab");
            }
        }
        None => {
            tracing::warn!(op, "Refused a command for a tab another process owns");
            return;
        }
    }
    if let Err(error) = runtime.tx.push(command) {
        tracing::warn!(%error, op, "Chromium command from a Browser process was not queued");
    }
}

/// Closes the tabs of a client that disconnected. Their routes keep
/// reserving capacity until the host acknowledges each close.
fn close_client_tabs(runtime: &Runtime, tabs: HashMap<u32, ClientTab>) {
    for (tab, entry) in tabs {
        let retire = {
            let mut routes = runtime.routes.lock_recover();
            match routes.get_mut(&tab) {
                Some(route)
                    if route
                        .upgrade()
                        .is_some_and(|route| Arc::ptr_eq(&route, &entry.shared)) =>
                {
                    *route = Weak::new();
                    !entry.closing
                }
                _ => false,
            }
        };
        if retire {
            let _ = runtime.tx.push(json!({"op":"close_tab","tab":tab}));
        }
    }
}

struct PendingTab {
    shared: Arc<Shared>,
    reply: SyncSender<Result<u32, String>>,
}

type Pending = Arc<Mutex<HashMap<u64, PendingTab>>>;

/// A client's connection to the profile's owner.
pub(super) struct ClientLink {
    connection: Connection,
    pending: Pending,
    next_request: AtomicU64,
}

impl ClientLink {
    /// Asks the owner for a tab and routes it to `shared`.
    pub(super) fn create_tab(&self, tx: &CommandQueue, shared: Arc<Shared>) -> Result<u32, String> {
        let request = self.next_request.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = sync_channel(1);
        self.pending
            .lock()
            .map_err(|_| "Tab requests unavailable")?
            .insert(request, PendingTab { shared, reply });
        if let Err(error) = tx.push(json!({"op":"shared_create_tab","request":request})) {
            self.forget(request);
            return Err(error.into());
        }
        match answer.recv_timeout(TAB_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Disconnected) => Err("Chromium engine disconnected".into()),
            Err(RecvTimeoutError::Timeout) => {
                // The reader answers while holding the pending lock, so an
                // answer is either still pending or already sent.
                if self.forget(request) {
                    Err(
                        "The Browser process running this profile's Chromium engine did not answer"
                            .into(),
                    )
                } else {
                    answer
                        .try_recv()
                        .unwrap_or_else(|_| Err("Chromium engine disconnected".into()))
                }
            }
        }
    }

    fn forget(&self, request: u64) -> bool {
        self.pending.lock_recover().remove(&request).is_some()
    }

    /// Cuts the connection, as the owner's exit would.
    pub(super) fn disconnect(&self) {
        self.connection.close();
    }
}

/// Opens a client engine over an accepted owner connection. Its first
/// endpoint is a new tab: tab 1 always belongs to the owner.
pub(super) fn connect(connection: Connection) -> Result<Arc<ChromiumEngine>, String> {
    let tx = Arc::new(CommandQueue::default());
    let routes: Routes = Arc::default();
    let download_events = Arc::new(Mutex::new(VecDeque::new()));
    let pending: Pending = Arc::default();
    let link = ClientLink {
        connection: connection.try_clone().map_err(|error| error.to_string())?,
        pending: pending.clone(),
        next_request: AtomicU64::new(1),
    };
    let writer = connection.try_clone().map_err(|error| error.to_string())?;
    let queue = tx.clone();
    let writer_routes = routes.clone();
    let started = spawn_named("chromium-client-commands", move || {
        send_commands(&queue, &writer_routes, &writer);
    })
    .and_then(|_| {
        let reader_routes = routes.clone();
        let reader_downloads = download_events.clone();
        let reader_tx = tx.clone();
        spawn_named("chromium-client-events", move || {
            receive_events(
                &connection,
                &reader_routes,
                &reader_downloads,
                &pending,
                &reader_tx,
            );
        })
    });
    if let Err(error) = started {
        tx.close();
        link.disconnect();
        return Err(format!("Chromium engine connection: {error}"));
    }
    let runtime = Arc::new(Runtime {
        tx: tx.clone(),
        process: Arc::new(Shared::default()),
        supervisor: Mutex::new(None),
        routes,
        next_tab: AtomicU32::new(0),
        download_events,
        server: OnceLock::new(),
        link: Some(link),
        service_session: None,
        exited: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });
    let shared = Arc::new(Shared::default());
    let tab = runtime
        .link
        .as_ref()
        .ok_or("Chromium engine connection unavailable")?
        .create_tab(&tx, shared.clone())?;
    Ok(Arc::new(ChromiumEngine {
        tx,
        shared,
        runtime,
        tab,
    }))
}

fn send_commands(queue: &CommandQueue, routes: &Routes, connection: &Connection) {
    let mut writer = BufWriter::new(connection);
    while let Some(command) = queue.pop() {
        if let Err(error) = protocol::write_json(&mut writer, &command) {
            fail_routes(routes, &format!("Chromium command channel closed: {error}"));
            break;
        }
    }
    queue.close();
    drop(writer);
    connection.close();
}

fn receive_events(
    connection: &Connection,
    routes: &Routes,
    downloads: &Mutex<VecDeque<WebViewEvent>>,
    pending: &Pending,
    tx: &CommandQueue,
) {
    let mut reader = BufReader::new(connection);
    let (ready, _ready) = sync_channel(1);
    loop {
        match protocol::read_packet(&mut reader) {
            Ok(Some(protocol::Packet::TabFrame(tab, frame))) => deliver_frame(routes, tab, frame),
            Ok(Some(protocol::Packet::Json(value))) => {
                if value.get("event").and_then(Value::as_str) == Some("shared_tab") {
                    answer_tab(&value, routes, pending, tx);
                } else if let Err(error) = route_native_event(&value, routes, downloads, &ready) {
                    tracing::warn!(error, "Invalid event from the shared Chromium engine");
                    break;
                }
            }
            Ok(Some(protocol::Packet::Frame(_)) | None) | Err(_) => break,
        }
    }
    pending.lock_recover().clear();
    tx.close();
    connection.close();
    queue_download_event(
        downloads,
        WebViewEvent::DownloadRuntime {
            admitted: 0,
            detached: 0,
            accepting: false,
            error: "Chromium engine disconnected".into(),
        },
    );
    fail_routes(routes, "Chromium engine disconnected");
}

fn answer_tab(value: &Value, routes: &Routes, pending: &Pending, tx: &CommandQueue) {
    let Some(request) = value.get("request").and_then(Value::as_u64) else {
        return;
    };
    let tab = value
        .get("tab")
        .and_then(Value::as_u64)
        .filter(|tab| *tab > 1 && i32::try_from(*tab).is_ok())
        .and_then(|tab| u32::try_from(tab).ok());
    let mut pending = pending.lock_recover();
    match (pending.remove(&request), tab) {
        (Some(waiting), Some(tab)) => {
            routes
                .lock_recover()
                .insert(tab, Arc::downgrade(&waiting.shared));
            let _ = waiting.reply.send(Ok(tab));
        }
        (Some(waiting), None) => {
            let error = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("The Chromium engine could not open a tab");
            let _ = waiting.reply.send(Err(error.to_owned()));
        }
        // Nobody waits for this tab any more: give it back.
        (None, Some(tab)) => {
            let _ = tx.push(json!({"op":"close_tab","tab":tab}));
        }
        (None, None) => {}
    }
    drop(pending);
}
