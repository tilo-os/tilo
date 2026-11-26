//! `tilo-projector serve`: the control socket and the one session it runs.

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use tilo_projector::control::{
    self, CONTROL_SOCKET_NAME, ConnectStage, Credential, ProjectorEvent, ProjectorRequest,
};
use tilo_projector::{ProjectionDevice, ProjectionOptions, ProjectionProtocol};

use crate::discovery::discover;
use crate::error::ProjectorError;
use crate::session::context::{EventSink, SessionContext, SessionControl};
use crate::session::paths::SessionPaths;
use crate::session::{self, Helpers, SESSION_STOP_TIMEOUT};
use tilo_lock::MutexRecover as _;

/// Discovery a client may ask for at most, so one request cannot hold its
/// connection for an unbounded time.
const MAX_DISCOVERY: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Clients {
    next: AtomicU64,
    senders: Mutex<Vec<(u64, mpsc::Sender<ProjectorEvent>)>>,
}

impl Clients {
    fn lock(&self) -> MutexGuard<'_, Vec<(u64, mpsc::Sender<ProjectorEvent>)>> {
        self.senders.lock_recover()
    }

    fn join(
        &self,
    ) -> (
        u64,
        mpsc::Sender<ProjectorEvent>,
        mpsc::Receiver<ProjectorEvent>,
    ) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.lock().push((id, tx.clone()));
        (id, tx, rx)
    }

    fn leave(&self, id: u64) -> usize {
        let mut senders = self.lock();
        senders.retain(|(client, _)| *client != id);
        senders.len()
    }

    fn broadcast(&self, event: &ProjectorEvent) {
        self.lock().retain(|(_, tx)| tx.send(event.clone()).is_ok());
    }
}

struct Session {
    control: Arc<SessionControl>,
    thread: JoinHandle<()>,
}

#[derive(Clone, Copy)]
struct Bounds {
    ready: Option<Duration>,
    stop: Duration,
}

impl Bounds {
    fn ready(self, protocol: ProjectionProtocol) -> Duration {
        self.ready
            .unwrap_or_else(|| session::ready_timeout(protocol))
    }
}

pub(crate) struct Service {
    paths: SessionPaths,
    helpers: Helpers,
    bounds: Bounds,
    clients: Arc<Clients>,
    session: Mutex<Option<Session>>,
}

impl Service {
    pub(crate) fn new(paths: SessionPaths, helpers: Helpers) -> Arc<Self> {
        Self::bounded(
            paths,
            helpers,
            Bounds {
                ready: None,
                stop: SESSION_STOP_TIMEOUT,
            },
        )
    }

    fn bounded(paths: SessionPaths, helpers: Helpers, bounds: Bounds) -> Arc<Self> {
        Arc::new(Self {
            paths,
            helpers,
            bounds,
            clients: Arc::default(),
            session: Mutex::new(None),
        })
    }

    /// Accepts clients until the listener fails.
    pub(crate) fn run(self: &Arc<Self>, listener: &UnixListener) -> Result<(), ProjectorError> {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let service = Arc::clone(self);
                    std::thread::Builder::new()
                        .name("tilo-proj-client".to_owned())
                        .spawn(move || service.serve_client(stream))
                        .map_err(ProjectorError::io("cannot serve a control connection"))?;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    return Err(ProjectorError::io("cannot accept a control connection")(
                        error,
                    ));
                }
            }
        }
        Ok(())
    }

    /// Serves one client until it disconnects. When it was the last client,
    /// the running session stops with it.
    pub(crate) fn serve_client(self: &Arc<Self>, stream: UnixStream) {
        let Ok(mut writer) = stream.try_clone() else {
            return;
        };
        let (id, reply, events) = self.clients.join();
        let writer_thread = std::thread::Builder::new()
            .name("tilo-proj-events".to_owned())
            .spawn(move || {
                while let Ok(event) = events.recv() {
                    if control::write_event(&mut writer, &event).is_err() {
                        break;
                    }
                }
            });
        let mut reader = stream;
        while let Ok(Some(request)) = control::read_request(&mut reader) {
            self.handle(request, &reply);
        }
        if self.clients.leave(id) == 0 {
            let _ = self.stop_session();
        }
        drop(reply);
        if let Ok(writer_thread) = writer_thread {
            let _ = writer_thread.join();
        }
    }

    fn handle(self: &Arc<Self>, request: ProjectorRequest, reply: &mpsc::Sender<ProjectorEvent>) {
        let event = match request {
            ProjectorRequest::Discover { timeout_ms } => {
                match discover(Duration::from_millis(timeout_ms).min(MAX_DISCOVERY)) {
                    Ok(devices) => ProjectorEvent::Devices(devices),
                    Err(error) => ProjectorEvent::Failed(error.to_string()),
                }
            }
            ProjectorRequest::Connect {
                device,
                options,
                credential,
            } => {
                self.connect(device, options, credential);
                return;
            }
            ProjectorRequest::Disconnect => match self.stop_session() {
                Ok(()) => ProjectorEvent::Idle,
                Err(error) => ProjectorEvent::Failed(error.to_string()),
            },
        };
        let _ = reply.send(event);
    }

    fn lock_session(&self) -> MutexGuard<'_, Option<Session>> {
        self.session.lock_recover()
    }

    /// Starts a session to `device`, stopping the running one first. Its
    /// progress, readiness and end reach every client as events.
    pub(crate) fn connect(
        self: &Arc<Self>,
        device: ProjectionDevice,
        options: ProjectionOptions,
        credential: Option<Credential>,
    ) {
        let device_id = device.id.clone();
        // Held across stop and start, so two clients' connects cannot both
        // start a session.
        let mut slot = self.lock_session();
        if let Err(error) = stop(&mut slot, self.bounds.stop) {
            drop(slot);
            self.clients.broadcast(&ProjectorEvent::Ended {
                device_id,
                error: Some(error.to_string()),
            });
            return;
        }
        let control = Arc::new(SessionControl::default());
        let clients = Arc::clone(&self.clients);
        let events: EventSink = Arc::new(move |event| clients.broadcast(&event));
        let context = SessionContext::new(
            self.paths.clone(),
            self.helpers.clone(),
            device_id.clone(),
            Arc::clone(&control),
            Arc::clone(&events),
        );
        context.stage(ConnectStage::Starting);
        let ready_timeout = self.bounds.ready(device.protocol);
        let spawned = std::thread::Builder::new()
            .name("tilo-proj-session".to_owned())
            .spawn(move || run_session(&device, &options, credential, &context));
        match spawned {
            Ok(thread) => {
                let watchdog = Arc::clone(&control);
                // A session that is not showing within the ready timeout is
                // stopped, and ends with that as its error.
                let _ = std::thread::Builder::new()
                    .name("tilo-proj-ready".to_owned())
                    .spawn(move || {
                        watchdog.expire_unless_ready(
                            ready_timeout,
                            "projection session did not become ready in time",
                        );
                    });
                *slot = Some(Session { control, thread });
            }
            Err(error) => {
                drop(slot);
                events(ProjectorEvent::Ended {
                    device_id,
                    error: Some(format!("cannot start projection session: {error}")),
                });
            }
        }
    }

    /// Stops the running session, if any, within the disconnect bound.
    ///
    /// # Errors
    ///
    /// [`ProjectorError::Timeout`] if the session did not stop in time.
    pub(crate) fn stop_session(&self) -> Result<(), ProjectorError> {
        stop(&mut self.lock_session(), self.bounds.stop)
    }
}

fn stop(slot: &mut Option<Session>, within: Duration) -> Result<(), ProjectorError> {
    let Some(session) = slot.take() else {
        return Ok(());
    };
    session.control.request_stop(None);
    if session.control.wait_finished(within) {
        let _ = session.thread.join();
        Ok(())
    } else {
        *slot = Some(session);
        Err(ProjectorError::Timeout(
            "projection session did not stop cleanly",
        ))
    }
}

/// The body of a session thread: run it, clean the runtime directory and
/// report how it ended.
fn run_session(
    device: &ProjectionDevice,
    options: &ProjectionOptions,
    credential: Option<Credential>,
    context: &SessionContext,
) {
    let error = match session::run(device, options, credential, context) {
        Err(error) => Some(error.to_string()),
        Ok(()) => context.control().stop_reason().map(str::to_owned),
    };
    context.paths.clear();
    // Finished before the event: whoever stops a session joins its thread,
    // which still delivers `Ended` before that stop is answered.
    context.control().mark_finished();
    context.end(error);
}

/// The listener systemd passed in, or a fresh one at `socket` (by default
/// the control socket in the runtime directory).
pub(crate) fn listen(
    paths: &SessionPaths,
    socket: Option<PathBuf>,
) -> Result<UnixListener, ProjectorError> {
    let socket = socket.unwrap_or_else(|| paths.root.join(CONTROL_SOCKET_NAME));
    // SAFETY: taken as `serve` starts, before any thread is spawned.
    if let Some(listener) = unsafe { tilo_files::native::private::activated_socket(&socket) }
        .map_err(ProjectorError::io("cannot take systemd's control socket"))?
    {
        return Ok(listener);
    }
    if tilo_files::native::socket::connect(&socket).is_ok() {
        return Err(ProjectorError::Unavailable(format!(
            "a projector already serves {}",
            socket.display()
        )));
    }
    tilo_files::native::private::bind_socket(&socket)
        .map_err(ProjectorError::io("cannot bind the control socket"))
}
