//! Screen projection state and protocol-broker integration.
//!
//! The shell owns the picker and its lifecycle, while the transport broker owns
//! protocol details.  Keeping that boundary explicit matters here: Miracast,
//! Google Cast, `AirPlay` and `FCast` have different discovery and authentication
//! handshakes, but the UI must not grow four subtly different connection flows.
//!
//! The production integration point is the `tilo-projector` service's control
//! socket (`tilo_projector::control`): typed requests in, session events out,
//! over one connection the shell holds. Tests replace it with an in-memory
//! backend, so every state transition is exercised without pretending a
//! receiver exists.

use std::fmt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_lock::MutexRecover as _;
use tilo_projector::CaptureProfile;
use tilo_projector::control::{self, ConnectStage, Credential, ProjectorEvent, ProjectorRequest};
pub use tilo_projector::{
    DisplayResolutionLimit, DisplayStreamPolicy, ProjectionDevice, ProjectionMode,
    ProjectionOptions, ProjectionProtocol,
};

/// How long a discovery listens for receivers.
const DISCOVERY_WINDOW_MS: u64 = 1_500;
/// Replies may take their own bound plus this much: the service's 15 s ready
/// timeout and 3 s stop bound for a connect, the 3 s stop bound for a
/// disconnect.
const DISCOVER_REPLY_TIMEOUT: Duration = Duration::from_millis(6_500);
const CONNECT_REPLY_TIMEOUT: Duration = Duration::from_secs(25);
const DISCONNECT_REPLY_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectionStatus {
    Idle,
    Scanning,
    Connecting { device_id: String },
    Connected { device_id: String },
    Error(String),
}

/// Blocking protocol implementation. The controller always calls it on its
/// worker thread, never on the render/event thread.
pub trait ProjectionBackend: Send + 'static {
    /// # Errors
    ///
    /// Returns a user-facing message if the projection service is
    /// unreachable or returns malformed discovery data.
    fn discover(&mut self) -> Result<Vec<ProjectionDevice>, String>;

    /// # Errors
    ///
    /// Returns a user-facing message if `device` doesn't support
    /// `options.mode` or the projection service refuses/fails the
    /// connection.
    fn connect(
        &mut self,
        device: &ProjectionDevice,
        options: ProjectionOptions,
    ) -> Result<(), String>;

    /// # Errors
    ///
    /// Returns a user-facing message if the projection service refuses
    /// or fails the disconnect.
    fn disconnect(&mut self, device: &ProjectionDevice) -> Result<(), String>;

    /// Gives the backend somewhere to report session changes nobody asked
    /// for. Backends without a live service (tests) have none to report.
    fn set_notice_sink(&mut self, _sink: NoticeSink) {}
}

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

/// Capture profiles the projection service asks for, fanned out to the
/// compositor's capture tap. The service reports a profile only when it
/// changes, so the tap drains a channel instead of reading anything per frame.
static CAPTURE_PROFILES: Mutex<Vec<Sender<CaptureProfile>>> = Mutex::new(Vec::new());

/// A channel of every capture profile the projection service sends from now on.
pub fn subscribe_capture_profiles() -> Receiver<CaptureProfile> {
    let (tx, rx) = mpsc::channel();
    CAPTURE_PROFILES.lock_recover().push(tx);
    rx
}

fn publish_capture_profile(profile: CaptureProfile) {
    CAPTURE_PROFILES
        .lock_recover()
        .retain(|tx| tx.send(profile).is_ok());
}

/// Production adapter for the small privileged projection broker provided by
/// the OS image. The compositor never grants a third-party app raw `PipeWire`,
/// Wi-Fi Direct or mDNS access just to populate this picker.
///
/// It holds one connection to the broker's control socket for as long as the
/// shell runs. That connection is the broker's reason to keep a session
/// going: when the shell exits or crashes, the broker stops projecting.
pub struct SystemProjectionBackend {
    socket: Option<PathBuf>,
    link: Option<ControlLink>,
    notices: Option<NoticeSink>,
}

impl fmt::Debug for SystemProjectionBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SystemProjectionBackend")
            .field("socket", &self.socket)
            .field("connected", &self.link.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for SystemProjectionBackend {
    fn default() -> Self {
        #[cfg(test)]
        crate::shell::test_support::ambient_start("projection service resolver");
        Self::with_socket(control::control_socket())
    }
}

/// One open control connection: the write half, and the events its reader
/// thread forwards.
struct ControlLink {
    writer: UnixStream,
    events: Receiver<ProjectorEvent>,
}

impl SystemProjectionBackend {
    pub fn with_socket(socket: Option<PathBuf>) -> Self {
        Self {
            socket,
            link: None,
            notices: None,
        }
    }

    fn unavailable(error: impl fmt::Display) -> String {
        crate::shell::i18n::t_args(
            "projection-service-unavailable-detail",
            &[("error", error.to_string().into())],
        )
    }

    fn open(&self) -> Result<ControlLink, String> {
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| Self::unavailable("XDG_RUNTIME_DIR is unavailable"))?;
        let writer = UnixStream::connect(socket).map_err(Self::unavailable)?;
        let mut reader = writer.try_clone().map_err(Self::unavailable)?;
        let (tx, events) = mpsc::channel();
        let notices = self.notices.clone();
        std::thread::Builder::new()
            .name("tilo-projection-events".into())
            .spawn(move || {
                while let Ok(Some(event)) = control::read_event(&mut reader) {
                    match event {
                        ProjectorEvent::CaptureProfile(profile) => {
                            publish_capture_profile(profile);
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
    fn send(&mut self, request: &ProjectorRequest) -> Result<&Receiver<ProjectorEvent>, String> {
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
        mut answer: impl FnMut(ProjectorEvent) -> Option<Result<T, String>>,
    ) -> Result<T, String> {
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
                    return Err(crate::shell::i18n::t("projection-service-stopped"));
                }
            }
        }
    }
}

impl ProjectionBackend for SystemProjectionBackend {
    fn discover(&mut self) -> Result<Vec<ProjectionDevice>, String> {
        let mut devices = self.reply(
            &ProjectorRequest::Discover {
                timeout_ms: DISCOVERY_WINDOW_MS,
            },
            DISCOVER_REPLY_TIMEOUT,
            |event| match event {
                ProjectorEvent::Devices(devices) => Some(Ok(devices)),
                ProjectorEvent::Failed(error) => Some(Err(error)),
                _ => None,
            },
        )?;
        // A receiver advertised on two interfaces must be one row. Sorting also
        // makes refreshes stable instead of visually shuffling the picker.
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
    ) -> Result<(), String> {
        if !device.supports(options.mode) {
            return Err(crate::shell::i18n::t_args(
                "projection-mode-unsupported",
                &[
                    ("device", device.name.as_str().into()),
                    ("mode", options.mode.label().into()),
                ],
            ));
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
                Some(Err(error.unwrap_or_else(|| {
                    crate::shell::i18n::t("projection-service-stopped")
                })))
            }
            ProjectorEvent::Failed(error) => Some(Err(error)),
            _ => None,
        })
    }

    fn disconnect(&mut self, _device: &ProjectionDevice) -> Result<(), String> {
        self.reply(
            &ProjectorRequest::Disconnect,
            DISCONNECT_REPLY_TIMEOUT,
            |event| match event {
                ProjectorEvent::Idle => Some(Ok(())),
                ProjectorEvent::Failed(error) => Some(Err(error)),
                _ => None,
            },
        )
    }

    fn set_notice_sink(&mut self, sink: NoticeSink) {
        self.notices = Some(sink);
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
    Discovered(Result<Vec<ProjectionDevice>, String>),
    Connected(String, Result<(), String>),
    Disconnected(String, Result<(), String>),
    Notice(ProjectionNotice),
}

/// Non-blocking façade consumed by the overlay.
pub struct ProjectionController {
    tx: Option<Sender<WorkerCommand>>,
    rx: Receiver<WorkerEvent>,
    pub devices: Vec<ProjectionDevice>,
    pub status: ProjectionStatus,
    pub options: ProjectionOptions,
    pub selected: usize,
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

impl Default for ProjectionController {
    fn default() -> Self {
        Self::new(Box::new(SystemProjectionBackend::default()))
    }
}

impl ProjectionController {
    pub fn new(mut backend: Box<dyn ProjectionBackend>) -> Self {
        #[cfg(test)]
        crate::shell::test_support::ambient_start("projection live worker");
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let notice_tx = event_tx.clone();
        backend.set_notice_sink(Arc::new(move |notice| {
            let _ = notice_tx.send(WorkerEvent::Notice(notice));
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
                    if event_tx.send(event).is_err() {
                        break;
                    }
                }
            });
        if let Err(err) = spawned {
            // Every caller of `self.tx.send` already treats a closed
            // channel as "service stopped" and reports
            // `ProjectionStatus::Error` (see `refresh`/`connect_selected`/
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
        }
    }

    pub(crate) fn unavailable() -> Self {
        let (_, rx) = mpsc::channel();
        Self {
            tx: None,
            rx,
            devices: Vec::new(),
            status: ProjectionStatus::Error(crate::shell::i18n::t(
                "projection-service-unavailable",
            )),
            options: ProjectionOptions::default(),
            selected: 0,
        }
    }

    fn available_sender(&mut self) -> Option<&Sender<WorkerCommand>> {
        if self.tx.is_none() {
            self.status =
                ProjectionStatus::Error(crate::shell::i18n::t("projection-service-unavailable"));
        }
        self.tx.as_ref()
    }

    pub fn refresh(&mut self) {
        let Some(tx) = self.available_sender() else {
            return;
        };
        if tx.send(WorkerCommand::Discover).is_ok() {
            self.status = ProjectionStatus::Scanning;
        } else {
            self.status =
                ProjectionStatus::Error(crate::shell::i18n::t("projection-service-stopped"));
        }
    }

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
            // Do not retain a receiver password in compositor memory after it
            // has crossed into the broker worker.
            self.options.credential = None;
        }
    }

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

    /// Drain every completed worker response. Returns whether visible state changed.
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
                        Err(error) => self.status = ProjectionStatus::Error(error),
                    }
                    changed = true;
                }
                Ok(WorkerEvent::Connected(device_id, result)) => {
                    self.status = match result {
                        Ok(()) => ProjectionStatus::Connected { device_id },
                        Err(error) => ProjectionStatus::Error(error),
                    };
                    changed = true;
                }
                Ok(WorkerEvent::Disconnected(_device_id, result)) => {
                    self.status = match result {
                        Ok(()) => ProjectionStatus::Idle,
                        Err(error) => ProjectionStatus::Error(error),
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

    /// What an unrequested session change means for the picker: only the
    /// session it shows as connected can end under it. A connect or
    /// disconnect in flight reports its own outcome.
    fn status_after(&self, notice: ProjectionNotice) -> Option<ProjectionStatus> {
        let ProjectionStatus::Connected { device_id: current } = &self.status else {
            return None;
        };
        match notice {
            ProjectionNotice::Ended { device_id, error } => (device_id == *current)
                .then(|| error.map_or(ProjectionStatus::Idle, ProjectionStatus::Error)),
            ProjectionNotice::ServiceLost => Some(ProjectionStatus::Error(crate::shell::i18n::t(
                "projection-service-stopped",
            ))),
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.devices.is_empty() {
            return;
        }
        let len = isize::try_from(self.devices.len()).unwrap_or(isize::MAX);
        let current = isize::try_from(self.selected).unwrap_or(0);
        let next = current.saturating_add(delta).rem_euclid(len);
        self.selected = usize::try_from(next).unwrap_or(0);
        if let Some(device) = self.devices.get(self.selected)
            && !device.supports(self.options.mode)
        {
            self.options.mode = ProjectionMode::Mirror;
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
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    #[derive(Default)]
    struct FakeBackend {
        devices: Vec<ProjectionDevice>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl ProjectionBackend for FakeBackend {
        fn discover(&mut self) -> Result<Vec<ProjectionDevice>, String> {
            self.calls.lock().unwrap().push("discover".into());
            Ok(self.devices.clone())
        }
        fn connect(&mut self, d: &ProjectionDevice, o: ProjectionOptions) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!(
                "connect:{}:{}:credential={}",
                d.protocol.label(),
                o.mode.label(),
                o.credential.is_some()
            ));
            Ok(())
        }
        fn disconnect(&mut self, d: &ProjectionDevice) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("stop:{}", d.id));
            Ok(())
        }
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

    #[test]
    fn projection_protocols_use_the_async_controller_and_honour_layout_support() {
        fn all_four_protocols_follow_the_real_async_controller_path() {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let backend = FakeBackend {
                devices: ProjectionProtocol::ALL.into_iter().map(device).collect(),
                calls: calls.clone(),
            };
            let mut controller = ProjectionController::new(Box::new(backend));
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
            }
            // Clone out of the guard immediately rather than holding the lock
            // for the length of the assertion loop below.
            let log = calls.lock().unwrap().clone();
            for protocol in ProjectionProtocol::ALL {
                assert!(
                    log.iter().any(|line| line.contains(protocol.label())),
                    "{protocol:?} must cross the same production controller boundary"
                );
            }
        }

        fn unsupported_layout_falls_back_to_duplicate_before_connecting() {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let backend = FakeBackend {
                devices: vec![ProjectionDevice {
                    modes: vec![ProjectionMode::Mirror],
                    ..device(ProjectionProtocol::AirPlay)
                }],
                calls: calls.clone(),
            };
            let mut controller = ProjectionController::new(Box::new(backend));
            controller.refresh();
            settle(&mut controller);
            controller.options.mode = ProjectionMode::Extend;
            controller.connect_selected();
            settle(&mut controller);
            assert_eq!(controller.options.mode, ProjectionMode::Mirror);
            let fell_back_to_duplicate = calls
                .lock()
                .unwrap()
                .iter()
                .any(|c| c.contains(":Duplicate:"));
            assert!(fell_back_to_duplicate);
        }

        fn airplay_pairing_secret_crosses_once_and_is_immediately_forgotten() {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let mut controller = ProjectionController::new(Box::new(FakeBackend {
                devices: vec![device(ProjectionProtocol::AirPlay)],
                calls: calls.clone(),
            }));
            controller.refresh();
            settle(&mut controller);
            controller.options.credential = Some("4821".to_owned());
            controller.connect_selected();
            assert_eq!(controller.options.credential, None);
            settle(&mut controller);
            assert!(
                calls
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|call| call.ends_with("credential=true")),
                "the worker must receive the secret without retaining it in overlay state"
            );
        }

        fn unsupported_layouts_are_exposed_as_disabled_to_the_overlay() {
            let mut controller = ProjectionController::new(Box::new(FakeBackend {
                devices: vec![ProjectionDevice {
                    modes: vec![ProjectionMode::Mirror],
                    ..device(ProjectionProtocol::Chromecast)
                }],
                ..FakeBackend::default()
            }));
            controller.refresh();
            settle(&mut controller);
            assert!(controller.selected_device_supports(ProjectionMode::Mirror));
            assert!(!controller.selected_device_supports(ProjectionMode::Extend));
            assert!(!controller.selected_device_supports(ProjectionMode::ExternalOnly));
        }

        all_four_protocols_follow_the_real_async_controller_path();
        unsupported_layout_falls_back_to_duplicate_before_connecting();
        airplay_pairing_secret_crosses_once_and_is_immediately_forgotten();
        unsupported_layouts_are_exposed_as_disabled_to_the_overlay();
    }

    /// The production backend against a stand-in broker on a real socket:
    /// one connection for every request, the credential inside the connect
    /// request, profiles to the capture tap and an unrequested end reported.
    #[test]
    fn system_backend_speaks_the_projector_control_socket() {
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().expect("runtime dir");
        let socket = dir.path().join("control.sock");
        let listener = UnixListener::bind(&socket).expect("broker socket");
        let receiver = device(ProjectionProtocol::AirPlay);
        let broker_device = receiver.clone();
        let broker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("shell connects");
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
                ProjectorEvent::CaptureProfile(CaptureProfile::new(1280, 720, 30, 4_000)),
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

        let profiles = subscribe_capture_profiles();
        let notices = Arc::new(Mutex::new(Vec::new()));
        let mut backend = SystemProjectionBackend::with_socket(Some(socket));
        let seen = notices.clone();
        backend.set_notice_sink(Arc::new(move |notice| seen.lock().unwrap().push(notice)));
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
        assert_eq!(
            profiles
                .recv_timeout(tilo_test_env::hang_bound())
                .expect("profile reaches the capture tap"),
            CaptureProfile::new(1280, 720, 30, 4_000)
        );
        let deadline = tilo_test_env::hang_deadline();
        while notices.lock().unwrap().len() < 2 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(
            *notices.lock().unwrap(),
            [
                ProjectionNotice::Ended {
                    device_id: receiver.id.clone(),
                    error: Some("receiver went to sleep".to_owned()),
                },
                ProjectionNotice::ServiceLost,
            ]
        );
    }

    #[test]
    fn a_connected_session_that_ends_unasked_returns_the_picker_to_idle_or_error() {
        let mut controller = ProjectionController::new(Box::new(FakeBackend::default()));
        controller.status = ProjectionStatus::Connected {
            device_id: "studio".to_owned(),
        };
        assert_eq!(
            controller.status_after(ProjectionNotice::Ended {
                device_id: "other".to_owned(),
                error: None,
            }),
            None,
            "another receiver's session is not this one"
        );
        assert_eq!(
            controller.status_after(ProjectionNotice::Ended {
                device_id: "studio".to_owned(),
                error: Some("receiver went to sleep".to_owned()),
            }),
            Some(ProjectionStatus::Error("receiver went to sleep".to_owned()))
        );
        controller.status = ProjectionStatus::Connecting {
            device_id: "studio".to_owned(),
        };
        assert_eq!(
            controller.status_after(ProjectionNotice::ServiceLost),
            None,
            "a request in flight reports its own outcome"
        );
    }
}
