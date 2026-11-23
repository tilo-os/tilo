//! The projector service as a playback's cast transport.
//!
//! One connection per cast session, open for as long as the session runs: the service stops the
//! session when its client goes away, which is how a playback that ends stops what it started.
//! A thread per session asks for the connection and pushes every stage, the connected moment and
//! the end into the host's event list; the host never waits on a receiver.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;
use tilo_projector::control::client::{Client, ControlError, ControlNotice};
use tilo_projector::control::{ConnectStage, ProjectorEvent};
use tilo_projector::{ProjectionDevice, ProjectionMode, ProjectionOptions};

use super::{CastEvent, CastStage, CastTransport, Receiver};

/// The projector service's cast sessions, one at a time.
pub struct ProjectorTransport {
    /// `None` is the service's own control socket.
    socket: Option<PathBuf>,
    /// The receivers the last discovery found, by the id an output names them with.
    receivers: BTreeMap<String, ProjectionDevice>,
    /// What the running session's thread reported, shared across the thread boundary.
    events: Arc<Mutex<Vec<(String, CastEvent)>>>,
    worker: Option<Worker>,
}

struct Worker {
    cast: String,
    /// Holding this keeps the session alive; dropping it tells the worker to disconnect.
    _stop: Option<Sender<()>>,
}

impl ProjectorTransport {
    /// A transport speaking to the service's own control socket.
    #[must_use]
    pub fn new() -> Self {
        Self::with_socket(tilo_projector::control::control_socket())
    }

    /// A transport speaking to `socket`; `None` is the service's own.
    #[must_use]
    pub fn with_socket(socket: Option<PathBuf>) -> Self {
        Self {
            socket,
            receivers: BTreeMap::new(),
            events: Arc::new(Mutex::new(Vec::new())),
            worker: None,
        }
    }
}

impl Default for ProjectorTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl CastTransport for ProjectorTransport {
    fn discover(&mut self) -> Result<Vec<Receiver>, String> {
        let mut client = Client::new(self.socket.clone());
        let devices = client.discover().map_err(readable)?;
        self.receivers = devices
            .iter()
            .map(|device| (device.id.clone(), device.clone()))
            .collect();
        Ok(devices
            .iter()
            .map(|device| Receiver {
                id: device.id.clone(),
                name: device.name.clone(),
                protocol: device.protocol.label().to_owned(),
                detail: device.detail.clone(),
            })
            .collect())
    }

    fn begin(&mut self, cast: &str, receiver: &Receiver) -> Result<(), String> {
        // The service runs one session: starting another ends whatever ran before it.
        self.worker = None;
        let device = self
            .receivers
            .get(&receiver.id)
            .cloned()
            .ok_or_else(|| format!("Couldn't find {} on the network.", receiver.name))?;
        let socket = self.socket.clone();
        let events = Arc::clone(&self.events);
        let id = cast.to_owned();
        let (stop, stopped) = mpsc::channel();
        std::thread::Builder::new()
            .name("tilo-cast".into())
            .spawn(move || {
                let mut client = Client::new(socket);
                let told = Arc::clone(&events);
                let told_id = id.clone();
                let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let told_ended = Arc::clone(&ended);
                client.set_notice_sink(Arc::new(move |notice| {
                    let event = match notice {
                        ControlNotice::Event(ProjectorEvent::Connecting { stage, .. }) => {
                            CastEvent::Stage(stage_of(stage))
                        }
                        ControlNotice::Event(ProjectorEvent::Connected { .. }) => {
                            CastEvent::Connected
                        }
                        ControlNotice::Event(ProjectorEvent::Ended { error, .. }) => {
                            told_ended.store(true, std::sync::atomic::Ordering::Release);
                            CastEvent::Ended { reason: error }
                        }
                        ControlNotice::Event(ProjectorEvent::Failed(reason)) => {
                            told_ended.store(true, std::sync::atomic::Ordering::Release);
                            CastEvent::Ended {
                                reason: Some(reason),
                            }
                        }
                        ControlNotice::Event(_) => return,
                        ControlNotice::Lost => {
                            told_ended.store(true, std::sync::atomic::Ordering::Release);
                            CastEvent::Ended { reason: None }
                        }
                    };
                    told.lock_recover().push((told_id.clone(), event));
                }));
                let options = ProjectionOptions {
                    mode: ProjectionMode::Mirror,
                    audio: true,
                    ..ProjectionOptions::default()
                };
                let started = client.connect(&device, options);
                let connected = started.is_ok();
                if let Err(error) = started
                    && !ended.load(std::sync::atomic::Ordering::Acquire)
                {
                    // The service said nothing about how it failed; say what a person can read.
                    let reason = readable(error);
                    events.lock_recover().push((
                        id.clone(),
                        CastEvent::Ended {
                            reason: Some(reason),
                        },
                    ));
                }
                // The session runs while this connection is open; the service stops it when the
                // connection closes.
                let _ = stopped.recv();
                if connected {
                    let _ = client.disconnect();
                }
            })
            .map_err(|error| format!("The connection to the projector could not start: {error}"))?;
        self.worker = Some(Worker {
            cast: cast.to_owned(),
            _stop: Some(stop),
        });
        Ok(())
    }

    fn end(&mut self, cast: &str) {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.cast == cast)
        {
            self.worker = None;
        }
    }

    fn events(&mut self) -> Vec<(String, CastEvent)> {
        std::mem::take(&mut *self.events.lock_recover())
    }
}

fn stage_of(stage: ConnectStage) -> CastStage {
    match stage {
        ConnectStage::Starting => CastStage::Starting,
        ConnectStage::Handshake => CastStage::Handshake,
        ConnectStage::WaitingForFrames => CastStage::WaitingForFrames,
        ConnectStage::Loading => CastStage::Loading,
    }
}

/// The outcome in words a person can read; the service's own account goes to the log.
fn readable(error: ControlError) -> String {
    match error {
        ControlError::Unavailable(detail) => {
            tracing::warn!(%detail, "the projection service is unavailable");
            "The projection service isn't running.".to_owned()
        }
        ControlError::Refused(detail) => {
            tracing::warn!(%detail, "the projection service refused");
            "The television refused the connection.".to_owned()
        }
        ControlError::Timeout => "The television didn't answer.".to_owned(),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_lossless,
    clippy::similar_names,
    clippy::too_many_lines,
    reason = "test fixtures: a failed step is the assertion, and their counting cannot overflow"
)]
mod tests {
    use super::*;
    use tilo_projector::control::{self, ProjectorRequest};
    use tilo_projector::{ProjectionDevice, ProjectionProtocol};

    /// One receiver of every protocol the service speaks.
    fn receivers() -> Vec<ProjectionDevice> {
        ProjectionProtocol::ALL
            .into_iter()
            .enumerate()
            .map(|(index, protocol)| ProjectionDevice {
                id: format!("{protocol:?}"),
                name: format!("Receiver {}", index + 1),
                protocol,
                detail: "Local network".to_owned(),
                modes: vec![ProjectionMode::Mirror],
                addresses: vec![format!("192.0.2.{}", index + 10)],
                port: 8009,
            })
            .collect()
    }

    fn cast_receiver() -> ProjectionDevice {
        let mut devices = receivers();
        devices
            .iter()
            .position(|device| device.protocol == ProjectionProtocol::Chromecast)
            .map(|index| devices.remove(index))
            .expect("the cast receiver is listed")
    }

    /// A stand-in projector service on a real socket: it answers a discovery, then runs one
    /// session - stages, connected, then idle when the client stops it - and stops when the
    /// client goes away.
    fn serve(listener: &std::os::unix::net::UnixListener) {
        let (mut stream, _) = listener.accept().expect("the transport connects");
        let Some(ProjectorRequest::Discover { .. }) =
            control::read_request(&mut stream).expect("discover")
        else {
            panic!("expected a discovery request");
        };
        control::write_event(&mut stream, &ProjectorEvent::Devices(receivers())).expect("devices");
        // A discovery is a connection of its own; the session opens a second one.
        let (mut stream, _) = listener.accept().expect("the session connects");
        let Some(ProjectorRequest::Connect { device: asked, .. }) =
            control::read_request(&mut stream).expect("connect")
        else {
            panic!("expected a connect request");
        };
        let device = cast_receiver();
        assert_eq!(asked.id, device.id);
        for event in [
            ProjectorEvent::Connecting {
                device_id: device.id.clone(),
                stage: ConnectStage::Starting,
            },
            ProjectorEvent::Connecting {
                device_id: device.id.clone(),
                stage: ConnectStage::Handshake,
            },
            ProjectorEvent::Connected {
                device_id: device.id,
            },
        ] {
            control::write_event(&mut stream, &event).expect("session event");
        }
        match control::read_request(&mut stream) {
            Ok(Some(ProjectorRequest::Disconnect) | None) | Err(_) => {}
            Ok(Some(other)) => panic!("unexpected request: {other:?}"),
        }
        control::write_event(&mut stream, &ProjectorEvent::Idle).expect("idle");
    }

    #[test]
    fn a_cast_crosses_the_control_socket_with_its_stages() {
        let dir = tempfile::tempdir().expect("runtime dir");
        let socket = dir.path().join("control.sock");
        let listener = tilo_files::native::socket::bind(&socket).expect("socket");
        let service = std::thread::spawn(move || serve(&listener));

        let mut transport = ProjectorTransport::with_socket(Some(socket));
        let found = transport.discover().expect("the receivers are found");
        let mut protocols: Vec<&str> = found
            .iter()
            .map(|receiver| receiver.protocol.as_str())
            .collect();
        protocols.sort_unstable();
        assert_eq!(protocols, ["AirPlay", "FCast", "Google Cast", "Miracast"]);

        // The session this test drives is the Google Cast one.
        let living_room = found
            .iter()
            .find(|receiver| receiver.protocol == "Google Cast")
            .expect("the cast receiver is listed");
        transport
            .begin("tv", living_room)
            .expect("the session starts");
        let deadline = tilo_test_env::hang_deadline();
        let mut seen = Vec::new();
        while !seen.contains(&CastEvent::Connected) && std::time::Instant::now() < deadline {
            seen.extend(transport.events().into_iter().map(|(_, event)| event));
            std::thread::yield_now();
        }
        assert_eq!(
            seen,
            [
                CastEvent::Stage(CastStage::Starting),
                CastEvent::Stage(CastStage::Handshake),
                CastEvent::Connected,
            ]
        );
        transport.end("tv");
        service.join().expect("the service stops");
    }
}
