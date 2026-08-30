//! Screen projection state and protocol-broker integration.
//!
//! The shell owns the picker and its lifecycle, while the transport broker owns
//! protocol details.  Keeping that boundary explicit matters here: Miracast,
//! Google Cast, `AirPlay` and `FCast` have different discovery and authentication
//! handshakes, but the UI must not grow four subtly different connection flows.
//!
//! The production integration point is `/usr/libexec/qos-projector`. It speaks
//! newline-free JSON on stdout for discovery and receives one protocol-neutral
//! connection request. Tests replace it with an in-memory backend, so every state
//! transition is exercised without pretending a receiver exists.

use std::fmt;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

pub use qos_projector::{
    DisplayResolutionLimit, DisplayStreamPolicy, ProjectionDevice, ProjectionMode,
    ProjectionOptions, ProjectionProtocol,
};

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
}

/// Production adapter for the small privileged projection broker provided by
/// the OS image. The compositor never grants a third-party app raw `PipeWire`,
/// Wi-Fi Direct or mDNS access just to populate this picker.
#[derive(Clone, Debug)]
pub struct SystemProjectionBackend {
    helper: String,
}

impl Default for SystemProjectionBackend {
    fn default() -> Self {
        Self {
            helper: std::env::var("QOS_PROJECTOR_HELPER")
                .unwrap_or_else(|_| "/usr/libexec/qos-projector".into()),
        }
    }
}

impl SystemProjectionBackend {
    pub fn with_helper(helper: impl Into<String>) -> Self {
        Self {
            helper: helper.into(),
        }
    }

    fn run(&self, args: &[&str], credential: Option<&str>) -> Result<std::process::Output, String> {
        let mut command = Command::new(&self.helper);
        command.args(args);
        if let Some(credential) = credential {
            command.env("QOS_PROJECTION_CREDENTIAL", credential);
        }
        command.output().map_err(|e| {
            format!("Projection service is unavailable ({e}). Check the qOS projector service.")
        })
    }

    fn checked(&self, args: &[&str]) -> Result<std::process::Output, String> {
        self.checked_with_credential(args, None)
    }

    fn checked_with_credential(
        &self,
        args: &[&str],
        credential: Option<&str>,
    ) -> Result<std::process::Output, String> {
        let out = self.run(args, credential)?;
        if out.status.success() {
            Ok(out)
        } else {
            let message = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if message.is_empty() {
                format!("Projection service exited with {}", out.status)
            } else {
                message
            })
        }
    }
}

impl ProjectionBackend for SystemProjectionBackend {
    fn discover(&mut self) -> Result<Vec<ProjectionDevice>, String> {
        let out = self.checked(&["discover", "--json"])?;
        let mut devices: Vec<ProjectionDevice> = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("Projection service returned invalid discovery data: {e}"))?;
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
        options: ProjectionOptions,
    ) -> Result<(), String> {
        if !device.supports(options.mode) {
            return Err(format!(
                "{} does not support {} mode",
                device.name,
                options.mode.label()
            ));
        }
        let protocol = serde_json::to_value(device.protocol)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "miracast".into());
        let mode = serde_json::to_value(options.mode)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "mirror".into());
        let port = device.port.to_string();
        let resolution_limit = serde_json::to_value(options.stream_policy.resolution_limit)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "native".into());
        let max_fps = options.stream_policy.max_fps.to_string();
        self.checked_with_credential(
            &[
                "connect",
                "--protocol",
                &protocol,
                "--device",
                &device.id,
                "--address",
                device.addresses.first().map_or("", String::as_str),
                "--port",
                &port,
                "--mode",
                &mode,
                "--resolution-limit",
                &resolution_limit,
                "--max-fps",
                &max_fps,
                if options.stream_policy.adaptive {
                    "--adaptive"
                } else {
                    "--fixed"
                },
                if options.audio {
                    "--audio"
                } else {
                    "--no-audio"
                },
            ],
            options.credential.as_deref(),
        )?;
        Ok(())
    }

    fn disconnect(&mut self, device: &ProjectionDevice) -> Result<(), String> {
        let protocol = serde_json::to_value(device.protocol)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "miracast".into());
        self.checked(&[
            "disconnect",
            "--protocol",
            &protocol,
            "--device",
            &device.id,
        ])?;
        Ok(())
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
}

/// Non-blocking façade consumed by the overlay.
pub struct ProjectionController {
    tx: Sender<WorkerCommand>,
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
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("qos-projection".into())
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
            tx: command_tx,
            rx: event_rx,
            devices: Vec::new(),
            status: ProjectionStatus::Idle,
            options: ProjectionOptions::default(),
            selected: 0,
        }
    }

    pub fn refresh(&mut self) {
        if self.tx.send(WorkerCommand::Discover).is_ok() {
            self.status = ProjectionStatus::Scanning;
        } else {
            self.status = ProjectionStatus::Error("Projection service stopped".into());
        }
    }

    pub fn connect_selected(&mut self) {
        let Some(device) = self.devices.get(self.selected).cloned() else {
            return;
        };
        if !device.supports(self.options.mode) {
            self.options.mode = ProjectionMode::Mirror;
        }
        if self
            .tx
            .send(WorkerCommand::Connect(device.clone(), self.options.clone()))
            .is_ok()
        {
            self.status = ProjectionStatus::Connecting {
                device_id: device.id,
            };
            // Do not retain a receiver password in compositor memory after it
            // has crossed into the broker worker.
            self.options.credential = None;
        }
    }

    pub fn disconnect(&mut self) {
        let ProjectionStatus::Connected { device_id } = &self.status else {
            return;
        };
        let Some(device) = self.devices.iter().find(|d| &d.id == device_id).cloned() else {
            self.status = ProjectionStatus::Idle;
            return;
        };
        if self.tx.send(WorkerCommand::Disconnect(device)).is_ok() {
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
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        changed
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
    use std::time::{Duration, Instant};

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
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        while !controller.poll() && Instant::now() < deadline {
            std::thread::yield_now();
        }
    }

    #[test]
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
        let log = calls.lock().unwrap();
        for protocol in ProjectionProtocol::ALL {
            assert!(
                log.iter().any(|line| line.contains(protocol.label())),
                "{protocol:?} must cross the same production controller boundary"
            );
        }
    }

    #[test]
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

    #[test]
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

    #[test]
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
}
