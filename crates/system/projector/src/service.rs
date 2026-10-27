//! `tilo-projector serve`: the control socket and the one session it runs.
//!
//! Each client connection is served on its own thread, in request order, and
//! has a writer thread that delivers its replies and every session event. The
//! service holds at most one session: a `Connect` stops the running one
//! first, and when the last client disconnects (the shell exited or crashed)
//! the session is stopped too, so nothing keeps streaming the desktop
//! unsupervised.

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

/// Every connected client and the channel its writer thread drains.
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

    /// Removes a client and returns how many remain.
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

/// How long a session may take to become ready and to stop.
#[derive(Clone, Copy)]
struct Bounds {
    /// Overrides [`session::ready_timeout`] for every protocol.
    ready: Option<Duration>,
    stop: Duration,
}

impl Bounds {
    fn ready(self, protocol: ProjectionProtocol) -> Duration {
        self.ready
            .unwrap_or_else(|| session::ready_timeout(protocol))
    }
}

/// The control service: its clients and the session it supervises.
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
        // The writer ends once the last sender of its channel is gone.
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

    #[cfg(test)]
    fn session_count(&self) -> usize {
        usize::from(
            self.lock_session()
                .as_ref()
                .is_some_and(|session| !session.control.finished()),
        )
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
        // Keep the handle: a later stop can still reap it.
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
    if UnixStream::connect(&socket).is_ok() {
        return Err(ProjectorError::Unavailable(format!(
            "a projector already serves {}",
            socket.display()
        )));
    }
    tilo_files::native::private::bind_socket(&socket)
        .map_err(ProjectorError::io("cannot bind the control socket"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use tilo_projector::ProjectionMode;

    struct Fixture {
        root: PathBuf,
        service: Arc<Service>,
    }

    impl Fixture {
        /// A service whose `AirPlay` sender is a script that reports the
        /// stream started after `delay` seconds, then runs until terminated.
        fn new(name: &str, delay: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "tilo-projector-service-{name}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            let paths = SessionPaths::for_root(root.clone()).expect("session paths");
            let sender = root.join("airplay-sender");
            fs::write(
                &sender,
                format!(
                    "#!/bin/sh\ntrap 'exit 0' TERM INT\nsleep {delay}\necho 'mirror session ready'\nwhile :; do sleep 1; done\n"
                ),
            )
            .expect("write sender");
            fs::set_permissions(&sender, fs::Permissions::from_mode(0o700))
                .expect("make sender executable");
            let helpers = Helpers {
                doubletake: sender.display().to_string(),
            };
            // Readiness and stopping are bounded only against a hang: the
            // sender script answers at once, however loaded the machine.
            let bounds = Bounds {
                ready: Some(tilo_test_env::hang_bound()),
                stop: tilo_test_env::hang_bound(),
            };
            Self {
                root,
                service: Service::bounded(paths, helpers, bounds),
            }
        }

        /// A subscribed client: the service serves the far end of a socket
        /// pair, and a round trip proves the subscription is in place before
        /// the test goes on.
        fn client(&self) -> (UnixStream, JoinHandle<()>) {
            let (mut client, server) = UnixStream::pair().expect("socket pair");
            let service = Arc::clone(&self.service);
            let thread = std::thread::spawn(move || service.serve_client(server));
            send(&mut client, &ProjectorRequest::Disconnect);
            events_until(&mut client, |event| event == &ProjectorEvent::Idle);
            (client, thread)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.service.stop_session();
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn airplay(id: &str) -> (ProjectionDevice, ProjectionOptions) {
        (
            ProjectionDevice {
                id: id.to_owned(),
                name: id.to_owned(),
                protocol: ProjectionProtocol::AirPlay,
                detail: "Apple TV".to_owned(),
                modes: vec![ProjectionMode::Mirror],
                addresses: vec!["192.0.2.7".to_owned()],
                port: 7000,
            },
            ProjectionOptions {
                audio: false,
                ..ProjectionOptions::default()
            },
        )
    }

    fn send(client: &mut UnixStream, request: &ProjectorRequest) {
        control::write_request(client, request).expect("write request");
    }

    fn connect(client: &mut UnixStream, device: &ProjectionDevice, options: &ProjectionOptions) {
        send(
            client,
            &ProjectorRequest::Connect {
                device: device.clone(),
                options: options.clone(),
                credential: None,
            },
        );
    }

    /// Reads events until `last` matches, returning all of them in order.
    fn events_until(
        client: &mut UnixStream,
        last: impl Fn(&ProjectorEvent) -> bool,
    ) -> Vec<ProjectorEvent> {
        client
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .expect("read timeout");
        let mut seen = Vec::new();
        loop {
            let event = control::read_event(client)
                .expect("read event")
                .expect("service closed the connection");
            let done = last(&event);
            seen.push(event);
            if done {
                return seen;
            }
        }
    }

    fn is_connected(event: &ProjectorEvent) -> bool {
        matches!(event, ProjectorEvent::Connected { .. })
    }

    fn runtime_files(root: &Path) -> Vec<String> {
        fs::read_dir(root)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn service_connect_reports_progress_then_connected() {
        let fixture = Fixture::new("progress", "0.2");
        let (device, options) = airplay("AirPlay:a");
        let (mut client, served) = fixture.client();
        connect(&mut client, &device, &options);
        let events = events_until(&mut client, |event| {
            is_connected(event) || matches!(event, ProjectorEvent::Ended { .. })
        });
        assert_eq!(
            events,
            [
                ProjectorEvent::Connecting {
                    device_id: device.id.clone(),
                    stage: ConnectStage::Starting
                },
                ProjectorEvent::Connecting {
                    device_id: device.id.clone(),
                    stage: ConnectStage::Handshake
                },
                ProjectorEvent::Connected {
                    device_id: device.id.clone()
                },
            ]
        );
        drop(client);
        served.join().expect("client thread");
    }

    #[test]
    fn stop_is_observed_without_polling_files() {
        let fixture = Fixture::new("stop", "0");
        let (device, options) = airplay("AirPlay:stop");
        let (mut client, served) = fixture.client();
        connect(&mut client, &device, &options);
        events_until(&mut client, is_connected);
        let during = runtime_files(&fixture.root);
        send(&mut client, &ProjectorRequest::Disconnect);
        let events = events_until(&mut client, |event| event == &ProjectorEvent::Idle);
        assert_eq!(
            events,
            [
                ProjectorEvent::Ended {
                    device_id: device.id.clone(),
                    error: None
                },
                ProjectorEvent::Idle
            ]
        );
        let after = runtime_files(&fixture.root);
        for marker in [
            "stop",
            "ready",
            "error",
            "session.pid",
            "capture-profile.json",
        ] {
            assert!(
                !during.iter().chain(&after).any(|file| file == marker),
                "a session wrote the {marker} marker: {during:?} {after:?}"
            );
        }
        assert_eq!(fixture.service.session_count(), 0);
        drop(client);
        served.join().expect("client thread");
    }

    #[test]
    fn second_connect_stops_the_first_session() {
        let fixture = Fixture::new("second", "0");
        let (first, options) = airplay("AirPlay:first");
        let (second, _) = airplay("AirPlay:second");
        let (mut client, served) = fixture.client();
        connect(&mut client, &first, &options);
        events_until(&mut client, is_connected);
        connect(&mut client, &second, &options);
        let events = events_until(
            &mut client,
            |event| matches!(event, ProjectorEvent::Connected { device_id } if *device_id == second.id),
        );
        let first_ended = events.iter().position(|event| {
            event
                == &ProjectorEvent::Ended {
                    device_id: first.id.clone(),
                    error: None,
                }
        });
        assert!(
            first_ended.is_some_and(|index| index + 1 < events.len()),
            "the first session ends before the second connects: {events:?}"
        );
        assert_eq!(fixture.service.session_count(), 1);
        drop(client);
        served.join().expect("client thread");
    }

    #[test]
    fn subscriber_drop_stops_session() {
        let fixture = Fixture::new("drop", "0");
        let (device, options) = airplay("AirPlay:dropped");
        let (mut owner, owner_served) = fixture.client();
        let (mut observer, observer_served) = fixture.client();
        connect(&mut owner, &device, &options);
        events_until(&mut observer, is_connected);
        drop(owner);
        owner_served.join().expect("owner thread");
        assert_eq!(
            fixture.service.session_count(),
            1,
            "another client still watches the session"
        );
        drop(observer);
        observer_served.join().expect("observer thread");
        assert_eq!(
            fixture.service.session_count(),
            0,
            "the last client leaving stops the session"
        );
    }

    #[test]
    fn a_failed_sender_ends_the_session_with_its_diagnostics() {
        let fixture = Fixture::new("failing", "0");
        fs::write(
            fixture.root.join("airplay-sender"),
            b"#!/bin/sh\necho 'peer refused AirPlay session' >&2\nexit 3\n",
        )
        .expect("write failing sender");
        let (device, options) = airplay("AirPlay:failing");
        let (mut client, served) = fixture.client();
        connect(&mut client, &device, &options);
        let events = events_until(&mut client, |event| {
            matches!(event, ProjectorEvent::Ended { .. })
        });
        assert!(
            matches!(
                events.last(),
                Some(ProjectorEvent::Ended { error: Some(error), .. })
                    if error.contains("peer refused AirPlay session")
            ),
            "{events:?}"
        );
        drop(client);
        served.join().expect("client thread");
        assert_eq!(fixture.service.session_count(), 0);
    }
}
