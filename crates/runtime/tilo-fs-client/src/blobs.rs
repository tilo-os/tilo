use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    io::{self, Read as _},
    os::unix::net::UnixStream as StdUnixStream,
    path::Path,
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt as _;
use wire_frame::Framing;

use crate::{
    connection::{Connection as Transport, Object},
    wire::{FsReply, FsRequest, ROOT},
};

/// Where the root filesystem daemon listens for document-media connections.
/// Group-connectable by the signed-in device user only, and never bound into
/// an app sandbox.
pub const DEFAULT_BLOB_SOCKET: &str = "/run/tilo/fs-blobs.sock";
/// Launcher service name; the app finds its descriptor in `TILO_SVC_BLOBS_FD`.
pub const BLOB_SERVICE: &str = "blobs";
/// Prefix of the handoff frame that precedes the protocol on every connection.
pub const BLOB_HANDOFF_MAGIC: [u8; 4] = *b"QBL1";
pub const BLOB_HANDOFF_PROTOCOL: u16 = 1;
/// Largest payload one `put` message carries; larger content uses `upload`.
pub const MAX_PUT_CHUNK: usize = 4 * 1024 * 1024;
/// Largest range one `get` answers.
pub const MAX_READ_CHUNK: u32 = 4 * 1024 * 1024;
/// Largest single blob the capability accepts or reassembles. Document media
/// is images and attachments; bigger content belongs in a file record.
pub const MAX_BLOB_BYTES: u64 = 64 * 1024 * 1024;
/// Largest piece of an upload held at once: an upload writes one piece per
/// message, and the server passes it on in pieces of this size, so what waits
/// between the app and the store is bounded by pieces, not by whole messages.
///
/// The bound is in pieces at every hop: the message carrying one, the framed
/// connection decoding it, [`UPLOAD_PIECES_IN_FLIGHT`] here, and the content
/// store's queue of up to 32 more on their way to its writer (iroh-blobs'
/// import channel). How full that queue runs depends on how far the disk lags
/// the connection, so the piece size is what keeps the worst case small:
/// 64 KiB pieces put at most 2.25 MiB between the app and the disk, where
/// 256 KiB pieces let a slow writer hold 9 MiB.
pub const UPLOAD_PIECE: usize = 64 * 1024;
/// Pieces of one upload that may wait for the backend before the uploader's
/// next `write` is held back.
pub const UPLOAD_PIECES_IN_FLIGHT: usize = 4;
const HANDOFF_FRAMING: Framing = Framing::little_endian(4096).reject_empty();
const MAX_SCOPE_COMPONENT: usize = 128;
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Who a connection acts for, fixed by the handoff frame before the first RPC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlobPrincipal {
    /// A sandboxed app in a workspace: the connection's bootstrap is that scope's
    /// `DocumentBlobs`.
    App(BlobScope),
    /// The desktop session: the bootstrap is `SessionBlobs`.
    Session,
}

/// One app's document media in one workspace.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlobScope {
    workspace: String,
    app: String,
}

impl BlobScope {
    /// Validate and build a scope. Both parts must be a single path-safe
    /// component (`[A-Za-z0-9._-]`, not only dots, at most 128 bytes): they
    /// key the daemon's index and must not be able to alias one another.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::InvalidScope`] for anything else.
    pub fn new(workspace: impl Into<String>, app: impl Into<String>) -> Result<Self, BlobError> {
        let workspace = workspace.into();
        let app = app.into();
        if valid_scope_component(&workspace) && valid_scope_component(&app) {
            Ok(Self { workspace, app })
        } else {
            Err(BlobError::InvalidScope)
        }
    }

    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    pub fn app(&self) -> &str {
        &self.app
    }
}

fn valid_scope_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SCOPE_COMPONENT
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        && !value.bytes().all(|byte| byte == b'.')
}

/// True for a lowercase 64-character hex BLAKE3 digest, the only hash form
/// the capability accepts.
pub fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A stored blob, as a document records it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    pub hash: String,
    pub len: u64,
    pub mime: String,
}

/// The answer to a read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReadOutcome {
    Ready {
        bytes: Vec<u8>,
        total_len: u64,
    },
    /// This device is fetching the content from a collaborator; ask again.
    Pending,
    /// Not readable through this capability, now or later.
    Unavailable,
}

/// Why a blob operation failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlobError {
    InvalidHash,
    InvalidScope,
    TooLarge {
        len: u64,
        limit: u64,
    },
    /// The service answered and refused.
    Refused(String),
    /// No service, or the connection is gone.
    Disconnected(String),
    /// The caller's own reader failed while its content was being stored.
    Input(String),
}

impl std::fmt::Display for BlobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidHash => f.write_str("blob hash must be 64 lowercase hex characters"),
            Self::InvalidScope => f.write_str("blob scope names an invalid workspace or app"),
            Self::TooLarge { len, limit } => {
                write!(f, "blob is {len} bytes; the limit is {limit} bytes")
            }
            Self::Refused(message) => write!(f, "blob service refused: {message}"),
            Self::Disconnected(message) => write!(f, "blob service unavailable: {message}"),
            Self::Input(message) => write!(f, "reading the content to store failed: {message}"),
        }
    }
}

impl std::error::Error for BlobError {}

impl From<runtime_portal::NotAvailable> for BlobError {
    fn from(error: runtime_portal::NotAvailable) -> Self {
        match error {
            runtime_portal::NotAvailable::ConnectionFailed(error) => {
                Self::Disconnected(error.to_string())
            }
            error => Self::Refused(error.to_string()),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HandoffFrame {
    protocol: u16,
    principal: String,
    /// The launcher sends `workspace`; a frame written before it did said
    /// `room`, which is still read.
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "room")]
    workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    app_id: Option<String>,
}

/// Write the handoff that fixes a new connection's principal: the magic, a
/// little-endian `u32` length and a JSON object
/// `{"protocol":1,"principal":"app","workspace":…,"app_id":…}` or
/// `{"protocol":1,"principal":"session"}`. The launcher writes the same bytes.
pub fn write_handoff(stream: &mut impl io::Write, principal: &BlobPrincipal) -> io::Result<()> {
    let frame = match principal {
        BlobPrincipal::App(scope) => HandoffFrame {
            protocol: BLOB_HANDOFF_PROTOCOL,
            principal: "app".into(),
            workspace: Some(scope.workspace.clone()),
            app_id: Some(scope.app.clone()),
        },
        BlobPrincipal::Session => HandoffFrame {
            protocol: BLOB_HANDOFF_PROTOCOL,
            principal: "session".into(),
            workspace: None,
            app_id: None,
        },
    };
    let bytes = serde_json::to_vec(&frame).map_err(io::Error::other)?;
    stream.write_all(&BLOB_HANDOFF_MAGIC)?;
    HANDOFF_FRAMING.write(stream, &bytes)?;
    stream.flush()
}

/// Read and validate a handoff frame (server side).
pub async fn read_handoff<R>(reader: &mut R) -> io::Result<BlobPrincipal>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let invalid = |message: &str| io::Error::new(io::ErrorKind::InvalidData, message.to_owned());
    let mut magic = [0_u8; 4];
    reader.read_exact(&mut magic).await?;
    if magic != BLOB_HANDOFF_MAGIC {
        return Err(invalid("not a document-media handoff"));
    }
    let bytes = HANDOFF_FRAMING
        .read_async(reader)
        .await?
        .ok_or(io::ErrorKind::UnexpectedEof)?;
    let frame: HandoffFrame =
        serde_json::from_slice(&bytes).map_err(|_| invalid("malformed document-media handoff"))?;
    if frame.protocol != BLOB_HANDOFF_PROTOCOL {
        return Err(invalid("unsupported document-media handoff protocol"));
    }
    match (frame.principal.as_str(), frame.workspace, frame.app_id) {
        ("app", Some(workspace), Some(app)) => BlobScope::new(workspace, app)
            .map(BlobPrincipal::App)
            .map_err(|_| invalid("document-media handoff names an invalid scope")),
        ("session", None, None) => Ok(BlobPrincipal::Session),
        _ => Err(invalid("document-media handoff names an unknown principal")),
    }
}

/// A read of a public copy: the range, and which copy it is (its hash),
/// when there is one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicRead {
    pub outcome: ReadOutcome,
    pub hash: Option<String>,
}

/// The agent the session says causes this device's next writes to `paths`.
/// Presentation only: it grants nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteAttribution {
    /// Absolute paths under the workspace mount; a folder covers its subtree.
    pub paths: Vec<String>,
    /// `agent:<id>`.
    pub actor: String,
    /// How the agent is shown.
    pub label: String,
    /// Its presence colour, sRGB.
    pub color: [u8; 3],
    /// How long the lease lasts.
    pub ttl: Duration,
}

/// Most paths one write attribution names.
pub const MAX_ATTRIBUTION_PATHS: usize = 256;

type Answer<T> = std::sync::mpsc::SyncSender<Result<T, BlobError>>;

/// One request to the connection's driver thread. Every job is one round
/// trip: content larger than a message travels as a sequence of upload jobs,
/// so a long transfer never holds the driver while other callers wait.
enum Job {
    Store(StoreJob),
    Read {
        target: u64,
        hash: String,
        offset: u64,
        max_len: u32,
        reply: Answer<ReadOutcome>,
    },
    Has {
        target: u64,
        hash: String,
        reply: Answer<bool>,
    },
    Size {
        target: u64,
        hash: String,
        reply: Answer<Option<u64>>,
    },
    Scope {
        scope: BlobScope,
        reply: Answer<u64>,
    },
    Attribute {
        attribution: WriteAttribution,
        reply: Answer<()>,
    },
    Public(PublicJob),
}

/// Public content: kept, released and read by name.
enum PublicJob {
    Keep {
        target: u64,
        name: String,
        reply: Answer<()>,
    },
    Release {
        target: u64,
        name: String,
        reply: Answer<()>,
    },
    Read {
        target: u64,
        name: String,
        offset: u64,
        max_len: u32,
        reply: Answer<PublicRead>,
    },
    Refresh {
        reply: Answer<u32>,
    },
}

/// Storing content: one message, or an upload of several.
enum StoreJob {
    Put {
        target: u64,
        bytes: Vec<u8>,
        mime: String,
        reply: Answer<BlobRef>,
    },
    Begin {
        target: u64,
        mime: String,
        reply: Answer<u64>,
    },
    Write {
        upload: u64,
        bytes: Vec<u8>,
        reply: Answer<()>,
    },
    Finish {
        upload: u64,
        reply: Answer<BlobRef>,
    },
    /// Release an upload that will not finish; nothing it wrote is stored.
    Abort {
        upload: u64,
    },
}

struct Worker {
    jobs: tokio::sync::mpsc::UnboundedSender<Job>,
    session: bool,
}

/// A synchronous client for the document-media capability.
///
/// Cheap to clone; clones share one connection and its driver thread. Every
/// call blocks for at most one local round trip to the daemon (a remote
/// fetch answers [`ReadOutcome::Pending`] rather than waiting); streaming
/// calls ([`put_reader`](Self::put_reader), [`BlobReader`]) make one round
/// trip per chunk.
#[derive(Clone)]
pub struct BlobClient {
    worker: Arc<Worker>,
    target: u64,
}

impl std::fmt::Debug for BlobClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobClient")
            .field("session", &self.worker.session)
            .field("target", &self.target)
            .finish()
    }
}

impl BlobClient {
    /// The scoped capability the launcher handed this process, if any.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::Disconnected`] when no descriptor was inherited.
    pub fn from_environment() -> Result<Self, BlobError> {
        let connection = runtime_portal::connect_service_fd(BLOB_SERVICE).map_err(|error| {
            BlobError::Disconnected(match error {
                runtime_portal::NotAvailable::ServiceFdMissing => {
                    "no document media service".into()
                }
                error => error.to_string(),
            })
        })?;
        Self::start(connection.into(), false)
    }

    /// Wrap a connection whose handoff was already written for an app scope.
    /// Takes ownership of the stream.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::Disconnected`] if the driver thread cannot start.
    pub fn from_app_stream(stream: StdUnixStream) -> Result<Self, BlobError> {
        Self::start(stream, false)
    }

    /// Connect to the daemon at `socket` as the desktop session.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::Disconnected`] when the socket cannot be reached.
    pub fn connect_session(socket: &Path) -> Result<Self, BlobError> {
        let mut stream = StdUnixStream::connect(socket)
            .map_err(|error| BlobError::Disconnected(format!("{}: {error}", socket.display())))?;
        write_handoff(&mut stream, &BlobPrincipal::Session)
            .map_err(|error| BlobError::Disconnected(error.to_string()))?;
        Self::from_session_stream(stream)
    }

    /// Wrap a connection whose session handoff was already written.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::Disconnected`] if the driver thread cannot start.
    pub fn from_session_stream(stream: StdUnixStream) -> Result<Self, BlobError> {
        Self::start(stream, true)
    }

    /// For a session client: the capability of one app in one workspace.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::InvalidScope`] for an invalid scope or a client
    /// that is not a session root, or a connection error.
    pub fn scope(&self, workspace: &str, app: &str) -> Result<Self, BlobError> {
        if !self.worker.session || self.target != ROOT {
            return Err(BlobError::InvalidScope);
        }
        let scope = BlobScope::new(workspace, app)?;
        let target = self.call(|reply| Job::Scope { scope, reply })?;
        Ok(Self {
            worker: Arc::clone(&self.worker),
            target,
        })
    }

    /// For a session client: name the agent behind this device's next writes
    /// to `attribution.paths`, as the session authenticated it from the
    /// action those writes run for.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::InvalidScope`] for a client that is not a session
    /// root, [`BlobError::Refused`] for too many paths or the daemon's
    /// refusal, or a connection error.
    pub fn attribute_writes(&self, attribution: WriteAttribution) -> Result<(), BlobError> {
        if !self.worker.session || self.target != ROOT {
            return Err(BlobError::InvalidScope);
        }
        if attribution.paths.len() > MAX_ATTRIBUTION_PATHS {
            return Err(BlobError::Refused("too many attributed paths".into()));
        }
        self.call(|reply| Job::Attribute { attribution, reply })
    }

    fn data_target(&self) -> Result<u64, BlobError> {
        if self.worker.session && self.target == ROOT {
            Err(BlobError::InvalidScope)
        } else {
            Ok(self.target)
        }
    }

    /// Store `bytes`, streaming when they exceed one message.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::TooLarge`] above [`MAX_BLOB_BYTES`], or the
    /// service's refusal.
    pub fn put(&self, bytes: &[u8], mime: &str) -> Result<BlobRef, BlobError> {
        if bytes.len() as u64 > MAX_BLOB_BYTES {
            return Err(BlobError::TooLarge {
                len: bytes.len() as u64,
                limit: MAX_BLOB_BYTES,
            });
        }
        let mut reader = bytes;
        self.put_reader(&mut reader, mime)
    }

    /// Store everything `reader` yields, [`UPLOAD_PIECE`] at a time: this
    /// holds one piece, never the whole content, and hashes it as it goes so
    /// the reference the store answers is checked against the bytes sent.
    /// A piece is the unit every hop between here and the disk holds, so the
    /// message carrying it is the same size as the piece the store takes:
    /// framing and decoding a whole [`MAX_PUT_CHUNK`] would hold it twice on
    /// each side.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::TooLarge`] as soon as `reader` yields more than
    /// [`MAX_BLOB_BYTES`] (having read at most one byte past it),
    /// [`BlobError::Input`] when `reader` fails, a refusal when the store
    /// answers a reference for other content, or a connection error. A
    /// failed put stores nothing.
    pub fn put_reader(&self, reader: &mut dyn io::Read, mime: &str) -> Result<BlobRef, BlobError> {
        let target = self.data_target()?;
        let mut reader = reader.take(MAX_BLOB_BYTES.saturating_add(1));
        let mut hasher = blake3::Hasher::new();
        let mut total: u64 = 0;
        let mut upload: Option<OpenUpload<'_>> = None;
        loop {
            let chunk = next_chunk(&mut reader)?;
            total = total.saturating_add(chunk.len() as u64);
            if total > MAX_BLOB_BYTES {
                return Err(BlobError::TooLarge {
                    len: total,
                    limit: MAX_BLOB_BYTES,
                });
            }
            hasher.update(&chunk);
            let last = chunk.len() < UPLOAD_PIECE;
            let id = match upload.as_ref().map(|open| open.id) {
                Some(id) => id,
                None if last => {
                    // The whole content fits one message.
                    let mime = mime.to_owned();
                    let stored = self.call(|reply| {
                        Job::Store(StoreJob::Put {
                            target,
                            bytes: chunk,
                            mime,
                            reply,
                        })
                    })?;
                    return checked_ref(stored, &hasher, total);
                }
                None => {
                    let mime = mime.to_owned();
                    let id = self.call(|reply| {
                        Job::Store(StoreJob::Begin {
                            target,
                            mime,
                            reply,
                        })
                    })?;
                    upload = Some(OpenUpload {
                        client: self,
                        id,
                        open: true,
                    });
                    id
                }
            };
            if !chunk.is_empty() {
                self.call(|reply| {
                    Job::Store(StoreJob::Write {
                        upload: id,
                        bytes: chunk,
                        reply,
                    })
                })?;
            }
            if last {
                break;
            }
        }
        let Some(mut open) = upload else {
            return Err(BlobError::Refused("upload was never opened".into()));
        };
        open.open = false;
        let stored = self.call(|reply| {
            Job::Store(StoreJob::Finish {
                upload: open.id,
                reply,
            })
        })?;
        checked_ref(stored, &hasher, total)
    }

    /// Begin storing content handed over piece by piece
    /// ([`BlobUpload::append`]): for a caller that receives the content
    /// over its own transport rather than holding a reader. Hashed as it
    /// goes, as [`Self::put_reader`] does; dropped unfinished, the upload
    /// stores nothing.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::InvalidScope`] on a session root, the service's
    /// refusal, or a connection error.
    pub fn begin_upload(&self, mime: &str) -> Result<BlobUpload, BlobError> {
        let target = self.data_target()?;
        let mime = mime.to_owned();
        let id = self.call(|reply| {
            Job::Store(StoreJob::Begin {
                target,
                mime,
                reply,
            })
        })?;
        Ok(BlobUpload {
            client: self.clone(),
            id,
            hasher: blake3::Hasher::new(),
            len: 0,
            open: true,
        })
    }

    /// Read one range.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::InvalidHash`] or a connection error.
    pub fn read(&self, hash: &str, offset: u64, max_len: u32) -> Result<ReadOutcome, BlobError> {
        let target = self.data_target()?;
        if !valid_hash(hash) {
            return Err(BlobError::InvalidHash);
        }
        let hash = hash.to_owned();
        self.call(|reply| Job::Read {
            target,
            hash,
            offset,
            max_len,
            reply,
        })
    }

    /// Keep a copy of the public content `name` on this device for this
    /// app; the first keeper starts its fetch.
    pub fn public_keep(&self, name: &str) -> Result<(), BlobError> {
        let target = self.data_target()?;
        let name = name.to_owned();
        self.call(|reply| {
            Job::Public(PublicJob::Keep {
                target,
                name,
                reply,
            })
        })
    }

    /// This app no longer keeps the public content `name`. The copy stays
    /// on this device until the device needs its room.
    pub fn public_release(&self, name: &str) -> Result<(), BlobError> {
        let target = self.data_target()?;
        let name = name.to_owned();
        self.call(|reply| {
            Job::Public(PublicJob::Release {
                target,
                name,
                reply,
            })
        })
    }

    /// Read one range of the current copy of the public content `name`.
    pub fn public_read(
        &self,
        name: &str,
        offset: u64,
        max_len: u32,
    ) -> Result<PublicRead, BlobError> {
        let target = self.data_target()?;
        let name = name.to_owned();
        self.call(|reply| {
            Job::Public(PublicJob::Read {
                target,
                name,
                offset,
                max_len,
                reply,
            })
        })
    }

    /// The whole current copy of the public content `name` and its hash,
    /// verified against it; `Err` with the outcome when it is pending or
    /// unavailable.
    pub fn public_read_all(
        &self,
        name: &str,
    ) -> Result<Result<(Vec<u8>, String), ReadOutcome>, BlobError> {
        let first = self.public_read(name, 0, MAX_READ_CHUNK)?;
        let (ReadOutcome::Ready { bytes, total_len }, Some(hash)) =
            (first.outcome.clone(), first.hash.clone())
        else {
            return Ok(Err(first.outcome));
        };
        if total_len > MAX_BLOB_BYTES {
            return Err(BlobError::TooLarge {
                len: total_len,
                limit: MAX_BLOB_BYTES,
            });
        }
        let mut whole = bytes;
        while (whole.len() as u64) < total_len {
            let next = self.public_read(name, whole.len() as u64, MAX_READ_CHUNK)?;
            match (next.outcome, next.hash) {
                (ReadOutcome::Ready { bytes, .. }, Some(same))
                    if same == hash && !bytes.is_empty() =>
                {
                    whole.extend_from_slice(&bytes);
                }
                // A newer copy replaced it while it was read: read that one.
                (ReadOutcome::Ready { .. }, Some(_)) => return self.public_read_all(name),
                (outcome, _) => return Ok(Err(outcome)),
            }
        }
        if blake3::hash(&whole).to_hex().as_str() != hash {
            return Err(BlobError::Refused(
                "the public copy does not match its hash".into(),
            ));
        }
        Ok(Ok((whole, hash)))
    }

    /// For a session client: check every kept public copy against where it
    /// came from, taking newer ones. How many changed.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::InvalidScope`] for a client that is not a
    /// session root, or a connection error.
    pub fn refresh_public(&self) -> Result<u32, BlobError> {
        if !self.worker.session || self.target != ROOT {
            return Err(BlobError::InvalidScope);
        }
        self.call(|reply| Job::Public(PublicJob::Refresh { reply }))
    }

    /// A reader over `hash` that fetches [`MAX_READ_CHUNK`] ranges as it is
    /// read, or `None` when the content is pending or unavailable. Only the
    /// first range is read before this returns.
    ///
    /// The reader does not hash what it returns: a consumer verifies it (as
    /// [`read_all`](Self::read_all) does) or hands it to something that does.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::InvalidHash`], [`BlobError::TooLarge`] for
    /// content above [`MAX_BLOB_BYTES`], or a connection error.
    pub fn open_reader(&self, hash: &str) -> Result<Option<BlobReader>, BlobError> {
        Ok(self.start_read(hash)?.ok())
    }

    fn start_read(&self, hash: &str) -> Result<Result<BlobReader, ReadOutcome>, BlobError> {
        let outcome = self.read(hash, 0, MAX_READ_CHUNK)?;
        if !matches!(outcome, ReadOutcome::Ready { .. }) {
            return Ok(Err(outcome));
        }
        let mut reader = BlobReader {
            client: self.clone(),
            hash: hash.to_owned(),
            total_len: None,
            next_offset: 0,
            chunk: Vec::new(),
            at: 0,
        };
        reader.accept(outcome)?;
        Ok(Ok(reader))
    }

    /// Read the whole content, range by range, and verify it against `hash`.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::InvalidHash`], [`BlobError::TooLarge`], a
    /// refusal for content that fails verification, or a connection error.
    pub fn read_all(&self, hash: &str) -> Result<ReadOutcome, BlobError> {
        let mut reader = match self.start_read(hash)? {
            Ok(reader) => reader,
            Err(outcome) => return Ok(outcome),
        };
        let mut bytes = Vec::with_capacity(usize::try_from(reader.len()).unwrap_or(0));
        io::Read::read_to_end(&mut reader, &mut bytes).map_err(|error| blob_error(&error))?;
        if blake3::hash(&bytes).to_hex().as_str() != hash {
            return Err(BlobError::Refused("content does not match its hash".into()));
        }
        let total_len = bytes.len() as u64;
        Ok(ReadOutcome::Ready { bytes, total_len })
    }

    /// # Errors
    ///
    /// Returns [`BlobError::InvalidHash`] or a connection error.
    pub fn has(&self, hash: &str) -> Result<bool, BlobError> {
        let target = self.data_target()?;
        if !valid_hash(hash) {
            return Err(BlobError::InvalidHash);
        }
        let hash = hash.to_owned();
        self.call(|reply| Job::Has {
            target,
            hash,
            reply,
        })
    }

    /// # Errors
    ///
    /// Returns [`BlobError::InvalidHash`] or a connection error.
    pub fn size(&self, hash: &str) -> Result<Option<u64>, BlobError> {
        let target = self.data_target()?;
        if !valid_hash(hash) {
            return Err(BlobError::InvalidHash);
        }
        let hash = hash.to_owned();
        self.call(|reply| Job::Size {
            target,
            hash,
            reply,
        })
    }

    fn call<T>(&self, job: impl FnOnce(Answer<T>) -> Job) -> Result<T, BlobError> {
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        self.worker
            .jobs
            .send(job(reply))
            .map_err(|_| BlobError::Disconnected("document media connection closed".into()))?;
        answer
            .recv_timeout(CALL_TIMEOUT)
            .map_err(|_| BlobError::Disconnected("document media service did not answer".into()))?
    }

    fn start(stream: StdUnixStream, session: bool) -> Result<Self, BlobError> {
        let (jobs, receiver) = tokio::sync::mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("blob-client".into())
            .spawn(move || drive(stream, session, receiver))
            .map_err(|error| BlobError::Disconnected(error.to_string()))?;
        Ok(Self {
            worker: Arc::new(Worker { jobs, session }),
            target: ROOT,
        })
    }
}

/// Up to one piece of content from `reader`; shorter only at its end.
fn next_chunk(reader: &mut impl io::Read) -> Result<Vec<u8>, BlobError> {
    let mut chunk = Vec::with_capacity(UPLOAD_PIECE);
    reader
        .take(UPLOAD_PIECE as u64)
        .read_to_end(&mut chunk)
        .map_err(|error| BlobError::Input(error.to_string()))?;
    Ok(chunk)
}

/// The reference goes into a document; one naming other bytes would poison
/// every replica that later reads it.
fn checked_ref(stored: BlobRef, sent: &blake3::Hasher, len: u64) -> Result<BlobRef, BlobError> {
    if stored.hash != sent.finalize().to_hex().as_str() || stored.len != len {
        return Err(BlobError::Refused(
            "store answered a reference for other content".into(),
        ));
    }
    Ok(stored)
}

/// An upload that has begun; released on the driver unless it finished.
struct OpenUpload<'a> {
    client: &'a BlobClient,
    id: u64,
    open: bool,
}

impl Drop for OpenUpload<'_> {
    fn drop(&mut self) {
        if self.open {
            let _ = self
                .client
                .worker
                .jobs
                .send(Job::Store(StoreJob::Abort { upload: self.id }));
        }
    }
}

/// An upload [`BlobClient::begin_upload`] opened: content appended piece
/// by piece, stored by [`Self::finish`]. Dropped unfinished, or after a
/// failed append, it is released and nothing it was given is stored.
pub struct BlobUpload {
    client: BlobClient,
    id: u64,
    hasher: blake3::Hasher,
    len: u64,
    open: bool,
}

impl std::fmt::Debug for BlobUpload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobUpload")
            .field("id", &self.id)
            .field("len", &self.len)
            .field("open", &self.open)
            .finish_non_exhaustive()
    }
}

impl BlobUpload {
    /// Bytes appended so far.
    pub const fn len(&self) -> u64 {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Hand the store the next `bytes`, [`MAX_PUT_CHUNK`] at a time.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::TooLarge`] once the content would pass
    /// [`MAX_BLOB_BYTES`], the service's refusal, or a connection error.
    /// Any failure ends the upload: nothing is stored and later calls are
    /// refused.
    pub fn append(&mut self, bytes: &[u8]) -> Result<(), BlobError> {
        if !self.open {
            return Err(BlobError::Refused("upload already ended".into()));
        }
        let len = self.len.saturating_add(bytes.len() as u64);
        if len > MAX_BLOB_BYTES {
            self.abort();
            return Err(BlobError::TooLarge {
                len,
                limit: MAX_BLOB_BYTES,
            });
        }
        for piece in bytes.chunks(MAX_PUT_CHUNK) {
            let upload = self.id;
            let written = self.client.call(|reply| {
                Job::Store(StoreJob::Write {
                    upload,
                    bytes: piece.to_vec(),
                    reply,
                })
            });
            if let Err(error) = written {
                self.abort();
                return Err(error);
            }
            self.hasher.update(piece);
        }
        self.len = len;
        Ok(())
    }

    /// Store what was appended and answer its reference, checked against
    /// the bytes handed over.
    ///
    /// # Errors
    ///
    /// Returns the service's refusal, a refusal when the store answers a
    /// reference for other content or the upload already ended, or a
    /// connection error.
    pub fn finish(mut self) -> Result<BlobRef, BlobError> {
        if !self.open {
            return Err(BlobError::Refused("upload already ended".into()));
        }
        self.open = false;
        let upload = self.id;
        let stored = self
            .client
            .call(|reply| Job::Store(StoreJob::Finish { upload, reply }))?;
        checked_ref(stored, &self.hasher, self.len)
    }

    fn abort(&mut self) {
        if std::mem::take(&mut self.open) {
            let _ = self
                .client
                .worker
                .jobs
                .send(Job::Store(StoreJob::Abort { upload: self.id }));
        }
    }
}

impl Drop for BlobUpload {
    fn drop(&mut self) {
        self.abort();
    }
}

/// Content read range by range as it is consumed ([`BlobClient::open_reader`]).
///
/// It holds at most one [`MAX_READ_CHUNK`] range. A blob whose length changes
/// mid-read, a range that makes no progress or content that stops being
/// readable fails the read. The bytes are **not** verified here.
pub struct BlobReader {
    client: BlobClient,
    hash: String,
    total_len: Option<u64>,
    /// Where the range after `chunk` starts.
    next_offset: u64,
    chunk: Vec<u8>,
    /// How much of `chunk` was returned.
    at: usize,
}

impl std::fmt::Debug for BlobReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobReader")
            .field("hash", &self.hash)
            .field("total_len", &self.total_len)
            .field("next_offset", &self.next_offset)
            .finish_non_exhaustive()
    }
}

impl BlobReader {
    /// The content's whole length, as the store reported it.
    pub fn len(&self) -> u64 {
        self.total_len.unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Take one answered range as the next chunk.
    fn accept(&mut self, outcome: ReadOutcome) -> Result<(), BlobError> {
        let ReadOutcome::Ready { bytes, total_len } = outcome else {
            return Err(BlobError::Refused(
                "blob stopped being readable mid-read".into(),
            ));
        };
        if total_len > MAX_BLOB_BYTES {
            return Err(BlobError::TooLarge {
                len: total_len,
                limit: MAX_BLOB_BYTES,
            });
        }
        if *self.total_len.get_or_insert(total_len) != total_len {
            return Err(BlobError::Refused("blob length changed mid-read".into()));
        }
        let end = self.next_offset.saturating_add(bytes.len() as u64);
        if bytes.is_empty() && self.next_offset < total_len {
            return Err(BlobError::Refused("blob read made no progress".into()));
        }
        if end > total_len {
            return Err(BlobError::Refused("blob read ran past its length".into()));
        }
        self.next_offset = end;
        self.chunk = bytes;
        self.at = 0;
        Ok(())
    }
}

impl io::Read for BlobReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.at >= self.chunk.len() {
            if self.next_offset >= self.len() || buf.is_empty() {
                return Ok(0);
            }
            let outcome = self
                .client
                .read(&self.hash, self.next_offset, MAX_READ_CHUNK)
                .map_err(io_error)?;
            self.accept(outcome).map_err(io_error)?;
        }
        let rest = self.chunk.get(self.at..).unwrap_or_default();
        let n = rest.len().min(buf.len());
        buf.get_mut(..n)
            .unwrap_or_default()
            .copy_from_slice(rest.get(..n).unwrap_or_default());
        self.at = self.at.saturating_add(n);
        Ok(n)
    }
}

fn io_error(error: BlobError) -> io::Error {
    let kind = match error {
        BlobError::Refused(_) | BlobError::TooLarge { .. } => io::ErrorKind::InvalidData,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, error)
}

fn blob_error(error: &io::Error) -> BlobError {
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<BlobError>())
        .cloned()
        .unwrap_or_else(|| BlobError::Disconnected(error.to_string()))
}

/// What the driver thread holds for one connection.
struct Session {
    root: Arc<Object>,
    scopes: RefCell<HashMap<u64, Arc<Object>>>,
    uploads: RefCell<HashMap<u64, Arc<Object>>>,
    next_id: Cell<u64>,
}

impl Session {
    fn issue_id(&self) -> u64 {
        let id = self.next_id.get();
        self.next_id.set(id.saturating_add(1));
        id
    }

    fn child(&self, handle: Option<u64>) -> Result<Arc<Object>, BlobError> {
        crate::handle_reply(handle, &self.root).map_err(BlobError::from)
    }

    fn scopes(&self, target: u64) -> Result<Arc<Object>, BlobError> {
        self.scopes
            .borrow()
            .get(&target)
            .cloned()
            .ok_or(BlobError::InvalidScope)
    }

    fn upload(&self, id: u64) -> Result<Arc<Object>, BlobError> {
        self.uploads
            .borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| BlobError::Refused("no such upload".into()))
    }

    fn issue_id_for(&self, object: Arc<Object>) -> u64 {
        let id = self.issue_id();
        self.scopes.borrow_mut().insert(id, object);
        id
    }
}

fn drive(
    stream: StdUnixStream,
    session: bool,
    mut jobs: tokio::sync::mpsc::UnboundedReceiver<Job>,
) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        // Dropping the receiver fails every pending and later call.
        return;
    };
    let Ok(transport) = Transport::connect("blob-client", stream) else {
        return;
    };
    let root = Object::root(transport);
    let mut scopes = HashMap::new();
    if !session {
        scopes.insert(ROOT, Arc::clone(&root));
    }
    let session = std::rc::Rc::new(Session {
        root,
        scopes: RefCell::new(scopes),
        uploads: RefCell::new(HashMap::new()),
        next_id: Cell::new(ROOT.saturating_add(1)),
    });
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async move {
        while let Some(job) = jobs.recv().await {
            let session = std::rc::Rc::clone(&session);
            tokio::task::spawn_local(async move {
                run_job(job, &session).await;
            });
        }
    });
}

#[allow(
    clippy::future_not_send,
    reason = "the client session is Rc-based and runs on a LocalSet by construction"
)]
async fn run_job(job: Job, session: &Session) {
    match job {
        Job::Store(job) => store(job, session).await,
        Job::Read {
            target,
            hash,
            offset,
            max_len,
            reply,
        } => {
            let result = async {
                let client = session.scopes(target)?;
                match client
                    .call(FsRequest::Get {
                        hash,
                        offset,
                        max_len,
                    })
                    .await?
                {
                    FsReply::BlobRead(outcome) => Ok(outcome),
                    _ => Err(BlobError::Refused("unexpected media reply".into())),
                }
            }
            .await;
            let _ = reply.send(result);
        }
        Job::Has {
            target,
            hash,
            reply,
        } => {
            let result = async {
                let client = session.scopes(target)?;
                match client.call(FsRequest::Has { hash }).await? {
                    FsReply::Present(present) => Ok(present),
                    _ => Err(BlobError::Refused("unexpected media reply".into())),
                }
            }
            .await;
            let _ = reply.send(result);
        }
        Job::Size {
            target,
            hash,
            reply,
        } => {
            let result = async {
                let client = session.scopes(target)?;
                match client.call(FsRequest::Size { hash }).await? {
                    FsReply::Size(len) => Ok(len),
                    _ => Err(BlobError::Refused("unexpected media reply".into())),
                }
            }
            .await;
            let _ = reply.send(result);
        }
        Job::Scope { scope, reply } => {
            let result = async {
                match session
                    .root
                    .call(FsRequest::SessionScope {
                        workspace: scope.workspace().to_owned(),
                        app: scope.app().to_owned(),
                    })
                    .await?
                {
                    FsReply::Scope(id) => Ok(session.issue_id_for(
                        crate::handle_reply(Some(id), &session.root).map_err(BlobError::from)?,
                    )),
                    _ => Err(BlobError::Refused("unexpected media reply".into())),
                }
            }
            .await;
            let _ = reply.send(result);
        }
        Job::Attribute { attribution, reply } => {
            let _ = reply.send(attribute(attribution, session).await);
        }
        Job::Public(job) => public(job, session).await,
    }
}

/// Name the agent behind this device's next writes, as the session.
#[allow(
    clippy::future_not_send,
    reason = "the client session is Rc-based and runs on a LocalSet by construction"
)]
async fn attribute(attribution: WriteAttribution, session: &Session) -> Result<(), BlobError> {
    let color = attribution.color;
    match session
        .root
        .call(FsRequest::SessionAttributeWrites {
            paths: attribution.paths,
            actor: attribution.actor,
            label: attribution.label,
            color,
            ttl_ms: u32::try_from(attribution.ttl.as_millis()).unwrap_or(u32::MAX),
        })
        .await?
    {
        FsReply::Empty => Ok(()),
        _ => Err(BlobError::Refused("unexpected media reply".into())),
    }
}

/// The public copy `name` as the capability `target` reaches it.
#[allow(
    clippy::future_not_send,
    reason = "the client session is Rc-based and runs on a LocalSet by construction"
)]
async fn public_copy(session: &Session, target: u64, name: &str) -> Result<Arc<Object>, BlobError> {
    let client = session.scopes(target)?;
    match client
        .call(FsRequest::PublicContent {
            name: name.to_owned(),
        })
        .await?
    {
        FsReply::Handle(handle) => session.child(handle),
        _ => Err(BlobError::Refused("unexpected media reply".into())),
    }
}

#[allow(
    clippy::future_not_send,
    reason = "the client session is Rc-based and runs on a LocalSet by construction"
)]
async fn public(job: PublicJob, session: &Session) {
    match job {
        PublicJob::Keep {
            target,
            name,
            reply,
        } => {
            let result = async {
                let copy = public_copy(session, target, &name).await?;
                match copy.call(FsRequest::PublicKeep).await? {
                    FsReply::Empty => Ok(()),
                    _ => Err(BlobError::Refused("unexpected media reply".into())),
                }
            }
            .await;
            let _ = reply.send(result);
        }
        PublicJob::Release {
            target,
            name,
            reply,
        } => {
            let result = async {
                let copy = public_copy(session, target, &name).await?;
                match copy.call(FsRequest::PublicRelease).await? {
                    FsReply::Empty => Ok(()),
                    _ => Err(BlobError::Refused("unexpected media reply".into())),
                }
            }
            .await;
            let _ = reply.send(result);
        }
        PublicJob::Read {
            target,
            name,
            offset,
            max_len,
            reply,
        } => {
            let result = async {
                let copy = public_copy(session, target, &name).await?;
                match copy.call(FsRequest::PublicRead { offset, max_len }).await? {
                    FsReply::PublicRead { read, hash } => Ok(PublicRead {
                        outcome: read,
                        hash: valid_hash(&hash).then_some(hash),
                    }),
                    _ => Err(BlobError::Refused("unexpected media reply".into())),
                }
            }
            .await;
            let _ = reply.send(result);
        }
        PublicJob::Refresh { reply } => {
            let result = async {
                match session.root.call(FsRequest::SessionRefreshPublic).await? {
                    FsReply::Refreshed(refreshed) => Ok(refreshed),
                    _ => Err(BlobError::Refused("unexpected media reply".into())),
                }
            }
            .await;
            let _ = reply.send(result);
        }
    }
}

#[allow(
    clippy::future_not_send,
    reason = "the client session is Rc-based and runs on a LocalSet by construction"
)]
async fn store(job: StoreJob, session: &Session) {
    match job {
        StoreJob::Put {
            target,
            bytes,
            mime,
            reply,
        } => {
            let result = async {
                let client = session.scopes(target)?;
                match client.call(FsRequest::Put { bytes, mime }).await? {
                    FsReply::Blob(reference) => Ok(reference),
                    _ => Err(BlobError::Refused("unexpected media reply".into())),
                }
            }
            .await;
            let _ = reply.send(result);
        }
        StoreJob::Begin {
            target,
            mime,
            reply,
        } => {
            let result = async {
                let client = session.scopes(target)?;
                match client.call(FsRequest::Upload { mime }).await? {
                    FsReply::Handle(handle) => {
                        let upload = session.child(handle)?;
                        let id = session.issue_id();
                        session.uploads.borrow_mut().insert(id, upload);
                        Ok(id)
                    }
                    _ => Err(BlobError::Refused("unexpected media reply".into())),
                }
            }
            .await;
            let _ = reply.send(result);
        }
        StoreJob::Write {
            upload,
            bytes,
            reply,
        } => {
            let result = async {
                let upload = session.upload(upload)?;
                match upload.call(FsRequest::UploadWrite { bytes }).await? {
                    FsReply::Empty => Ok(()),
                    _ => Err(BlobError::Refused("unexpected media reply".into())),
                }
            }
            .await;
            let _ = reply.send(result);
        }
        StoreJob::Finish { upload, reply } => {
            let result = async {
                let client = session.upload(upload)?;
                session.uploads.borrow_mut().remove(&upload);
                match client.call(FsRequest::UploadFinish).await? {
                    FsReply::Blob(reference) => Ok(reference),
                    _ => Err(BlobError::Refused("unexpected media reply".into())),
                }
            }
            .await;
            let _ = reply.send(result);
        }
        StoreJob::Abort { upload } => {
            session.uploads.borrow_mut().remove(&upload);
        }
    }
}
