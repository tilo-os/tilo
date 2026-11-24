//! One connection to the projector service's control socket, wherever a caller needs it: the
//! desktop's display sheet and a playback's cast outputs ask the same service the same questions.
//!
//! The connection is opened on the first request and kept: the projector runs one projection at
//! a time, and it stops that one when the connection closes, which is the reason a caller that
//! exits stops what it started. A thread reads the events back so a session that ends without
//! being asked is reported through the notice sink instead of waiting for the next request.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};
use tilo_files::native::socket::Stream;

use crate::control::{self, ConnectStage, Credential, ProjectorEvent, ProjectorRequest};
use crate::{ProjectionDevice, ProjectionOptions};

/// How long a discovery listens for receivers.
pub const DISCOVERY_WINDOW_MS: u64 = 1_500;
/// Replies may take their own bound plus this much: the service's 15 s ready timeout and 3 s stop
/// bound for a connect, the 3 s stop bound for a disconnect.
pub const DISCOVER_REPLY_TIMEOUT: Duration = Duration::from_millis(6_500);
pub const CONNECT_REPLY_TIMEOUT: Duration = Duration::from_secs(25);
pub const DISCONNECT_REPLY_TIMEOUT: Duration = Duration::from_secs(8);

/// What a caller learns without having asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlNotice {
    /// A session event, including ones nobody waited for.
    Event(ProjectorEvent),
    /// The service stopped answering: the connection closed under it.
    Lost,
}

/// Where a caller receives [`ControlNotice`]s.
pub type NoticeSink = Arc<dyn Fn(ControlNotice) + Send + Sync>;

/// Why a request did not get its answer. The service's own words are in the payload; whether a
/// person reads them is the caller's call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlError {
    /// The service is not running, or the connection to it broke.
    Unavailable(String),
    /// The service answered with a failure, or the session ended while starting or stopping.
    Refused(String),
    /// No answer within the request's own bound.
    Timeout,
}

impl fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(detail) => {
                write!(formatter, "the projection service is unavailable: {detail}")
            }
            Self::Refused(detail) => write!(formatter, "the projection service refused: {detail}"),
            Self::Timeout => formatter.write_str("the projection service did not answer in time"),
        }
    }
}

impl std::error::Error for ControlError {}

struct Link {
    writer: Stream,
    events: Receiver<ProjectorEvent>,
    /// Whether the connection is still the one the caller wants. A link the caller replaced is
    /// shut down here and its reader stays quiet: its end is not the service's.
    wanted: Arc<AtomicBool>,
}

impl Drop for Link {
    /// Closing the connection is what stops the projection: the service runs a session only
    /// while its client is there. The reader thread shares the socket, so it is shut down here
    /// rather than left to notice a writer that went away.
    fn drop(&mut self) {
        self.wanted.store(false, Ordering::Release);
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
    }
}

/// A caller's connection to the projector service: requests out, session events back, over one
/// socket that is opened on demand and reopened once if the service restarted.
pub struct Client {
    socket: Option<PathBuf>,
    link: Option<Link>,
    notice: Option<NoticeSink>,
}

impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Client")
            .field("socket", &self.socket)
            .field("connected", &self.link.is_some())
            .finish_non_exhaustive()
    }
}

impl Client {
    /// A client for `socket`; `None` is the service's own control socket.
    #[must_use]
    pub fn new(socket: Option<PathBuf>) -> Self {
        Self {
            socket: socket.or_else(control::control_socket),
            link: None,
            notice: None,
        }
    }

    /// Gives the client somewhere to report session changes nobody asked for. A connection
    /// opened before this would not report to the sink, so it is dropped and reopened.
    pub fn set_notice_sink(&mut self, sink: NoticeSink) {
        self.notice = Some(sink);
        self.link = None;
    }

    fn unavailable(detail: impl fmt::Display) -> ControlError {
        ControlError::Unavailable(detail.to_string())
    }

    fn open(&self) -> Result<Link, ControlError> {
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| Self::unavailable("no runtime directory for the control socket"))?;
        let writer = tilo_files::native::socket::connect(socket).map_err(Self::unavailable)?;
        let mut reader = writer.try_clone().map_err(Self::unavailable)?;
        let (tx, events) = mpsc::channel();
        let notice = self.notice.clone();
        let wanted = Arc::new(AtomicBool::new(true));
        let listened = Arc::clone(&wanted);
        std::thread::Builder::new()
            .name("tilo-projector-events".into())
            .spawn(move || {
                while let Ok(Some(event)) = control::read_event(&mut reader) {
                    if let Some(notice) = notice.as_ref() {
                        notice(ControlNotice::Event(event.clone()));
                    }
                    let _ = tx.send(event);
                }
                if listened.load(Ordering::Acquire)
                    && let Some(notice) = notice.as_ref()
                {
                    notice(ControlNotice::Lost);
                }
            })
            .map_err(Self::unavailable)?;
        Ok(Link {
            writer,
            events,
            wanted,
        })
    }

    /// Sends `request` on the shared connection, opening (or reopening, after the service
    /// restarted) it as needed, and returns the channel its answer arrives on. Events nobody
    /// waited for are dropped first, so a previous session's end is never this one's answer.
    fn send(
        &mut self,
        request: &ProjectorRequest,
    ) -> Result<&Receiver<ProjectorEvent>, ControlError> {
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

    /// Reads events until `answer` turns one into a result, or `timeout` passes.
    fn reply<T>(
        &mut self,
        request: &ProjectorRequest,
        timeout: Duration,
        mut answer: impl FnMut(ProjectorEvent) -> Option<Result<T, ControlError>>,
    ) -> Result<T, ControlError> {
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
                Err(RecvTimeoutError::Timeout) => return Err(ControlError::Timeout),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Self::unavailable("the projection service stopped"));
                }
            }
        }
    }

    /// Looks for receivers for the discovery window. A receiver advertised on two interfaces is
    /// one row, and the list is sorted so refreshes do not shuffle it.
    ///
    /// # Errors
    ///
    /// [`ControlError::Unavailable`] when the service is unreachable, [`ControlError::Refused`]
    /// when it could not look.
    pub fn discover(&mut self) -> Result<Vec<ProjectionDevice>, ControlError> {
        let mut devices = self.reply(
            &ProjectorRequest::Discover {
                timeout_ms: DISCOVERY_WINDOW_MS,
            },
            DISCOVER_REPLY_TIMEOUT,
            |event| match event {
                ProjectorEvent::Devices(devices) => Some(Ok(devices)),
                ProjectorEvent::Failed(error) => Some(Err(ControlError::Refused(error))),
                _ => None,
            },
        )?;
        devices.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.protocol.label().cmp(b.protocol.label()))
        });
        devices.dedup_by(|a, b| a.id == b.id && a.protocol == b.protocol);
        Ok(devices)
    }

    /// Projects to `device`, stopping any running session first, and answers once the receiver
    /// shows the stream. The credential crosses inside the request and is wiped once sent.
    ///
    /// # Errors
    ///
    /// [`ControlError::Refused`] when the receiver refuses or the session ends while starting,
    /// [`ControlError::Unavailable`] without a service, [`ControlError::Timeout`] when it does
    /// not start in time.
    pub fn connect(
        &mut self,
        device: &ProjectionDevice,
        mut options: ProjectionOptions,
    ) -> Result<(), ControlError> {
        if !device.supports(options.mode) {
            return Err(ControlError::Refused(format!(
                "{} does not take the {} layout",
                device.name,
                options.mode.label()
            )));
        }
        let credential = options.credential.take().map(Credential::new);
        let request = ProjectorRequest::Connect {
            device: device.clone(),
            options,
            credential,
        };
        // A running session to the same receiver ends before this one starts; only events after
        // this session's start answer it.
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
                Some(Err(ControlError::Refused(error.unwrap_or_else(|| {
                    "the session ended while starting".to_owned()
                }))))
            }
            ProjectorEvent::Failed(error) => Some(Err(ControlError::Refused(error))),
            _ => None,
        })
    }

    /// Stops the running session, if any, and answers once nothing runs.
    ///
    /// # Errors
    ///
    /// [`ControlError::Refused`] when the service could not stop it,
    /// [`ControlError::Unavailable`] without a service.
    pub fn disconnect(&mut self) -> Result<(), ControlError> {
        self.reply(
            &ProjectorRequest::Disconnect,
            DISCONNECT_REPLY_TIMEOUT,
            |event| match event {
                ProjectorEvent::Idle => Some(Ok(())),
                ProjectorEvent::Failed(error) => Some(Err(ControlError::Refused(error))),
                _ => None,
            },
        )
    }
}
