//! The launcher's handoff on every service connection it hands an app.
//!
//! Before an app holds a service descriptor, the launcher has already written
//! one preamble on it: a magic that says which service the connection belongs
//! to, a length-framed typed envelope naming the app (and room) the launcher
//! launched, and a protocol number. The service reads that preamble before it
//! trusts anything the app sends, so an app cannot name another app or another
//! room on a connection opened on its behalf.
//!
//! Both ends of every such connection use this crate: one table
//! ([`CONTRACTS`]) of what each service's handoff is, one envelope type
//! ([`Handoff`]) it carries, and the framing of [`wire_frame::Framing`].
//! Changing a magic, a bound, the byte order of a length prefix or what an
//! envelope carries changes this file, and both peers compile it.
//!
//! The bytes are the wire, not an implementation detail: they are pinned by
//! the tests here and read by services that reject anything else. The magic
//! carries no authority — each service still binds the connection to its own
//! peer credentials and launch record — it only says which handoff to parse.
//!
//! The collaboration and compatibility services read their handoff through the
//! launcher's re-export of this crate, so they keep its one definition without
//! a dependency of their own. Not every service connection is described here:
//! the identity broker (`tilo-identity-client`), the render broker
//! (`tilo_render_protocol::broker`), the chooser and the shell-UI connection
//! each carry their own preamble contract beside their own protocol.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};
use wire_frame::{FrameError, Framing};

/// Manifest name of the collaboration service (`[sandbox.services] collab`).
///
/// The name is the single point that decides where the collab socket lives,
/// with two owners on opposite sides of a namespace boundary: the compositor
/// *binds* `socket_for` of this name and the launcher *connects* it. They
/// were once two independently written path expressions, so the compositor
/// bound a socket nobody dialled and every app quietly ran with
/// collaboration off.
pub const COLLAB_SERVICE: &str = "collab";
/// Manifest name of the Windows compatibility service, handed only to the
/// one app that drives the compatibility VM.
pub const COMPAT_SERVICE: &str = "compat";
/// The launcher service name of document media; the app finds the descriptor
/// in `TILO_SVC_BLOBS_FD`. Not declared by manifests: every app is handed a
/// capability scoped to itself and its room, so a document's images live in
/// the device content store rather than in the app's sandbox.
pub const BLOB_SERVICE: &str = "blobs";
/// The package-management door only the Store, Browser and Settings hold;
/// the host decides by the app id stamped on the connection.
pub const APP_HOST_SERVICE: &str = "app-host";
/// The shell runtime's supervisor. Its connection is stamped with the app id
/// before the app holds it, so the supervisor knows which app is asking —
/// the device's own shell is answered for Terminal and nobody else.
pub const USH_SERVICE: &str = "ush";
/// The per-user, desktop-owned model gateway. The launcher authenticates the
/// consuming package before handing it the connected descriptor, so a same-UID
/// process cannot manufacture a Settings credential operation by merely
/// connecting to the socket.
pub const MODEL_GATEWAY_SERVICE: &str = "model-gateway";

/// Where the filesystem daemon listens for document-media connections:
/// group-connectable by the signed-in device user only, and never bound into
/// an app sandbox.
pub const DEFAULT_BLOB_SOCKET: &str = "/run/tilo/fs-blobs.sock";
/// Overrides [`DEFAULT_BLOB_SOCKET`] for sessions whose daemon runs elsewhere.
pub const BLOB_SOCKET_ENV: &str = "TILO_FS_BLOB_SOCKET";

/// Prefix that separates a launcher-authenticated collaboration connection
/// from a raw Cap'n Proto diagnostic client.
pub const COLLAB_LAUNCH_MAGIC: [u8; 4] = *b"QCL1";
/// After [`COLLAB_LAUNCH_MAGIC`]: a little-endian `u32` length, then the JSON
/// envelope. Deliberately tiny: it only binds a socket to the shell's
/// already-created process record.
pub const COLLAB_LAUNCH_FRAMING: Framing = Framing::little_endian(1024).reject_empty();
pub const COLLAB_LAUNCH_PROTOCOL_VERSION: u16 = 1;

/// The compatibility service's handoff: a little-endian `u32` length, then
/// JSON. No magic: the descriptor is only ever handed to the one app that
/// speaks it.
pub const COMPAT_HANDOFF_FRAMING: Framing = Framing::little_endian(4 * 1024).reject_empty();
pub const COMPAT_PROTOCOL_VERSION: u16 = 1;

/// Prefix of the document-media handoff; the daemon reads it before any
/// Cap'n Proto.
pub const BLOB_HANDOFF_MAGIC: [u8; 4] = *b"QBL1";
/// After [`BLOB_HANDOFF_MAGIC`]: a little-endian `u32` length, then the JSON
/// envelope.
pub const BLOB_HANDOFF_FRAMING: Framing = Framing::little_endian(4096).reject_empty();
pub const BLOB_HANDOFF_PROTOCOL: u16 = 1;
/// In-sandbox descriptor slot of the automatic document-media service. The
/// slot is part of the contract: the app reads the number from
/// `TILO_SVC_BLOBS_FD`, and the daemon's scope comes from the handoff written
/// at it.
pub const BLOB_CHILD_FD: i32 = 30;
/// Largest one component of a document-media scope may be, in bytes.
pub const MAX_SCOPE_COMPONENT: usize = 128;

/// Prefix of the app-host handoff.
pub const APP_HOST_LAUNCH_MAGIC: [u8; 4] = *b"QAH1";
/// After [`APP_HOST_LAUNCH_MAGIC`]: a big-endian `u32` length, then the JSON
/// envelope.
pub const APP_HOST_HANDOFF_FRAMING: Framing = Framing::big_endian(1024).reject_empty();
pub const APP_HOST_HANDOFF_PROTOCOL: u16 = 1;

/// Prefix of the `ush` handoff; the supervisor reads it before any request.
pub const USH_LAUNCH_MAGIC: [u8; 4] = *b"QUS1";
/// After [`USH_LAUNCH_MAGIC`]: a little-endian `u32` length, then the JSON
/// envelope.
pub const USH_HANDOFF_FRAMING: Framing = Framing::little_endian(1024).reject_empty();
pub const USH_HANDOFF_PROTOCOL: u16 = 1;

/// The model gateway's handoff: a little-endian `u32` length, then the JSON
/// envelope. No magic: the gateway is the last thing on its own socket.
pub const MODEL_GATEWAY_HANDOFF_FRAMING: Framing = Framing::little_endian(4 * 1024);
pub const MODEL_GATEWAY_HANDOFF_PROTOCOL: u16 = 1;

/// Which envelope a service's handoff carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Envelope {
    /// The app the connection was opened for.
    App,
    /// The app and the room it was launched into.
    Room,
    /// A document-media principal: one app in one room, or the session.
    Blob,
}

/// One service's launcher handoff, as both ends of its socket agree it.
#[derive(Clone, Copy, Debug)]
pub struct Contract {
    /// The service name, as a manifest and the launcher's socket path use it.
    pub service: &'static str,
    /// The prefix on the connection before the frame; empty when the service
    /// reads a bare frame.
    pub magic: &'static [u8],
    /// Byte order and bound of the length prefix.
    pub framing: Framing,
    /// The protocol number inside the envelope.
    pub protocol: u16,
    /// The envelope this service reads.
    pub envelope: Envelope,
    /// The in-sandbox descriptor slot, for a service the launcher connects
    /// without the app declaring it.
    pub child_fd: Option<i32>,
}

/// Every length-framed launcher handoff, by service name.
pub const CONTRACTS: &[Contract] = &[COLLAB, COMPAT, BLOBS, APP_HOST, USH, MODEL_GATEWAY];

/// Collaboration: app and room, after [`COLLAB_LAUNCH_MAGIC`].
pub const COLLAB: Contract = Contract {
    service: COLLAB_SERVICE,
    magic: &COLLAB_LAUNCH_MAGIC,
    framing: COLLAB_LAUNCH_FRAMING,
    protocol: COLLAB_LAUNCH_PROTOCOL_VERSION,
    envelope: Envelope::Room,
    child_fd: None,
};

/// Windows compatibility: app and room, bare frame.
pub const COMPAT: Contract = Contract {
    service: COMPAT_SERVICE,
    magic: &[],
    framing: COMPAT_HANDOFF_FRAMING,
    protocol: COMPAT_PROTOCOL_VERSION,
    envelope: Envelope::Room,
    child_fd: None,
};

/// Document media: one scope, at [`BLOB_CHILD_FD`].
pub const BLOBS: Contract = Contract {
    service: BLOB_SERVICE,
    magic: &BLOB_HANDOFF_MAGIC,
    framing: BLOB_HANDOFF_FRAMING,
    protocol: BLOB_HANDOFF_PROTOCOL,
    envelope: Envelope::Blob,
    child_fd: Some(BLOB_CHILD_FD),
};

/// The package store's door: app, after [`APP_HOST_LAUNCH_MAGIC`].
pub const APP_HOST: Contract = Contract {
    service: APP_HOST_SERVICE,
    magic: &APP_HOST_LAUNCH_MAGIC,
    framing: APP_HOST_HANDOFF_FRAMING,
    protocol: APP_HOST_HANDOFF_PROTOCOL,
    envelope: Envelope::App,
    child_fd: None,
};

/// The shell runtime supervisor: app, after [`USH_LAUNCH_MAGIC`].
pub const USH: Contract = Contract {
    service: USH_SERVICE,
    magic: &USH_LAUNCH_MAGIC,
    framing: USH_HANDOFF_FRAMING,
    protocol: USH_HANDOFF_PROTOCOL,
    envelope: Envelope::App,
    child_fd: None,
};

/// The model gateway: app, bare frame.
pub const MODEL_GATEWAY: Contract = Contract {
    service: MODEL_GATEWAY_SERVICE,
    magic: &[],
    framing: MODEL_GATEWAY_HANDOFF_FRAMING,
    protocol: MODEL_GATEWAY_HANDOFF_PROTOCOL,
    envelope: Envelope::App,
    child_fd: None,
};

/// The contract of a service, by the name its socket is named after.
#[must_use]
pub fn contract(service: &str) -> Option<&'static Contract> {
    CONTRACTS.iter().find(|entry| entry.service == service)
}

/// Who a document-media connection acts for, as its handoff fixes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlobScope {
    /// A sandboxed app in one room.
    App { room: String, app_id: String },
    /// The desktop session.
    Session,
}

/// What a handoff names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Handoff {
    /// `{"protocol":1,"app_id":"…"}`
    App { app_id: String },
    /// `{"protocol":1,"app_id":"…","workspace_id":"…"}`, with
    /// `"executing_peer":true` when the connection asks for executing-task
    /// custody.
    Room {
        app_id: String,
        workspace_id: String,
        executing_peer: bool,
    },
    /// `{"protocol":1,"principal":"app","room":"…","app_id":"…"}` or
    /// `{"protocol":1,"principal":"session"}`.
    Blob(BlobScope),
}

impl Handoff {
    /// A handoff naming one app.
    #[must_use]
    pub fn app(app_id: impl Into<String>) -> Self {
        Self::App {
            app_id: app_id.into(),
        }
    }

    /// A handoff naming one app in one room.
    #[must_use]
    pub fn room(app_id: impl Into<String>, workspace_id: impl Into<String>) -> Self {
        Self::Room {
            app_id: app_id.into(),
            workspace_id: workspace_id.into(),
            executing_peer: false,
        }
    }

    /// The same room handoff, asking for exact executing-task custody.
    #[must_use]
    pub fn with_executing_peer(self) -> Self {
        match self {
            Self::Room {
                app_id,
                workspace_id,
                ..
            } => Self::Room {
                app_id,
                workspace_id,
                executing_peer: true,
            },
            other => other,
        }
    }
}

/// Why a handoff could not be written or read.
#[derive(Debug, thiserror::Error)]
pub enum HandoffError {
    #[error("handoff stream: {0}")]
    Io(#[from] io::Error),
    #[error("handoff frame: {0}")]
    Frame(#[from] FrameError),
    /// The stream closed before any of the handoff arrived.
    #[error("the connection closed before its handoff")]
    Missing,
    /// The connection starts with something that is not this handoff —
    /// carrying whatever was read of it, for a service that also accepts a
    /// bare first message.
    #[error("the connection does not start with this service's handoff")]
    NotHandoff { carried: Vec<u8> },
    /// The magic started but did not arrive whole or did not match.
    #[error("the handoff marker is truncated or does not match")]
    Marker,
    /// The envelope is well-formed JSON of the wrong shape, names an empty
    /// identifier, or carries fields this contract does not define.
    #[error("the handoff is not the envelope this service reads")]
    Malformed,
    /// The envelope's shape is not one this service carries at all.
    #[error("the handoff's shape does not belong to this service")]
    Envelope,
    /// Another protocol number than this service speaks.
    #[error("handoff protocol {found} is not {expected}")]
    Protocol { found: u16, expected: u16 },
    /// A principal the document-media contract does not define.
    #[error("the handoff names an unknown principal")]
    Principal,
}

impl From<HandoffError> for io::Error {
    fn from(error: HandoffError) -> Self {
        match error {
            HandoffError::Io(error) => error,
            HandoffError::Frame(error) => error.into(),
            other => {
                let kind = match &other {
                    HandoffError::Missing | HandoffError::Marker => io::ErrorKind::UnexpectedEof,
                    _ => io::ErrorKind::InvalidData,
                };
                Self::new(kind, other)
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppEnvelope {
    protocol: u16,
    app_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoomEnvelope {
    protocol: u16,
    app_id: String,
    workspace_id: String,
    #[serde(default, skip_serializing_if = "is_false")]
    executing_peer: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlobEnvelope {
    protocol: u16,
    principal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    room: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    app_id: Option<String>,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the field by reference"
)]
const fn is_false(value: &bool) -> bool {
    !*value
}

/// One component of a document-media scope: `[A-Za-z0-9._-]`, not only dots,
/// at most [`MAX_SCOPE_COMPONENT`] bytes. The components key the daemon's
/// index, so they must not be able to alias one another.
#[must_use]
pub fn valid_scope_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SCOPE_COMPONENT
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        && !value.bytes().all(|byte| byte == b'.')
}

impl Contract {
    /// The envelope bytes for `handoff`, refusing one this service does not
    /// carry: where a caller learns it is about to write the wrong handoff for
    /// the connection it holds.
    fn payload(&self, handoff: &Handoff) -> Result<Vec<u8>, HandoffError> {
        match (self.envelope, handoff) {
            (Envelope::App, Handoff::App { app_id }) => {
                require_identifier(app_id)?;
                to_bytes(&AppEnvelope {
                    protocol: self.protocol,
                    app_id: app_id.clone(),
                })
            }
            (
                Envelope::Room,
                Handoff::Room {
                    app_id,
                    workspace_id,
                    executing_peer,
                },
            ) => {
                require_identifier(app_id)?;
                require_identifier(workspace_id)?;
                to_bytes(&RoomEnvelope {
                    protocol: self.protocol,
                    app_id: app_id.clone(),
                    workspace_id: workspace_id.clone(),
                    executing_peer: *executing_peer,
                })
            }
            (Envelope::Blob, Handoff::Blob(BlobScope::App { room, app_id })) => {
                if !valid_scope_component(room) || !valid_scope_component(app_id) {
                    return Err(HandoffError::Malformed);
                }
                to_bytes(&BlobEnvelope {
                    protocol: self.protocol,
                    principal: "app".to_owned(),
                    room: Some(room.clone()),
                    app_id: Some(app_id.clone()),
                })
            }
            (Envelope::Blob, Handoff::Blob(BlobScope::Session)) => to_bytes(&BlobEnvelope {
                protocol: self.protocol,
                principal: "session".to_owned(),
                room: None,
                app_id: None,
            }),
            _ => Err(HandoffError::Envelope),
        }
    }

    /// The whole handoff — magic, length prefix, envelope — ready for one
    /// write.
    ///
    /// # Errors
    ///
    /// [`HandoffError::Envelope`] for another service's shape,
    /// [`HandoffError::Malformed`] for an empty or unsafe identifier, and
    /// [`HandoffError::Frame`] when the envelope does not fit this service's
    /// bound.
    pub fn encode(&self, handoff: &Handoff) -> Result<Vec<u8>, HandoffError> {
        let payload = self.payload(handoff)?;
        let frame = self.framing.encode(&payload)?;
        let mut out = Vec::with_capacity(self.magic.len().saturating_add(frame.len()));
        out.extend_from_slice(self.magic);
        out.extend_from_slice(&frame);
        Ok(out)
    }

    /// Write the whole handoff and flush it: the service must have it before
    /// the app holds the descriptor.
    ///
    /// # Errors
    ///
    /// As [`Self::encode`], or the stream refuses the write.
    pub fn write(&self, out: &mut impl Write, handoff: &Handoff) -> io::Result<()> {
        out.write_all(&self.encode(handoff)?)?;
        out.flush()
    }

    /// Write the whole handoff on a tokio stream and flush it.
    ///
    /// # Errors
    ///
    /// As [`Self::encode`], or the stream refuses the write.
    #[cfg(feature = "tokio")]
    pub async fn write_async<W>(&self, out: &mut W, handoff: &Handoff) -> io::Result<()>
    where
        W: tokio::io::AsyncWrite + Unpin + ?Sized,
    {
        use tokio::io::AsyncWriteExt as _;
        out.write_all(&self.encode(handoff)?).await?;
        out.flush().await
    }

    /// Read and check one handoff.
    ///
    /// # Errors
    ///
    /// [`HandoffError::Missing`] when the connection closed first,
    /// [`HandoffError::NotHandoff`] when it starts with something else (a
    /// legacy client's first message), and the frame, protocol, envelope,
    /// principal and scope errors for a handoff this service must refuse.
    pub fn read(&self, input: &mut impl Read) -> Result<Handoff, HandoffError> {
        self.read_marker(input)?;
        let bytes = self.framing.read(input)?.ok_or(HandoffError::Missing)?;
        self.parse(&bytes)
    }

    /// Read and check one handoff from a tokio stream.
    ///
    /// # Errors
    ///
    /// As [`Self::read`].
    #[cfg(feature = "tokio")]
    pub async fn read_async<R>(&self, input: &mut R) -> Result<Handoff, HandoffError>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
    {
        self.read_marker_async(input).await?;
        let bytes = self.framing.read_async(input).await?;
        let bytes = bytes.ok_or(HandoffError::Missing)?;
        self.parse(&bytes)
    }

    fn parse(&self, bytes: &[u8]) -> Result<Handoff, HandoffError> {
        match self.envelope {
            Envelope::App => {
                let envelope: AppEnvelope =
                    serde_json::from_slice(bytes).map_err(|_| HandoffError::Malformed)?;
                self.check_protocol(envelope.protocol)?;
                require_identifier(&envelope.app_id)?;
                Ok(Handoff::App {
                    app_id: envelope.app_id,
                })
            }
            Envelope::Room => {
                let envelope: RoomEnvelope =
                    serde_json::from_slice(bytes).map_err(|_| HandoffError::Malformed)?;
                self.check_protocol(envelope.protocol)?;
                require_identifier(&envelope.app_id)?;
                require_identifier(&envelope.workspace_id)?;
                Ok(Handoff::Room {
                    app_id: envelope.app_id,
                    workspace_id: envelope.workspace_id,
                    executing_peer: envelope.executing_peer,
                })
            }
            Envelope::Blob => {
                let envelope: BlobEnvelope =
                    serde_json::from_slice(bytes).map_err(|_| HandoffError::Malformed)?;
                self.check_protocol(envelope.protocol)?;
                match (envelope.principal.as_str(), envelope.room, envelope.app_id) {
                    ("app", Some(room), Some(app_id)) => {
                        if !valid_scope_component(&room) || !valid_scope_component(&app_id) {
                            return Err(HandoffError::Malformed);
                        }
                        Ok(Handoff::Blob(BlobScope::App { room, app_id }))
                    }
                    ("session", None, None) => Ok(Handoff::Blob(BlobScope::Session)),
                    _ => Err(HandoffError::Principal),
                }
            }
        }
    }

    fn check_protocol(&self, found: u16) -> Result<(), HandoffError> {
        if found == self.protocol {
            Ok(())
        } else {
            Err(HandoffError::Protocol {
                found,
                expected: self.protocol,
            })
        }
    }

    fn read_marker(&self, input: &mut impl Read) -> Result<(), HandoffError> {
        let Some((&first, rest)) = self.magic.split_first() else {
            return Ok(());
        };
        let Some(byte) = read_byte(input)? else {
            return Err(HandoffError::Missing);
        };
        if byte != first {
            return Err(HandoffError::NotHandoff {
                carried: vec![byte],
            });
        }
        for &expected in rest {
            let Some(byte) = read_byte(input)? else {
                return Err(HandoffError::Marker);
            };
            if byte != expected {
                return Err(HandoffError::Marker);
            }
        }
        Ok(())
    }

    #[cfg(feature = "tokio")]
    async fn read_marker_async<R>(&self, input: &mut R) -> Result<(), HandoffError>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
    {
        use tokio::io::AsyncReadExt as _;
        let Some((&first, rest)) = self.magic.split_first() else {
            return Ok(());
        };
        let mut byte = [0_u8; 1];
        if input.read(&mut byte).await? == 0 {
            return Err(HandoffError::Missing);
        }
        if byte[0] != first {
            return Err(HandoffError::NotHandoff {
                carried: vec![byte[0]],
            });
        }
        for &expected in rest {
            if input.read(&mut byte).await? == 0 {
                return Err(HandoffError::Marker);
            }
            if byte[0] != expected {
                return Err(HandoffError::Marker);
            }
        }
        Ok(())
    }
}

/// One byte, or `None` at a clean end of stream. Interrupted reads retry.
fn read_byte(input: &mut impl Read) -> io::Result<Option<u8>> {
    let mut byte = [0_u8; 1];
    loop {
        match input.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => return Ok(Some(byte[0])),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

fn require_identifier(value: &str) -> Result<(), HandoffError> {
    if value.is_empty() {
        Err(HandoffError::Malformed)
    } else {
        Ok(())
    }
}

fn to_bytes(value: &impl Serialize) -> Result<Vec<u8>, HandoffError> {
    serde_json::to_vec(value).map_err(|error| HandoffError::Io(io::Error::other(error)))
}

#[cfg(test)]
mod tests;
