//! Wireless displays: what the projector service reports, and what the sheet
//! asks of it.
//!
//! The sheet is the picker and its lifecycle; the transport broker owns the
//! protocol details. Keeping that boundary explicit matters here: Miracast,
//! Google Cast, `AirPlay` and `FCast` have different discovery and
//! authentication handshakes, but the sheet must not grow four subtly
//! different connection flows.
//!
//! The production integration point is the `tilo-projector` service's control
//! socket ([`tilo_projector::control`]): typed requests in, session events out,
//! over one connection the session holds. That connection is also the
//! broker's reason to keep a session going: when the session exits or
//! crashes, the broker stops projecting. Tests replace it with an in-memory
//! backend, so every state transition is exercised without pretending a
//! receiver exists.

use std::fmt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_lock::MutexRecover as _;
use tilo_projector::control::{self, ConnectStage, Credential, ProjectorEvent, ProjectorRequest};
pub use tilo_projector::{CaptureProfile, ProjectionDevice, ProjectionMode, ProjectionOptions};

use crate::i18n::t;

/// The socket the projector reads the desktop's video from, beside its
/// control socket; a broker of the person's own says so in the environment.
const VIDEO_SOCKET_ENV: &str = "TILO_PROJECTOR_VIDEO_SOCKET";
const VIDEO_SOCKET_NAME: &str = "video.sock";

/// Where the projector reads the desktop's frames, for the sheet speaking on
/// `control` (`None`: the service's own control socket).
#[must_use]
pub(crate) fn video_socket(control: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(VIDEO_SOCKET_ENV) {
        return Some(PathBuf::from(path));
    }
    let control = control
        .map(Path::to_path_buf)
        .or_else(control::control_socket)?;
    control.parent().map(|dir| dir.join(VIDEO_SOCKET_NAME))
}

/// How long a discovery listens for receivers.
const DISCOVERY_WINDOW_MS: u64 = 1_500;
/// Replies may take their own bound plus this much: the service's 15 s ready
/// timeout and 3 s stop bound for a connect, the 3 s stop bound for a
/// disconnect.
const DISCOVER_REPLY_TIMEOUT: Duration = Duration::from_millis(6_500);
const CONNECT_REPLY_TIMEOUT: Duration = Duration::from_secs(25);
const DISCONNECT_REPLY_TIMEOUT: Duration = Duration::from_secs(8);

/// What the sheet shows of the projector: looking, connecting, streaming, or
/// what went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectionStatus {
    Idle,
    Scanning,
    Connecting { device_id: String },
    Connected { device_id: String },
    Failed(ProjectionFailure),
}

/// What went wrong, as the person is told it: an outcome and a next step.
/// The service's own words (socket errors, `os error` text, a receiver's
/// diagnostic) go to the log, never to the sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionFailure {
    /// The wireless display service isn't running or doesn't answer.
    Unavailable,
    /// Looking for receivers failed.
    SearchFailed,
    /// The receiver refused the connection or never answered it.
    ConnectFailed,
    /// A running stream ended without being asked to.
    Lost,
    /// Stopping the stream failed.
    StopFailed,
}

impl ProjectionFailure {
    /// The outcome, in one line.
    pub fn title(self) -> String {
        t(match self {
            Self::Unavailable => "wireless-unavailable",
            Self::SearchFailed => "wireless-search-failed",
            Self::ConnectFailed => "wireless-connect-failed",
            Self::Lost => "wireless-lost",
            Self::StopFailed => "wireless-stop-failed",
        })
    }

    /// What to do about it.
    pub fn next_step(self) -> String {
        t(match self {
            Self::Unavailable => "wireless-unavailable-detail",
            Self::SearchFailed => "wireless-search-failed-detail",
            Self::ConnectFailed => "wireless-connect-failed-detail",
            Self::Lost => "wireless-lost-detail",
            Self::StopFailed => "wireless-stop-failed-detail",
        })
    }

    /// Record the service's own account of `self` for whoever reads the
    /// log, and hand back the outcome the person is shown.
    fn logged(self, detail: impl fmt::Display) -> Self {
        tracing::warn!(failure = ?self, %detail, "projection");
        self
    }
}

/// Blocking protocol implementation. The controller always calls it on its
/// worker thread, never on the loop.
pub trait ProjectionBackend: Send + 'static {
    /// # Errors
    ///
    /// [`ProjectionFailure::Unavailable`] when the projection service is
    /// unreachable, [`ProjectionFailure::SearchFailed`] when it could not
    /// look for receivers.
    fn discover(&mut self) -> Result<Vec<ProjectionDevice>, ProjectionFailure>;

    /// # Errors
    ///
    /// [`ProjectionFailure::ConnectFailed`] if `device` doesn't support
    /// `options.mode` or the service or receiver refuses or fails the
    /// connection; [`ProjectionFailure::Unavailable`] without a service.
    fn connect(
        &mut self,
        device: &ProjectionDevice,
        options: ProjectionOptions,
    ) -> Result<(), ProjectionFailure>;

    /// # Errors
    ///
    /// [`ProjectionFailure::StopFailed`] if the service refuses or fails the
    /// disconnect; [`ProjectionFailure::Unavailable`] without a service.
    fn disconnect(&mut self, device: &ProjectionDevice) -> Result<(), ProjectionFailure>;

    /// Gives the backend somewhere to report session changes nobody asked
    /// for. Backends without a live service (tests) have none to report.
    fn set_notice_sink(&mut self, _sink: NoticeSink) {}

    /// Gives the backend somewhere to report the profile the projector asks
    /// the desktop's frames at, as the receiver and the link allow more or
    /// less. Backends without a live service (tests) ask for none.
    fn set_profile_sink(&mut self, _sink: ProfileSink) {}
}

/// Where a backend delivers the [`CaptureProfile`]s the projector asks for.
pub type ProfileSink = Arc<dyn Fn(CaptureProfile) + Send + Sync>;

/// A session change the backend learns of without being asked: the receiver
/// went away, or the projection service itself did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectionNotice {
    Ended {
        device_id: String,
        error: Option<String>,
    },
    ServiceLost,
}

/// Where a backend delivers [`ProjectionNotice`]s.
pub type NoticeSink = Arc<dyn Fn(ProjectionNotice) + Send + Sync>;

/// One connection to the broker: requests go out on the writer, and a thread
/// reads the events back so a session that ended is reported without the
/// sheet asking.
struct ControlLink {
    writer: UnixStream,
    events: Receiver<ProjectorEvent>,
}

/// The production adapter for the projection broker the OS image installs.
/// The session never grants itself raw `PipeWire`, Wi-Fi Direct or mDNS
/// access just to fill this sheet in.
///
/// It holds one connection to the broker's control socket for as long as the
/// session runs; the socket is activated on the first request, so the session
/// starts nothing beside the ones it already hosts.
pub struct SystemProjectionBackend {
    socket: Option<PathBuf>,
    link: Option<ControlLink>,
    notices: Option<NoticeSink>,
    profiles: Option<ProfileSink>,
}

impl fmt::Debug for SystemProjectionBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SystemProjectionBackend")
            .field("socket", &self.socket)
            .field("connected", &self.link.is_some())
            .finish_non_exhaustive()
    }
}

impl SystemProjectionBackend {
    /// The backend speaking to the service on `socket`; `None` is the
    /// service's own.
    #[must_use]
    pub fn new(socket: Option<PathBuf>) -> Self {
        Self {
            socket: socket.or_else(control::control_socket),
            link: None,
            notices: None,
            profiles: None,
        }
    }

    fn unavailable(error: impl fmt::Display) -> ProjectionFailure {
        ProjectionFailure::Unavailable.logged(error)
    }

    fn open(&self) -> Result<ControlLink, ProjectionFailure> {
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| Self::unavailable("XDG_RUNTIME_DIR is unavailable"))?;
        let writer = tilo_files::native::socket::connect(socket).map_err(Self::unavailable)?;
        let mut reader = writer.try_clone().map_err(Self::unavailable)?;
        let (tx, events) = mpsc::channel();
        let notices = self.notices.clone();
        let profiles = self.profiles.clone();
        std::thread::Builder::new()
            .name("tilo-projection-events".into())
            .spawn(move || {
                while let Ok(Some(event)) = control::read_event(&mut reader) {
                    match event {
                        ProjectorEvent::CaptureProfile(profile) => {
                            // Nobody waited for this one: the frames of the
                            // desktop are sent at it from now on.
                            if let Some(profiles) = profiles.as_ref() {
                                profiles(profile);
                            }
                        }
                        ProjectorEvent::Ended { device_id, error } => {
                            if let Some(notices) = notices.as_ref() {
                                notices(ProjectionNotice::Ended {
                                    device_id: device_id.clone(),
                                    error: error.clone(),
                                });
                            }
                            let _ = tx.send(ProjectorEvent::Ended { device_id, error });
                        }
                        event => {
                            let _ = tx.send(event);
                        }
                    }
                }
                if let Some(notices) = notices.as_ref() {
                    notices(ProjectionNotice::ServiceLost);
                }
            })
            .map_err(Self::unavailable)?;
        Ok(ControlLink { writer, events })
    }

    /// Sends `request` on the shared connection, opening (or reopening, after
    /// the broker restarted) it as needed, and returns the event stream to
    /// read the reply from. Events nobody waited for are dropped first.
    fn send(
        &mut self,
        request: &ProjectorRequest,
    ) -> Result<&Receiver<ProjectorEvent>, ProjectionFailure> {
        let mut last_error = None;
        for _ in 0..2 {
            let mut link = match self.link.take() {
                Some(link) => link,
                None => self.open()?,
            };
            let open = loop {
                match link.events.try_recv() {
                    Ok(_) => {}
                    Err(TryRecvError::Empty) => break true,
                    Err(TryRecvError::Disconnected) => break false,
                }
            };
            if !open {
                continue;
            }
            match control::write_request(&mut link.writer, request) {
                Ok(()) => return Ok(&self.link.insert(link).events),
                Err(error) => last_error = Some(error.to_string()),
            }
        }
        Err(Self::unavailable(
            last_error.unwrap_or_else(|| "the connection closed".to_owned()),
        ))
    }

    /// Reads events until `reply` turns one into a result, or `timeout`
    /// passes.
    fn reply<T>(
        &mut self,
        request: &ProjectorRequest,
        timeout: Duration,
        mut answer: impl FnMut(ProjectorEvent) -> Option<Result<T, ProjectionFailure>>,
    ) -> Result<T, ProjectionFailure> {
        let deadline = Instant::now().checked_add(timeout);
        let events = self.send(request)?;
        loop {
            let remaining = deadline.map_or(timeout, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            });
            match events.recv_timeout(remaining) {
                Ok(event) => {
                    if let Some(result) = answer(event) {
                        return result;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(Self::unavailable("no reply in time"));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Self::unavailable("the projection service stopped"));
                }
            }
        }
    }
}

impl ProjectionBackend for SystemProjectionBackend {
    fn discover(&mut self) -> Result<Vec<ProjectionDevice>, ProjectionFailure> {
        let mut devices = self.reply(
            &ProjectorRequest::Discover {
                timeout_ms: DISCOVERY_WINDOW_MS,
            },
            DISCOVER_REPLY_TIMEOUT,
            |event| match event {
                ProjectorEvent::Devices(devices) => Some(Ok(devices)),
                ProjectorEvent::Failed(error) => {
                    Some(Err(ProjectionFailure::SearchFailed.logged(error)))
                }
                _ => None,
            },
        )?;
        // A receiver advertised on two interfaces must be one row. Sorting also
        // makes refreshes stable instead of visually shuffling the sheet.
        devices.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.protocol.label().cmp(b.protocol.label()))
        });
        devices.dedup_by(|a, b| a.id == b.id && a.protocol == b.protocol);
        Ok(devices)
    }

    fn connect(
        &mut self,
        device: &ProjectionDevice,
        mut options: ProjectionOptions,
    ) -> Result<(), ProjectionFailure> {
        if !device.supports(options.mode) {
            return Err(ProjectionFailure::ConnectFailed.logged(format_args!(
                "{} does not take the {} layout",
                device.name,
                options.mode.label()
            )));
        }
        // The secret crosses the socket in the request itself, never in an
        // environment or argument, and is wiped once sent.
        let credential = options.credential.take().map(Credential::new);
        let request = ProjectorRequest::Connect {
            device: device.clone(),
            options,
            credential,
        };
        // A running session to the same receiver ends before this one
        // starts; only events after this session's start answer it.
        let mut started = false;
        self.reply(&request, CONNECT_REPLY_TIMEOUT, |event| match event {
            ProjectorEvent::Connecting { device_id, stage } if device_id == device.id => {
                started |= stage == ConnectStage::Starting;
                None
            }
            ProjectorEvent::Connected { device_id } if started && device_id == device.id => {
                Some(Ok(()))
            }
            ProjectorEvent::Ended { device_id, error } if started && device_id == device.id => {
                Some(Err(ProjectionFailure::ConnectFailed.logged(
                    error
                        .as_deref()
                        .unwrap_or("the session ended while starting"),
                )))
            }
            ProjectorEvent::Failed(error) => {
                Some(Err(ProjectionFailure::ConnectFailed.logged(error)))
            }
            _ => None,
        })
    }

    fn disconnect(&mut self, _device: &ProjectionDevice) -> Result<(), ProjectionFailure> {
        self.reply(
            &ProjectorRequest::Disconnect,
            DISCONNECT_REPLY_TIMEOUT,
            |event| match event {
                ProjectorEvent::Idle => Some(Ok(())),
                ProjectorEvent::Failed(error) => {
                    Some(Err(ProjectionFailure::StopFailed.logged(error)))
                }
                _ => None,
            },
        )
    }

    fn set_notice_sink(&mut self, sink: NoticeSink) {
        self.notices = Some(sink);
        // A connection opened before this would not report to the sink.
        self.link = None;
    }

    fn set_profile_sink(&mut self, sink: ProfileSink) {
        self.profiles = Some(sink);
        // A connection opened before this would not report to the sink.
        self.link = None;
    }
}

enum WorkerCommand {
    Discover,
    Connect(ProjectionDevice, ProjectionOptions),
    Disconnect(ProjectionDevice),
}

enum WorkerEvent {
    Discovered(Result<Vec<ProjectionDevice>, ProjectionFailure>),
    Connected(String, Result<(), ProjectionFailure>),
    Disconnected(String, Result<(), ProjectionFailure>),
    Notice(ProjectionNotice),
}

/// What the sheet reads of the projector without waiting on it: the receivers
/// found, how the session with one stands, and the options a stream starts
/// with.
pub struct ProjectionController {
    tx: Option<Sender<WorkerCommand>>,
    rx: Receiver<WorkerEvent>,
    pub devices: Vec<ProjectionDevice>,
    pub status: ProjectionStatus,
    pub options: ProjectionOptions,
    pub selected: usize,
    /// The profile the projector last asked the desktop's frames at; `None`
    /// until it says, and for every new session until it does.
    profile: Arc<Mutex<Option<CaptureProfile>>>,
}

impl fmt::Debug for ProjectionController {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProjectionController")
            .field("devices", &self.devices)
            .field("status", &self.status)
            .field("options", &self.options)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl ProjectionController {
    /// A controller whose requests the worker runs against `backend`, waking
    /// the session's loop with `wake` when something came back.
    #[must_use]
    pub fn new(backend: Box<dyn ProjectionBackend>, wake: &tilo_service::Wake) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let noticed = event_tx.clone();
        let woken = Arc::clone(wake);
        let notice_wake = Arc::clone(wake);
        let mut backend = backend;
        backend.set_notice_sink(Arc::new(move |notice| {
            if event_tx.send(WorkerEvent::Notice(notice)).is_ok() {
                notice_wake();
            }
        }));
        let profile: Arc<Mutex<Option<CaptureProfile>>> = Arc::default();
        let asked = Arc::clone(&profile);
        backend.set_profile_sink(Arc::new(move |sent| {
            *asked.lock_recover() = Some(sent);
        }));
        let spawned = std::thread::Builder::new()
            .name("tilo-projection".into())
            .spawn(move || {
                while let Ok(command) = command_rx.recv() {
                    let event = match command {
                        WorkerCommand::Discover => WorkerEvent::Discovered(backend.discover()),
                        WorkerCommand::Connect(device, options) => {
                            let id = device.id.clone();
                            WorkerEvent::Connected(id, backend.connect(&device, options))
                        }
                        WorkerCommand::Disconnect(device) => {
                            let id = device.id.clone();
                            WorkerEvent::Disconnected(id, backend.disconnect(&device))
                        }
                    };
                    if noticed.send(event).is_err() {
                        break;
                    }
                    woken();
                }
            });
        if let Err(err) = spawned {
            // Every caller of `self.tx.send` already treats a closed
            // channel as "service stopped" and reports
            // [`ProjectionStatus::Failed`] (see `refresh`/`connect_selected`/
            // `disconnect` below); a worker that never started degrades
            // through that same path instead of panicking this
            // panic=abort process. Dropping the closure here drops
            // `command_rx`/`event_tx` with it, which is what makes the
            // channels look "closed" to every method below.
            tracing::error!("projection: worker thread failed to start: {err}");
        }
        Self {
            tx: Some(command_tx),
            rx: event_rx,
            devices: Vec::new(),
            status: ProjectionStatus::Idle,
            options: ProjectionOptions::default(),
            selected: 0,
            profile,
        }
    }

    /// The profile the projector asks the desktop's frames at, once it said:
    /// the receiver's and the link's own ceiling.
    #[must_use]
    pub fn profile(&self) -> Option<CaptureProfile> {
        *self.profile.lock_recover()
    }

    /// The channel to the worker, once there is one: without a worker the
    /// sheet says the service is unavailable.
    fn available_sender(&mut self) -> Option<&Sender<WorkerCommand>> {
        if self.tx.is_none() {
            self.status = ProjectionStatus::Failed(ProjectionFailure::Unavailable);
        }
        self.tx.as_ref()
    }

    /// Look for receivers again, as the sheet does whenever it opens.
    pub fn refresh(&mut self) {
        let Some(tx) = self.available_sender() else {
            return;
        };
        if tx.send(WorkerCommand::Discover).is_ok() {
            self.status = ProjectionStatus::Scanning;
        } else {
            self.status = ProjectionStatus::Failed(ProjectionFailure::Unavailable);
        }
    }

    /// Stream to the selected receiver.
    pub fn connect_selected(&mut self) {
        if self.available_sender().is_none() {
            return;
        }
        let Some(device) = self.devices.get(self.selected).cloned() else {
            return;
        };
        if !device.supports(self.options.mode) {
            self.options.mode = ProjectionMode::Mirror;
        }
        if self.tx.as_ref().is_some_and(|tx| {
            tx.send(WorkerCommand::Connect(device.clone(), self.options.clone()))
                .is_ok()
        }) {
            self.status = ProjectionStatus::Connecting {
                device_id: device.id,
            };
            // A new session asks its own profile of the frames; the one the
            // last receiver asked for says nothing about this one.
            *self.profile.lock_recover() = None;
            // Do not retain a receiver password in session memory after it
            // has crossed into the broker worker.
            self.options.credential = None;
        }
    }

    /// Stop streaming.
    pub fn disconnect(&mut self) {
        if self.available_sender().is_none() {
            return;
        }
        let ProjectionStatus::Connected { device_id } = &self.status else {
            return;
        };
        let Some(device) = self.devices.iter().find(|d| &d.id == device_id).cloned() else {
            self.status = ProjectionStatus::Idle;
            return;
        };
        if self
            .tx
            .as_ref()
            .is_some_and(|tx| tx.send(WorkerCommand::Disconnect(device)).is_ok())
        {
            self.status = ProjectionStatus::Connecting {
                device_id: device_id.clone(),
            };
        }
    }

    /// Drain everything the worker answered since the last pass. Returns
    /// whether what the sheet shows changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok(WorkerEvent::Discovered(result)) => {
                    match result {
                        Ok(devices) => {
                            self.devices = devices;
                            self.selected = self.selected.min(self.devices.len().saturating_sub(1));
                            self.status = ProjectionStatus::Idle;
                        }
                        Err(failure) => self.status = ProjectionStatus::Failed(failure),
                    }
                    changed = true;
                }
                Ok(WorkerEvent::Connected(device_id, result)) => {
                    self.status = match result {
                        Ok(()) => ProjectionStatus::Connected { device_id },
                        Err(failure) => ProjectionStatus::Failed(failure),
                    };
                    changed = true;
                }
                Ok(WorkerEvent::Disconnected(_device_id, result)) => {
                    self.status = match result {
                        Ok(()) => ProjectionStatus::Idle,
                        Err(failure) => ProjectionStatus::Failed(failure),
                    };
                    changed = true;
                }
                Ok(WorkerEvent::Notice(notice)) => {
                    if let Some(status) = self.status_after(notice) {
                        self.status = status;
                        changed = true;
                    }
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        changed
    }

    /// What an unrequested session change means for the sheet: only the
    /// session it shows as connected can end under it. A connect or
    /// disconnect in flight reports its own outcome.
    fn status_after(&self, notice: ProjectionNotice) -> Option<ProjectionStatus> {
        let ProjectionStatus::Connected { device_id: current } = &self.status else {
            return None;
        };
        match notice {
            ProjectionNotice::Ended { device_id, error } => (device_id == *current).then(|| {
                error.map_or(ProjectionStatus::Idle, |error| {
                    ProjectionStatus::Failed(ProjectionFailure::Lost.logged(error))
                })
            }),
            ProjectionNotice::ServiceLost => Some(ProjectionStatus::Failed(
                ProjectionFailure::Lost.logged("the projection service stopped"),
            )),
        }
    }

    /// Expose unsupported layout choices as disabled instead of accepting a
    /// choice that the selected receiver cannot honour.
    pub fn selected_device_supports(&self, mode: ProjectionMode) -> bool {
        self.devices
            .get(self.selected)
            .is_some_and(|device| device.supports(mode))
            || (self.devices.is_empty() && mode == ProjectionMode::Mirror)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Instant;
    use tilo_projector::ProjectionProtocol;

    #[derive(Default)]
    struct FakeBackend {
        devices: Vec<ProjectionDevice>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl ProjectionBackend for FakeBackend {
        fn discover(&mut self) -> Result<Vec<ProjectionDevice>, ProjectionFailure> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push("discover".into());
            Ok(self.devices.clone())
        }
        fn connect(
            &mut self,
            d: &ProjectionDevice,
            o: ProjectionOptions,
        ) -> Result<(), ProjectionFailure> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(format!(
                    "connect:{}:{}:credential={}",
                    d.protocol.label(),
                    o.mode.label(),
                    o.credential.is_some()
                ));
            Ok(())
        }
        fn disconnect(&mut self, d: &ProjectionDevice) -> Result<(), ProjectionFailure> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(format!("stop:{}", d.id));
            Ok(())
        }
    }

    fn drive(backend: FakeBackend) -> (ProjectionController, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::clone(&backend.calls);
        (
            {
                let wake: tilo_service::Wake = Arc::new(|| ());
                ProjectionController::new(Box::new(backend), &wake)
            },
            calls,
        )
    }

    fn device(protocol: ProjectionProtocol) -> ProjectionDevice {
        ProjectionDevice {
            id: format!("{protocol:?}"),
            name: format!("{} display", protocol.label()),
            protocol,
            detail: "Test room".into(),
            modes: ProjectionMode::ALL.to_vec(),
            addresses: vec!["192.0.2.10".into()],
            port: 1234,
        }
    }

    fn settle(controller: &mut ProjectionController) {
        let deadline = tilo_test_env::hang_deadline();
        while !controller.poll() && Instant::now() < deadline {
            std::thread::yield_now();
        }
    }

    fn calls(calls: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[test]
    fn every_protocol_crosses_the_one_controller_and_unsupported_layouts_are_disabled() {
        let backend = FakeBackend {
            devices: ProjectionProtocol::ALL.into_iter().map(device).collect(),
            calls: Arc::default(),
        };
        let (mut controller, log) = drive(backend);
        controller.refresh();
        settle(&mut controller);
        assert_eq!(controller.devices.len(), 4);

        for index in 0..controller.devices.len() {
            controller.selected = index;
            controller.connect_selected();
            settle(&mut controller);
            assert!(matches!(
                controller.status,
                ProjectionStatus::Connected { .. }
            ));
            controller.disconnect();
            settle(&mut controller);
            assert_eq!(controller.status, ProjectionStatus::Idle);
        }
        let log = calls(&log);
        for protocol in ProjectionProtocol::ALL {
            assert!(
                log.iter().any(|line| line.contains(protocol.label())),
                "{protocol:?} crosses the same controller"
            );
        }

        let only_mirror = FakeBackend {
            devices: vec![ProjectionDevice {
                modes: vec![ProjectionMode::Mirror],
                ..device(ProjectionProtocol::Chromecast)
            }],
            calls: Arc::default(),
        };
        let (mut controller, _) = drive(only_mirror);
        controller.refresh();
        settle(&mut controller);
        assert!(controller.selected_device_supports(ProjectionMode::Mirror));
        assert!(!controller.selected_device_supports(ProjectionMode::Extend));
        // A receiver that cannot spread the desktop is streamed to in
        // duplicate, which every one of them can do.
        controller.options.mode = ProjectionMode::Extend;
        controller.connect_selected();
        settle(&mut controller);
        assert_eq!(controller.options.mode, ProjectionMode::Mirror);
    }

    #[test]
    fn a_pin_crosses_once_and_is_forgotten_and_a_session_that_ends_unasked_is_reported() {
        let backend = FakeBackend {
            devices: vec![device(ProjectionProtocol::AirPlay)],
            calls: Arc::default(),
        };
        let (mut controller, log) = drive(backend);
        controller.refresh();
        settle(&mut controller);
        controller.options.credential = Some("4821".to_owned());
        controller.connect_selected();
        assert_eq!(controller.options.credential, None);
        settle(&mut controller);
        assert!(
            calls(&log)
                .iter()
                .any(|call| call.ends_with("credential=true")),
            "the worker receives the secret without the sheet keeping it"
        );

        assert_eq!(
            controller.status_after(ProjectionNotice::Ended {
                device_id: "other".to_owned(),
                error: None,
            }),
            None,
            "another receiver's session is not this one"
        );
        controller.status = ProjectionStatus::Connected {
            device_id: "AirPlay".to_owned(),
        };
        assert_eq!(
            controller.status_after(ProjectionNotice::Ended {
                device_id: "AirPlay".to_owned(),
                error: Some("receiver went to sleep".to_owned()),
            }),
            Some(ProjectionStatus::Failed(ProjectionFailure::Lost)),
            "the receiver's own words go to the log; the sheet says it disconnected"
        );
        controller.status = ProjectionStatus::Connecting {
            device_id: "AirPlay".to_owned(),
        };
        assert_eq!(
            controller.status_after(ProjectionNotice::ServiceLost),
            None,
            "a request in flight reports its own outcome"
        );
    }

    /// The production backend against a stand-in broker on a real socket:
    /// one connection for every request, the credential inside the connect
    /// request, and an unrequested end reported.
    #[test]
    fn the_backend_speaks_the_projector_control_socket() {
        let dir = tempfile::tempdir().expect("runtime dir");
        let socket = dir.path().join("control.sock");
        let listener = tilo_files::native::socket::bind(&socket).expect("broker socket");
        let receiver = device(ProjectionProtocol::AirPlay);
        let broker_device = receiver.clone();
        let broker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("the session connects");
            let Some(ProjectorRequest::Discover { .. }) =
                control::read_request(&mut stream).expect("discover")
            else {
                panic!("expected a discovery request");
            };
            control::write_event(
                &mut stream,
                &ProjectorEvent::Devices(vec![broker_device.clone()]),
            )
            .expect("devices");
            let Some(ProjectorRequest::Connect { credential, .. }) =
                control::read_request(&mut stream).expect("connect")
            else {
                panic!("expected a connect request");
            };
            let credential = credential.map(|secret| secret.expose().to_owned());
            for event in [
                ProjectorEvent::Connecting {
                    device_id: broker_device.id.clone(),
                    stage: ConnectStage::Starting,
                },
                ProjectorEvent::Connected {
                    device_id: broker_device.id.clone(),
                },
                ProjectorEvent::Ended {
                    device_id: broker_device.id.clone(),
                    error: Some("receiver went to sleep".to_owned()),
                },
            ] {
                control::write_event(&mut stream, &event).expect("session event");
            }
            // Only one connection: a second accept would mean the backend
            // reconnected per request.
            listener.set_nonblocking(true).expect("nonblocking");
            assert!(
                listener.accept().is_err(),
                "one connection serves every request"
            );
            credential
        });

        let notices = Arc::new(Mutex::new(Vec::new()));
        let mut backend = SystemProjectionBackend::new(Some(socket));
        let seen = Arc::clone(&notices);
        backend.set_notice_sink(Arc::new(move |notice| {
            seen.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(notice);
        }));
        assert_eq!(
            backend.discover().expect("discover"),
            std::slice::from_ref(&receiver)
        );
        let options = ProjectionOptions {
            credential: Some("4821".to_owned()),
            ..ProjectionOptions::default()
        };
        backend.connect(&receiver, options).expect("connect");
        assert_eq!(
            broker.join().expect("broker").as_deref(),
            Some("4821"),
            "the credential crossed inside the connect request"
        );
        let deadline = tilo_test_env::hang_deadline();
        while notices
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
            < 2
            && Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert_eq!(
            *notices
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            [
                ProjectionNotice::Ended {
                    device_id: receiver.id.clone(),
                    error: Some("receiver went to sleep".to_owned()),
                },
                ProjectionNotice::ServiceLost,
            ]
        );
    }
}
