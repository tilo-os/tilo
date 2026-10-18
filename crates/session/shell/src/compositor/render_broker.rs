//! The render broker: which render service draws which app, and keeping
//! those services alive.
//!
//! Toolkit apps do not draw on the GPU themselves; the render service of
//! their room does (`tilo-render-service`, `docs/architecture/app-rendering.md`).
//! The broker is where that is decided and supervised:
//!
//! - **Attribution.** The launcher connects every app it starts to the
//!   broker's socket and writes the app's id and room before the app holds
//!   the connection (`tilo_render_protocol::broker`). The broker reads that,
//!   never anything the app says, to pick a service.
//! - **One service per room.** The person's private rooms (nobody but them
//!   on the roster or present) share one service; every other room gets its
//!   own, so an app in a room with strangers never shares a device, a cache
//!   or GPU memory with another room's apps. A room whose privacy changes
//!   moves its apps: the broker drops their channels on the old service and
//!   they ask again.
//! - **Channels.** Each request gets a fresh `SOCK_SEQPACKET` pair: one end
//!   goes to the service with the app's identity, the other back to the app.
//! - **Supervision.** A service is started on first use (the private one at
//!   session start, so the first app finds it warm) as `shell
//!   __render-service`, holding only its broker socket. When one dies, apps
//!   keep their last frame on screen; their next request starts a new one.
//!   A service that keeps dying is restarted with backoff, never in a spin.

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tilo_lock::MutexRecover as _;
use tilo_render_protocol::broker as wire;
use tilo_render_protocol::channel::Channel;
use tilo_render_service::broker::{BROKER_FD_ENV, FromService, MODE, ToService};

use crate::shell::state::WorkspaceTreeHandle;

/// The service name of the person's private rooms.
pub const PRIVATE: &str = "private";

/// How long a request waits for a starting service to be ready.
const READY_WAIT: Duration = Duration::from_secs(10);

/// A service that lived at least this long is not crash-looping.
const STABLE_AFTER: Duration = Duration::from_secs(30);

/// Most channels one app connection may hold open on the broker's books.
const MAX_CHANNELS_PER_APP: usize = 32;

/// Environment a service must not inherit: it talks to no display and no bus.
const STRIPPED_ENV: &[&str] = &[
    "WAYLAND_DISPLAY",
    "WAYLAND_SOCKET",
    "DISPLAY",
    "DBUS_SESSION_BUS_ADDRESS",
    "TILO_SVC_RENDER_FD",
    "TILO_RENDER_SOCKET",
];

/// Which service draws apps of a room.
pub fn render_key(tree: &crate::shell::state::WorkspaceTree, room: Option<&str>) -> String {
    let Some(room) = room else {
        // Launched outside any room: by the person, from their own session.
        return PRIVATE.to_owned();
    };
    let Some(workspace) = tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room.to_string() == room)
    else {
        // A room this session does not know is nobody's private room.
        return format!("room:{room}");
    };
    let me = crate::shell::state::local_user_id_for_room_prompt(&workspace.room);
    let private = workspace.roster().iter().all(|(user, _)| *user == me)
        && workspace.collaborators.iter().all(|person| person.id == me);
    if private {
        PRIVATE.to_owned()
    } else {
        format!("room:{room}")
    }
}

/// A running service.
struct Service {
    channel: Arc<Channel>,
    /// `Some(device)` once ready; `None` while it starts.
    ready: Arc<(Mutex<Option<Result<String, ()>>>, Condvar)>,
    /// Reaped by the service's watcher.
    _child: Arc<Mutex<std::process::Child>>,
}

/// Restart bookkeeping for one key.
#[derive(Default, Clone, Copy)]
struct Crashes {
    count: u32,
    last_start: Option<Instant>,
}

/// An app channel the broker handed out.
struct Live {
    key: String,
    room: Option<String>,
}

struct Inner {
    tree: WorkspaceTreeHandle,
    exe: PathBuf,
    render_node: Option<PathBuf>,
    services: Mutex<HashMap<String, Service>>,
    crashes: Mutex<HashMap<String, Crashes>>,
    live: Mutex<HashMap<u64, Live>>,
    next_connection: AtomicU64,
    /// The tree generation the live channels' keys were last checked at.
    checked: AtomicU64,
}

/// The broker; serves until dropped with the session.
pub struct RenderBroker {
    inner: Arc<Inner>,
}

impl RenderBroker {
    /// Bind the broker's socket and start the private rooms' service.
    ///
    /// # Errors
    ///
    /// When the socket cannot be bound or the acceptor cannot start.
    #[allow(
        clippy::disallowed_methods,
        reason = "compositor helper: the broker accepts and serves on its own threads"
    )]
    pub fn start(tree: WorkspaceTreeHandle) -> Result<Self, String> {
        launcher::services::ensure_services_root()
            .map_err(|error| format!("create render broker directory: {error}"))?;
        let path = launcher::services::socket_for(wire::SERVICE);
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|error| format!("remove stale render broker socket: {error}"))?;
        }
        let listener = UnixListener::bind(&path)
            .map_err(|error| format!("bind render broker {}: {error}", path.display()))?;
        let exe = std::env::current_exe()
            .map_err(|error| format!("render broker: the shell's own binary: {error}"))?;
        let inner = Arc::new(Inner {
            tree,
            exe,
            render_node: crate::compositor::render_node::advertised().map(PathBuf::from),
            services: Mutex::default(),
            crashes: Mutex::default(),
            live: Mutex::default(),
            next_connection: AtomicU64::new(1),
            checked: AtomicU64::new(0),
        });
        // Warm before the first app asks.
        let warm = Arc::clone(&inner);
        std::thread::Builder::new()
            .name("tilo-render-warm".into())
            .spawn(move || {
                if let Err(error) = warm.service(PRIVATE) {
                    tracing::warn!(%error, "the private rooms' render service did not start");
                }
            })
            .map_err(|error| format!("render broker: {error}"))?;
        let acceptor = Arc::clone(&inner);
        std::thread::Builder::new()
            .name("tilo-render-accept".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => {
                            let inner = Arc::clone(&acceptor);
                            let _ = std::thread::Builder::new()
                                .name("tilo-render-app".into())
                                .spawn(move || inner.serve_app(stream));
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => {
                            tracing::warn!(%error, "render broker accept failed");
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
            })
            .map_err(|error| format!("render broker: {error}"))?;
        tracing::info!(path = %path.display(), "serving apps' render services");
        Ok(Self { inner })
    }

    /// Move apps whose room changed hands between private and shared to the
    /// right service. Cheap when the tree did not change: call every tick.
    pub fn sync_rooms(&self) {
        let generation = self.inner.tree.generation();
        if self.inner.checked.swap(generation, Ordering::AcqRel) == generation {
            return;
        }
        let tree = self.inner.tree.read();
        let mut moved = Vec::new();
        {
            let mut live = self.inner.live.lock_recover();
            live.retain(|connection, channel| {
                let key = render_key(&tree, channel.room.as_deref());
                if key == channel.key {
                    return true;
                }
                moved.push((*connection, channel.key.clone()));
                false
            });
        }
        drop(tree);
        let services = self.inner.services.lock_recover();
        for (connection, key) in moved {
            tracing::info!(connection, %key, "a room changed hands: its app renders elsewhere now");
            if let Some(service) = services.get(&key) {
                let _ = service.channel.send(&ToService::Drop { connection }, &[]);
            }
        }
    }
}

impl Inner {
    /// Serve one app's broker connection: its handshake, then its requests.
    fn serve_app(self: &Arc<Self>, mut stream: UnixStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let handshake = match wire::read_handshake(&mut stream) {
            Ok(handshake) => handshake,
            Err(error) => {
                tracing::debug!(%error, "render broker: no handshake");
                return;
            }
        };
        let _ = stream.set_read_timeout(None);
        let mut mine: Vec<u64> = Vec::new();
        loop {
            match wire::read_request(stream.as_fd()) {
                Ok(true) => {}
                Ok(false) | Err(_) => break,
            }
            mine.retain(|connection| self.live.lock_recover().contains_key(connection));
            let channel = if mine.len() >= MAX_CHANNELS_PER_APP {
                Err("too many windows".to_owned())
            } else {
                self.connect(&handshake)
            };
            match channel {
                Ok((connection, channel)) => {
                    mine.push(connection);
                    if wire::answer(stream.as_fd(), Some(&channel)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    tracing::warn!(app = %handshake.app_id, %error, "no render service for the app");
                    if wire::answer(stream.as_fd(), None).is_err() {
                        break;
                    }
                }
            }
        }
        let mut live = self.live.lock_recover();
        for connection in mine {
            live.remove(&connection);
        }
    }

    /// A channel to the service of the app's room.
    fn connect(self: &Arc<Self>, handshake: &wire::Handshake) -> Result<(u64, Channel), String> {
        let key = render_key(&self.tree.read(), handshake.workspace_id.as_deref());
        let service = self.service(&key)?;
        let (app, theirs) = Channel::pair().map_err(|error| error.to_string())?;
        let connection = self.next_connection.fetch_add(1, Ordering::Relaxed);
        service
            .send(
                &ToService::Connect {
                    connection,
                    app_id: handshake.app_id.clone(),
                    room: key.clone(),
                },
                &[theirs.as_fd()],
            )
            .map_err(|error| error.to_string())?;
        self.live.lock_recover().insert(
            connection,
            Live {
                key,
                room: handshake.workspace_id.clone(),
            },
        );
        Ok((connection, app))
    }

    /// The running, ready service for `key`, started if there is none.
    fn service(self: &Arc<Self>, key: &str) -> Result<Arc<Channel>, String> {
        let (channel, ready) = {
            let mut services = self.services.lock_recover();
            if let Some(service) = services.get(key) {
                (Arc::clone(&service.channel), Arc::clone(&service.ready))
            } else {
                let service = self.spawn(key)?;
                let found = (Arc::clone(&service.channel), Arc::clone(&service.ready));
                services.insert(key.to_owned(), service);
                found
            }
        };
        let (state, signal) = &*ready;
        let state = state.lock_recover();
        let (state, timeout) = signal
            .wait_timeout_while(state, READY_WAIT, |state| state.is_none())
            .map_err(|_| "render service state poisoned".to_owned())?;
        match &*state {
            Some(Ok(_)) => Ok(channel),
            Some(Err(())) => Err(format!("the {key} render service failed to start")),
            None if timeout.timed_out() => Err(format!("the {key} render service is not ready")),
            None => Err("render service state".to_owned()),
        }
    }

    /// Start the service for `key`, waiting out the backoff of one that
    /// keeps dying.
    #[allow(
        clippy::disallowed_methods,
        reason = "compositor helper: each service is watched by a thread of its own"
    )]
    fn spawn(self: &Arc<Self>, key: &str) -> Result<Service, String> {
        let delay = {
            let mut crashes = self.crashes.lock_recover();
            let record = crashes.entry(key.to_owned()).or_default();
            let delay = backoff(*record);
            record.last_start = Some(Instant::now());
            delay
        };
        if !delay.is_zero() {
            tracing::warn!(%key, ?delay, "render service keeps dying; waiting before starting it again");
            std::thread::sleep(delay);
        }
        let (ours, theirs) = Channel::pair().map_err(|error| error.to_string())?;
        let mut command = std::process::Command::new(&self.exe);
        command
            .arg(MODE)
            .env(BROKER_FD_ENV, "3")
            .env(
                tilo_render_service::RENDER_NODE_ENV,
                self.render_node
                    .as_ref()
                    .map(PathBuf::as_os_str)
                    .unwrap_or_default(),
            )
            .stdin(std::process::Stdio::null());
        for variable in STRIPPED_ENV {
            command.env_remove(variable);
        }
        let fd = theirs.as_fd().as_raw_fd();
        // SAFETY: only async-signal-safe calls between fork and exec: the
        // service's socket is placed at descriptor 3, the one inherited
        // descriptor the service keeps (it closes every other one first).
        unsafe {
            command.pre_exec(move || {
                if fd == 3 {
                    if libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                } else if libc::dup2(fd, 3) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command
            .spawn()
            .map_err(|error| format!("start the {key} render service: {error}"))?;
        drop(theirs);
        tracing::info!(%key, pid = child.id(), "render service started");
        let channel = Arc::new(ours);
        let ready = Arc::new((Mutex::new(None), Condvar::new()));
        let child = Arc::new(Mutex::new(child));
        let watch = Watch {
            key: key.to_owned(),
            channel: Arc::clone(&channel),
            ready: Arc::clone(&ready),
            child: Arc::clone(&child),
            started: Instant::now(),
            broker: Arc::downgrade(self),
        };
        std::thread::Builder::new()
            .name("tilo-render-watch".into())
            .spawn(move || watch.run())
            .map_err(|error| format!("watch the {key} render service: {error}"))?;
        Ok(Service {
            channel,
            ready,
            _child: child,
        })
    }

    /// The service for `key` ended: forget it (if it is still the one on
    /// the books), so the next request starts another, and count the death
    /// if it died young.
    fn ended(&self, key: &str, channel: &Arc<Channel>, lived: Duration) {
        {
            let mut services = self.services.lock_recover();
            if services
                .get(key)
                .is_some_and(|service| Arc::ptr_eq(&service.channel, channel))
            {
                services.remove(key);
            }
        }
        let mut crashes = self.crashes.lock_recover();
        let record = crashes.entry(key.to_owned()).or_default();
        record.count = if lived < STABLE_AFTER {
            record.count.saturating_add(1)
        } else {
            0
        };
        self.live.lock_recover().retain(|_, live| live.key != key);
    }
}

/// Watches one service: its ready message, then its end.
struct Watch {
    key: String,
    channel: Arc<Channel>,
    ready: Arc<(Mutex<Option<Result<String, ()>>>, Condvar)>,
    child: Arc<Mutex<std::process::Child>>,
    started: Instant,
    broker: std::sync::Weak<Inner>,
}

impl Watch {
    fn run(self) {
        loop {
            match self.channel.recv::<FromService>() {
                Ok(received) => {
                    let FromService::Ready { device, gpu } = received.message;
                    tracing::info!(key = %self.key, %device, gpu, "render service ready");
                    let (state, signal) = &*self.ready;
                    *state.lock_recover() = Some(Ok(device));
                    signal.notify_all();
                }
                Err(error) => {
                    let status = self.child.lock_recover().wait();
                    tracing::warn!(key = %self.key, %error, ?status, "render service ended");
                    let (state, signal) = &*self.ready;
                    {
                        let mut state = state.lock_recover();
                        if state.is_none() {
                            *state = Some(Err(()));
                        }
                    }
                    signal.notify_all();
                    if let Some(broker) = self.broker.upgrade() {
                        broker.ended(&self.key, &self.channel, self.started.elapsed());
                    }
                    return;
                }
            }
        }
    }
}

/// How long to wait before starting a service again that last started at
/// `record.last_start` and died `record.count` times in a row young.
fn backoff(record: Crashes) -> Duration {
    match record.last_start {
        Some(started) if started.elapsed() < STABLE_AFTER => Duration::from_millis(
            100_u64.saturating_mul(1_u64.checked_shl(record.count.min(6)).unwrap_or(64)),
        )
        .min(Duration::from_secs(5)),
        _ => Duration::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{CapRole, WorkspaceTree, local_user_id};

    /// Decision Q1: a room only its person is in renders in the private
    /// rooms' service; a room anyone else is in, or one this session does
    /// not know, renders in a service of its own.
    #[test]
    fn rooms_render_in_the_private_service_only_while_nobody_else_is_in_them() {
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.adopt_ownerless_workspaces(&local_user_id());
        let room = tree.desktop.workspaces[0].room.to_string();
        assert_eq!(render_key(&tree, Some(&room)), PRIVATE);
        assert_eq!(
            render_key(&tree, None),
            PRIVATE,
            "a launch outside any room"
        );
        tree.desktop.workspaces[0].admit("ada", CapRole::Editor);
        assert_eq!(render_key(&tree, Some(&room)), format!("room:{room}"));
        assert_eq!(render_key(&tree, Some("elsewhere")), "room:elsewhere");
    }

    #[test]
    fn a_service_that_keeps_dying_young_waits_longer_each_time() {
        let young = |count| Crashes {
            count,
            last_start: Some(Instant::now()),
        };
        assert_eq!(
            backoff(Crashes::default()),
            Duration::ZERO,
            "the first start"
        );
        assert!(backoff(young(1)) < backoff(young(3)));
        assert_eq!(backoff(young(30)), Duration::from_secs(5), "bounded");
    }
}
