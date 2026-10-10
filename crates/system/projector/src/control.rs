//! The broker's control socket, shared by `tilo-projector serve` and the shell.
//!
//! Every message in either direction is one [`CONTROL_FRAMING`] frame holding
//! a postcard-encoded [`ProjectorRequest`] (client to broker) or
//! [`ProjectorEvent`] (broker to client). A connection is also a subscription:
//! the broker sends every client the events of the session it runs, and stops
//! that session when its last client goes away.

use std::fmt;
use std::io::{Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use wire_frame::{FrameError, Framing};
use zeroize::Zeroizing;

use crate::{CaptureProfile, ProjectionDevice, ProjectionOptions};

/// Framing of the control socket. Requests and events are small; the largest
/// is a discovery result.
pub const CONTROL_FRAMING: Framing = Framing::little_endian(1024 * 1024);

/// Overrides the broker's runtime directory (tests, isolated sessions).
pub const RUNTIME_DIR_ENV: &str = "TILO_PROJECTOR_RUNTIME_DIR";

/// File name of the control socket inside [`runtime_dir`].
pub const CONTROL_SOCKET_NAME: &str = "control.sock";

/// The broker's private runtime directory: the control socket, the
/// compositor's video socket and the HLS playlist and segments.
pub fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os(RUNTIME_DIR_ENV)
        .map(PathBuf::from)
        .or_else(|| tilo_paths::xdg::runtime_dir().map(|dir| dir.join("tilo-projector")))
}

/// Where the broker listens for control connections.
pub fn control_socket() -> Option<PathBuf> {
    runtime_dir().map(|dir| dir.join(CONTROL_SOCKET_NAME))
}

/// A receiver PIN or password on its way to the broker.
///
/// It travels only inside a control frame, never in an environment variable
/// or argument, and its memory is wiped when it is dropped.
#[derive(Clone)]
pub struct Credential(Zeroizing<String>);

impl Credential {
    pub fn new(secret: String) -> Self {
        Self(Zeroizing::new(secret))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credential([redacted])")
    }
}

impl Serialize for Credential {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Credential {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::new)
    }
}

/// What a client asks the broker to do.
#[derive(Debug, Serialize, Deserialize)]
pub enum ProjectorRequest {
    /// Look for receivers for `timeout_ms`; answered with
    /// [`ProjectorEvent::Devices`] or [`ProjectorEvent::Failed`].
    Discover { timeout_ms: u64 },
    /// Project to `device`, stopping any running session first. Progress is
    /// [`ProjectorEvent::Connecting`], then [`ProjectorEvent::Connected`] or
    /// [`ProjectorEvent::Ended`] with the error.
    Connect {
        device: ProjectionDevice,
        options: ProjectionOptions,
        credential: Option<Credential>,
    },
    /// Stop the running session, if any; answered with
    /// [`ProjectorEvent::Idle`] once nothing runs, after the session's
    /// [`ProjectorEvent::Ended`].
    Disconnect,
}

/// How far a connection has got, for a picker that shows progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectStage {
    /// The broker accepted the request and is starting the session.
    Starting,
    /// Talking to the receiver: Cast/FCast connect, `AirPlay` pairing,
    /// Miracast negotiation.
    Handshake,
    /// Waiting for the compositor's first frames to fill the stream.
    WaitingForFrames,
    /// The receiver is loading the stream.
    Loading,
}

/// What the broker tells its clients.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectorEvent {
    /// Reply to [`ProjectorRequest::Discover`].
    Devices(Vec<ProjectionDevice>),
    /// A session to `device_id` reached `stage`.
    Connecting {
        device_id: String,
        stage: ConnectStage,
    },
    /// The receiver is showing the stream.
    Connected { device_id: String },
    /// The capture size, rate and bitrate the compositor should encode at.
    CaptureProfile(CaptureProfile),
    /// The session to `device_id` is over: stopped (`error` is `None`) or
    /// failed.
    Ended {
        device_id: String,
        error: Option<String>,
    },
    /// Reply to [`ProjectorRequest::Disconnect`]: no session runs.
    Idle,
    /// The request could not be carried out.
    Failed(String),
}

/// Writes one request. The encoded bytes may hold a credential, so they are
/// wiped once written.
///
/// # Errors
///
/// [`FrameError::Encode`] if the request cannot be encoded, otherwise as
/// [`Framing::write`].
pub fn write_request<W: Write + ?Sized>(
    writer: &mut W,
    request: &ProjectorRequest,
) -> Result<(), FrameError> {
    let bytes = Zeroizing::new(postcard::to_allocvec(request).map_err(FrameError::Encode)?);
    CONTROL_FRAMING.write(writer, &bytes)
}

/// Reads the next request, or `None` when the client closed the connection.
/// The received bytes may hold a credential, so they are wiped once decoded.
///
/// # Errors
///
/// [`FrameError::Decode`] for a malformed request, otherwise as
/// [`Framing::read`].
pub fn read_request<R: Read + ?Sized>(
    reader: &mut R,
) -> Result<Option<ProjectorRequest>, FrameError> {
    let Some(bytes) = CONTROL_FRAMING.read(reader)? else {
        return Ok(None);
    };
    let bytes = Zeroizing::new(bytes);
    postcard::from_bytes(&bytes)
        .map(Some)
        .map_err(FrameError::Decode)
}

/// Writes one event.
///
/// # Errors
///
/// As [`Framing::write_value`].
pub fn write_event<W: Write + ?Sized>(
    writer: &mut W,
    event: &ProjectorEvent,
) -> Result<(), FrameError> {
    CONTROL_FRAMING.write_value(writer, event)
}

/// Reads the next event, or `None` when the broker closed the connection.
///
/// # Errors
///
/// As [`Framing::read_value`].
pub fn read_event<R: Read + ?Sized>(reader: &mut R) -> Result<Option<ProjectorEvent>, FrameError> {
    CONTROL_FRAMING.read_value(reader)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProjectionProtocol;

    #[test]
    fn requests_and_events_round_trip_and_carry_the_credential_in_band() {
        let device = ProjectionDevice {
            id: "AirPlay:receiver".to_owned(),
            name: "Studio".to_owned(),
            protocol: ProjectionProtocol::AirPlay,
            detail: "Local network".to_owned(),
            modes: vec![crate::ProjectionMode::Mirror],
            addresses: vec!["192.0.2.7".to_owned()],
            port: 7000,
        };
        let mut wire = Vec::new();
        write_request(
            &mut wire,
            &ProjectorRequest::Connect {
                device: device.clone(),
                options: ProjectionOptions::default(),
                credential: Some(Credential::new("4821".to_owned())),
            },
        )
        .expect("encode connect");
        let Some(ProjectorRequest::Connect {
            device: decoded,
            credential,
            ..
        }) = read_request(&mut wire.as_slice()).expect("decode connect")
        else {
            panic!("expected a connect request");
        };
        assert_eq!(decoded, device);
        let credential = credential.expect("credential crosses the socket");
        assert_eq!(credential.expose(), "4821");
        assert!(!format!("{credential:?}").contains("4821"));

        let mut wire = Vec::new();
        let event = ProjectorEvent::Connecting {
            device_id: device.id,
            stage: ConnectStage::Handshake,
        };
        write_event(&mut wire, &event).expect("encode event");
        assert_eq!(
            read_event(&mut wire.as_slice()).expect("decode event"),
            Some(event)
        );
        assert_eq!(read_event(&mut [].as_slice()).expect("clean close"), None);
    }
}
