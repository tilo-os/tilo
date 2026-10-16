//! A device's terminal into an environment on a relay.
//!
//! [`open_remote_environment`] starts a session from an environment record
//! and [`attach_remote_environment`] re-attaches to one by its id; both
//! return a [`PtyStream`]: the guest's output down, keystrokes and resizes
//! up. Dropping the stream (or [`PtyStream::detach`]) leaves the session
//! running on the relay; [`PtyStream::terminate`] ends it.
//!
//! The wire is [`tilo_relay_protocol::environment`] on the relay's compute
//! ALPN, after the same certified-device handshake every relay stream uses.

use iroh::endpoint::{Connection, Endpoint, RecvStream, SendStream};
use iroh_base::EndpointAddr;
use thiserror::Error;
use tilo_relay_protocol::auth::{self, AuthError, Hello, HelloAck};
use tilo_relay_protocol::environment::{
    ClientFrame, ENVIRONMENT_PROTOCOL_VERSION, EnvironmentRecordRef, EnvironmentSpec,
    MAX_ENVIRONMENT_FRAME_BYTES, MAX_PTY_CHUNK_BYTES, PtySize, Refusal, ServerFrame, SessionId,
    SessionRequest, StreamOpen, ToolImage, WindowFrame, WindowInput,
};
use tilo_relay_protocol::transport::COMPUTE_ALPN;
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::frame::{read_frame, read_frame_or_eof, write_frame};

/// Why a remote environment could not be reached or used.
#[derive(Debug, Error)]
pub enum EnvironmentClientError {
    /// Dialing or a stream failed.
    #[error("relay connection: {0}")]
    Connection(String),
    /// A frame did not encode or decode.
    #[error("environment codec: {0}")]
    Codec(String),
    /// The relay's identity did not verify.
    #[error("relay authentication: {0}")]
    Auth(#[from] AuthError),
    /// The relay refused the session.
    #[error("the relay refused the session ({reason:?}): {detail}")]
    Refused {
        /// Why.
        reason: Refusal,
        /// The relay's explanation.
        detail: String,
    },
    /// The relay broke the protocol.
    #[error("environment protocol: {0}")]
    Protocol(String),
}

impl From<std::io::Error> for EnvironmentClientError {
    fn from(error: std::io::Error) -> Self {
        Self::Connection(error.to_string())
    }
}

/// The relay to reach, and who this device is.
#[derive(Clone)]
pub struct RemoteRelay {
    /// This device's endpoint.
    pub endpoint: Endpoint,
    /// The relay's endpoint address (its id pins it).
    pub address: EndpointAddr,
    /// The relay's user root, when known; `None` accepts any valid host
    /// certificate behind the pinned endpoint (a provider's relay).
    pub relay_user: Option<[u8; 32]>,
    /// This device's certificate.
    pub cert: DeviceCert,
    /// This device's key.
    pub signing_key: ed25519_dalek::SigningKey,
    /// Revoked devices.
    pub revocations: RevocationList,
}

/// One event from a remote session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PtyEvent {
    /// Guest output.
    Output(Vec<u8>),
    /// The guest ended; the session is gone.
    Exited(Option<i32>),
    /// Another terminal attached to the session; this one is done.
    Superseded,
    /// A tool was built for the environment on the relay; the device
    /// records it with the record's `add-tool` action.
    ToolBuilt {
        /// The environment.
        record: EnvironmentRecordRef,
        /// The tool image.
        tool: ToolImage,
    },
}

/// A terminal attached to a session on a relay.
pub struct PtyStream {
    session: SessionId,
    resumed: bool,
    scrollback: Vec<u8>,
    writer: PtyWriter,
    reader: PtyReader,
}

/// The sending half of a [`PtyStream`].
pub struct PtyWriter {
    send: SendStream,
}

/// The receiving half of a [`PtyStream`]. It keeps the connection, and the
/// authenticated control stream, alive.
pub struct PtyReader {
    receive: RecvStream,
    _control: (SendStream, RecvStream),
    _connection: Connection,
}

/// Start a session from an environment record on `relay`.
///
/// # Errors
///
/// Returns an error when the relay cannot be reached or authenticated, or
/// refuses the session.
pub async fn open_remote_environment(
    relay: &RemoteRelay,
    record: &EnvironmentRecordRef,
    spec: &EnvironmentSpec,
    size: PtySize,
    term: Option<String>,
) -> Result<PtyStream, EnvironmentClientError> {
    connect(
        relay,
        record.workspace_id,
        SessionRequest::Open {
            record: record.clone(),
            spec: spec.clone(),
            size,
            term,
        },
    )
    .await
}

/// Re-attach to a session on `relay` by its id, receiving its scrollback.
///
/// # Errors
///
/// Returns an error when the relay cannot be reached or authenticated, or
/// refuses the attachment (unknown session, someone else's session).
pub async fn attach_remote_environment(
    relay: &RemoteRelay,
    workspace_id: u64,
    session: SessionId,
    size: PtySize,
) -> Result<PtyStream, EnvironmentClientError> {
    connect(
        relay,
        workspace_id,
        SessionRequest::Attach { session, size },
    )
    .await
}

async fn connect(
    relay: &RemoteRelay,
    workspace_id: u64,
    request: SessionRequest,
) -> Result<PtyStream, EnvironmentClientError> {
    let connection = relay
        .endpoint
        .connect(relay.address.clone(), COMPUTE_ALPN)
        .await
        .map_err(|error| EnvironmentClientError::Connection(error.to_string()))?;
    let control = handshake(relay, &connection).await?;
    let (mut send, mut receive) = connection
        .open_bi()
        .await
        .map_err(|error| EnvironmentClientError::Connection(error.to_string()))?;
    write_message(
        &mut send,
        &StreamOpen::Environment {
            version: ENVIRONMENT_PROTOCOL_VERSION,
            workspace_id,
            request,
        },
    )
    .await?;
    let first: ServerFrame = read_message(&mut receive)
        .await?
        .ok_or_else(|| EnvironmentClientError::Protocol("the relay closed the stream".into()))?;
    match first {
        ServerFrame::Opened {
            session,
            resumed,
            scrollback,
        } => Ok(PtyStream {
            session,
            resumed,
            scrollback,
            writer: PtyWriter { send },
            reader: PtyReader {
                receive,
                _control: control,
                _connection: connection,
            },
        }),
        ServerFrame::Refused { reason, detail } => {
            Err(EnvironmentClientError::Refused { reason, detail })
        }
        other => Err(EnvironmentClientError::Protocol(format!(
            "expected the session to open, got {other:?}"
        ))),
    }
}

async fn handshake(
    relay: &RemoteRelay,
    connection: &Connection,
) -> Result<(SendStream, RecvStream), EnvironmentClientError> {
    let (mut send, mut receive) = connection
        .open_bi()
        .await
        .map_err(|error| EnvironmentClientError::Connection(error.to_string()))?;
    let mut client_nonce = [0u8; 32];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut client_nonce);
    let hello = Hello {
        cert: relay.cert.clone(),
        client_nonce,
    };
    write_frame(&mut send, &encode(&hello)?).await?;
    let ack: HelloAck = decode(&read_frame(&mut receive, MAX_ENVIRONMENT_FRAME_BYTES).await?)?;
    let now = tilo_clock::unix_now_ns();
    match relay.relay_user.as_ref() {
        Some(user) => {
            auth::verify_hello_ack(&ack, user, &relay.revocations, now, &client_nonce)?;
        }
        None => {
            auth::verify_pinned_host_ack(&ack, &relay.revocations, now, &client_nonce)?;
        }
    }
    let ack2 = auth::build_hello_ack2(&relay.signing_key, &ack.server_nonce);
    write_frame(&mut send, &encode(&ack2)?).await?;
    Ok((send, receive))
}

impl PtyStream {
    /// The session's id, for re-attaching later.
    #[must_use]
    pub fn session(&self) -> SessionId {
        self.session
    }

    /// Whether this attached to a session that was already running.
    #[must_use]
    pub fn resumed(&self) -> bool {
        self.resumed
    }

    /// The output the session kept while no terminal was attached (empty
    /// for a new session); returned once.
    pub fn take_scrollback(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.scrollback)
    }

    /// Type `bytes` into the session.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream is gone.
    pub async fn write(&mut self, bytes: &[u8]) -> Result<(), EnvironmentClientError> {
        self.writer.write(bytes).await
    }

    /// Resize the session's terminal.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream is gone.
    pub async fn resize(&mut self, size: PtySize) -> Result<(), EnvironmentClientError> {
        self.writer.resize(size).await
    }

    /// The next event; `None` once the relay closed the stream.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream fails or a frame is malformed.
    pub async fn next(&mut self) -> Result<Option<PtyEvent>, EnvironmentClientError> {
        self.reader.next().await
    }

    /// Leave the session running on the relay.
    pub async fn detach(self) {
        self.writer.detach().await;
    }

    /// End the session.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream is gone.
    pub async fn terminate(&mut self) -> Result<(), EnvironmentClientError> {
        self.writer.terminate().await
    }

    /// Split into halves that can be driven from separate tasks.
    #[must_use]
    pub fn split(self) -> (PtyWriter, PtyReader) {
        (self.writer, self.reader)
    }
}

impl PtyWriter {
    /// Type `bytes` into the session.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream is gone.
    pub async fn write(&mut self, bytes: &[u8]) -> Result<(), EnvironmentClientError> {
        for chunk in bytes.chunks(MAX_PTY_CHUNK_BYTES) {
            write_message(&mut self.send, &ClientFrame::Input(chunk.to_vec())).await?;
        }
        Ok(())
    }

    /// Resize the session's terminal.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream is gone.
    pub async fn resize(&mut self, size: PtySize) -> Result<(), EnvironmentClientError> {
        write_message(&mut self.send, &ClientFrame::Resize(size)).await
    }

    /// End the session.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream is gone.
    pub async fn terminate(&mut self) -> Result<(), EnvironmentClientError> {
        write_message(&mut self.send, &ClientFrame::Terminate).await
    }

    /// Leave the session running on the relay.
    pub async fn detach(mut self) {
        let _ = write_message(&mut self.send, &ClientFrame::Detach).await;
        let _ = self.send.finish();
    }
}

impl PtyReader {
    /// The next event; `None` once the relay closed the stream.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream fails or a frame is malformed.
    pub async fn next(&mut self) -> Result<Option<PtyEvent>, EnvironmentClientError> {
        let Some(frame) = read_message::<ServerFrame>(&mut self.receive).await? else {
            return Ok(None);
        };
        Ok(Some(match frame {
            ServerFrame::Output(bytes) => PtyEvent::Output(bytes),
            ServerFrame::Exited { code } => PtyEvent::Exited(code),
            ServerFrame::Superseded => PtyEvent::Superseded,
            ServerFrame::ToolBuilt { record, tool } => PtyEvent::ToolBuilt { record, tool },
            ServerFrame::Refused { reason, detail } => {
                return Err(EnvironmentClientError::Refused { reason, detail });
            }
            ServerFrame::Opened { .. } => {
                return Err(EnvironmentClientError::Protocol(
                    "the session opened twice".into(),
                ));
            }
        }))
    }
}

/// The windows of the member's legacy apps launched from relay
/// environments, and the way to send them input.
pub struct LegacyWindowStream {
    send: SendStream,
    receive: RecvStream,
    _control: (SendStream, RecvStream),
    _connection: Connection,
}

/// Watch the windows of this member's legacy apps launched from relay
/// environments in `workspace_id`. Every launch after this returns reaches
/// the stream.
///
/// # Errors
///
/// Returns an error when the relay cannot be reached or authenticated.
pub async fn open_legacy_windows(
    relay: &RemoteRelay,
    workspace_id: u64,
) -> Result<LegacyWindowStream, EnvironmentClientError> {
    let connection = relay
        .endpoint
        .connect(relay.address.clone(), COMPUTE_ALPN)
        .await
        .map_err(|error| EnvironmentClientError::Connection(error.to_string()))?;
    let control = handshake(relay, &connection).await?;
    let (mut send, receive) = connection
        .open_bi()
        .await
        .map_err(|error| EnvironmentClientError::Connection(error.to_string()))?;
    write_message(
        &mut send,
        &StreamOpen::LegacyWindows {
            version: ENVIRONMENT_PROTOCOL_VERSION,
            workspace_id,
        },
    )
    .await?;
    let mut stream = LegacyWindowStream {
        send,
        receive,
        _control: control,
        _connection: connection,
    };
    match stream.next().await? {
        Some(WindowFrame::Watching) => Ok(stream),
        other => Err(EnvironmentClientError::Protocol(format!(
            "expected the window stream to be registered, got {other:?}"
        ))),
    }
}

impl LegacyWindowStream {
    /// The next window frame; `None` once the relay closed the stream.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream fails, a frame is malformed, or the
    /// relay refused the stream.
    pub async fn next(&mut self) -> Result<Option<WindowFrame>, EnvironmentClientError> {
        let Some(bytes) = read_frame_or_eof(&mut self.receive, MAX_ENVIRONMENT_FRAME_BYTES)
            .await
            .map_err(|error| EnvironmentClientError::Connection(error.to_string()))?
        else {
            return Ok(None);
        };
        if let Ok(ServerFrame::Refused { reason, detail }) = decode::<ServerFrame>(&bytes) {
            return Err(EnvironmentClientError::Refused { reason, detail });
        }
        decode(&bytes).map(Some)
    }

    /// Send input to one of the windows.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream is gone.
    pub async fn send_input(&mut self, input: &WindowInput) -> Result<(), EnvironmentClientError> {
        write_message(&mut self.send, input).await
    }
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, EnvironmentClientError> {
    let bytes = postcard::to_allocvec(value)
        .map_err(|error| EnvironmentClientError::Codec(error.to_string()))?;
    if bytes.len() > MAX_ENVIRONMENT_FRAME_BYTES {
        return Err(EnvironmentClientError::Codec(
            "frame exceeds its bound".into(),
        ));
    }
    Ok(bytes)
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, EnvironmentClientError> {
    postcard::from_bytes(bytes).map_err(|error| EnvironmentClientError::Codec(error.to_string()))
}

async fn write_message<T: serde::Serialize + Sync>(
    send: &mut SendStream,
    value: &T,
) -> Result<(), EnvironmentClientError> {
    write_frame(send, &encode(value)?).await?;
    Ok(())
}

async fn read_message<T: serde::de::DeserializeOwned>(
    receive: &mut RecvStream,
) -> Result<Option<T>, EnvironmentClientError> {
    match read_frame_or_eof(receive, MAX_ENVIRONMENT_FRAME_BYTES).await {
        Ok(Some(bytes)) => decode(&bytes).map(Some),
        Ok(None) => Ok(None),
        Err(error) => Err(EnvironmentClientError::Connection(error.to_string())),
    }
}
