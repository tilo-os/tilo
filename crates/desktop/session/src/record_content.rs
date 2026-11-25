//! The records' content as this session reaches it.
//!
//! The bytes a record names by address are kept on the device's one shelf, which the device's
//! daemon opens and serves on its document-media socket beside the files' bytes. A session
//! reads them back by the address its record carries, and keeps what it makes itself: the
//! picture of a window it does not run, the parts of a record it keeps, and what a drag carries
//! to another of the person's devices. Each is a scope the daemon serves, a workspace and an
//! app.
//!
//! Possessing an address never grants access to it: the daemon answers a scope only while that
//! workspace references the content, and a record's readers alone hold the references its
//! records name. What a session asks for that this device does not keep is asked of the
//! workspace's other devices by the daemon's record content, once the read here missed it; what
//! one of them answers is kept where this session's next read finds it.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_fs_client::blobs::{BlobClient, BlobError, ReadOutcome};
use tilo_lock::MutexRecover as _;

/// The app whose scope the content this session keeps for a workspace lives under: the one the
/// content put API writes into, so every caller that names a workspace's content by address
/// reads what the session kept there.
const PUT_SCOPE: &str = tilo_broker_core::services::CONTENT_PUT_SCOPE;
/// The scopes one connection keeps open at once; past it, they are opened again as they are used.
const OPEN_SCOPES: usize = 64;
/// How long one read waits before the device it asked hands content over, when a caller waits.
const POLL: Duration = Duration::from_millis(25);

/// The device's record content, as this session reaches it: one connection to the daemon's
/// document media, and the scope of each workspace and app opened on it.
pub(crate) struct RecordContent {
    socket: PathBuf,
    connection: Mutex<Option<Connection>>,
}

/// One connection to the daemon's document media, and the scopes opened on it.
struct Connection {
    session: BlobClient,
    scopes: HashMap<(String, String), BlobClient>,
}

/// This session's content, reached at `socket` — the daemon's document media — or, when it
/// names none, at the one the launcher named the daemon's.
pub(crate) fn open(socket: Option<PathBuf>) -> Arc<RecordContent> {
    Arc::new(RecordContent {
        socket: socket.unwrap_or_else(launcher::services::blob_socket),
        connection: Mutex::default(),
    })
}

impl RecordContent {
    /// `call` on the client of `workspace` and `app`. A connection the daemon closed is made
    /// again, once.
    fn on<T>(
        &self,
        workspace: &str,
        app: &str,
        call: impl Fn(&BlobClient) -> Result<T, BlobError>,
    ) -> Result<T, String> {
        let answered = call(&self.scoped(workspace, app)?);
        if matches!(&answered, Err(BlobError::Disconnected(_))) {
            self.connection.lock_recover().take();
            return call(&self.scoped(workspace, app)?).map_err(|error| error.to_string());
        }
        answered.map_err(|error| error.to_string())
    }

    /// The client of `workspace` and `app`, opened on the connection to the daemon when it is
    /// not open yet. A workspace or app name no store takes is refused here.
    fn scoped(&self, workspace: &str, app: &str) -> Result<BlobClient, String> {
        let mut guard = self.connection.lock_recover();
        let connection = match &mut *guard {
            Some(connection) => connection,
            None => guard.insert(Connection {
                session: BlobClient::connect_session(&self.socket)
                    .map_err(|error| error.to_string())?,
                scopes: HashMap::new(),
            }),
        };
        if let Some(scoped) = connection
            .scopes
            .get(&(workspace.to_owned(), app.to_owned()))
        {
            return Ok(scoped.clone());
        }
        let scoped = connection
            .session
            .scope(workspace, app)
            .map_err(|error| error.to_string())?;
        if connection.scopes.len() >= OPEN_SCOPES {
            connection.scopes.clear();
        }
        connection
            .scopes
            .insert((workspace.to_owned(), app.to_owned()), scoped.clone());
        drop(guard);
        Ok(scoped)
    }

    /// The bytes this device keeps of `app` 's content in `workspace` under `hash` (lower-case
    /// hex): never a fetch of what it lacks, and what it lacks is asked of the workspace's other
    /// devices, to read again once it is here.
    pub(crate) fn read(&self, workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
        let outcome = self
            .on(workspace, app, |scoped| scoped.read_all(hash))
            .ok()?;
        match outcome {
            ReadOutcome::Ready { bytes, .. } => Some(bytes),
            ReadOutcome::Pending | ReadOutcome::Unavailable => {
                // The daemon asks the workspace's devices for it, and keeps what one answers,
                // where the caller's next read finds it.
                let _ = self.want(workspace, app, hash);
                None
            }
        }
    }

    /// What an app put for `workspace` under `hash`, by the address its record names: the scope
    /// the content put API writes into, where a chosen file's bytes live.
    pub(crate) fn read_put(&self, workspace: &str, hash: &str) -> Option<Vec<u8>> {
        self.read(workspace, PUT_SCOPE, hash)
    }

    /// Whether this device keeps `hash` of `app` 's content in `workspace` now.
    pub(crate) fn has(&self, workspace: &str, app: &str, hash: &str) -> bool {
        self.on(workspace, app, |scoped| scoped.has(hash))
            .unwrap_or(false)
    }

    /// Ask the daemon for `hash` of `app` 's content in `workspace`, which this device does not
    /// keep: it asks the workspace's devices and keeps what one answers. Asked again on every
    /// call, and the daemon decides whether it goes out again.
    fn want(&self, workspace: &str, app: &str, hash: &str) -> Result<(), String> {
        self.on(workspace, app, |scoped| scoped.want(hash))
    }

    /// `digest` of `app` 's content in `workspace` , once it is here: what this device keeps
    /// already, else what the workspace's other devices hand over within `within` . `None` when
    /// it does not arrive in time.
    pub(crate) async fn wait(
        &self,
        workspace: &str,
        app: &str,
        digest: [u8; 32],
        within: Duration,
    ) -> Option<Vec<u8>> {
        let hash = hex::encode(digest);
        let started = Instant::now();
        loop {
            if let Some(bytes) = self.read(workspace, app, &hash) {
                return Some(bytes);
            }
            if started.elapsed() >= within {
                return None;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Keep the file at `path` under `app`'s scope in `workspace`, streamed: a
    /// file the person hands to one of their other devices, which is not held
    /// whole in memory and is not bounded by the media capability's size.
    /// Answers the address the bytes are kept under.
    ///
    /// # Errors
    /// The file cannot be read, the workspace and app name no store, or the
    /// store refused the bytes.
    pub(crate) fn keep_file_under(
        &self,
        workspace: &str,
        app: &str,
        path: &std::path::Path,
    ) -> Result<String, String> {
        let owned = path.to_owned();
        self.on(workspace, app, |scoped| {
            let mut file =
                std::fs::File::open(&owned).map_err(|error| BlobError::Input(error.to_string()))?;
            scoped.put_file(&mut file)
        })
        .map(|kept| kept.hash)
    }

    pub(crate) fn keep(&self, workspace: &str, bytes: &[u8]) -> Result<String, String> {
        self.keep_in(workspace, PUT_SCOPE, bytes)
    }

    /// Whether `hash` is kept of `workspace` 's put content, as `keep` keeps it.
    #[cfg(test)]
    pub(crate) fn holds(&self, workspace: &str, hash: &str) -> bool {
        self.has(workspace, PUT_SCOPE, hash)
    }

    /// Keep `bytes` as content of `app` 's record in `workspace`. Answers the address the bytes
    /// are kept under.
    ///
    /// # Errors
    /// The workspace and app name no store, or the store refused the bytes.
    fn keep_in(&self, workspace: &str, app: &str, bytes: &[u8]) -> Result<String, String> {
        let stored = self
            .on(workspace, app, |scoped| {
                scoped.put(bytes, "application/octet-stream")
            })
            .map_err(|error| error.to_string())?;
        let hash = tilo_content::hash_bytes(bytes);
        if stored.hash == hash {
            Ok(hash)
        } else {
            Err("the content store keeps these bytes under another address".into())
        }
    }

    /// The content store of the record of `app` in `workspace` , as the session keeps it: each
    /// part sealed under a key of its own, which the record that names it carries, so the
    /// record's readers alone open it. The session holds no workspace's keys; the daemon does.
    /// What the store keeps is offered where the workspace's devices fetch the record's content from, since it is kept on
    /// the one shelf they read through.
    ///
    /// The store is the same one for a workspace and app every time: a session lends it to the
    /// services whose records it keeps (`tilo_service::Service::lend_content`) on every pass.
    pub(crate) fn store(
        self: &Arc<Self>,
        workspace: &str,
        app: &str,
    ) -> tilo_content::ContentStore {
        tilo_content::ContentStore::from_backend(Arc::new(Scope {
            content: Arc::clone(self),
            workspace: workspace.to_owned(),
            app: app.to_owned(),
        })
            as Arc<dyn tilo_content::ContentBackend>)
        .sealing()
    }
}

/// The store of one record's content, as the session keeps it: the daemon's shelf, under the
/// record's scope.
struct Scope {
    content: Arc<RecordContent>,
    workspace: String,
    app: String,
}

/// A part as `reader` yields it, refused past what one record's part may hold.
fn read_part(reader: &mut dyn std::io::Read) -> Result<Vec<u8>, tilo_content::ContentError> {
    let limit = neighbour_wire::content::MAX_CONTENT_BYTES;
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(tilo_content::ContentError::ItemTooLarge {
            actual: bytes.len(),
            limit: usize::try_from(limit).unwrap_or(usize::MAX),
        });
    }
    Ok(bytes)
}

impl tilo_content::ContentBackend for Scope {
    fn put(
        &self,
        reader: &mut dyn std::io::Read,
        _mime: &str,
    ) -> Result<tilo_content::Stored, tilo_content::ContentError> {
        let bytes = read_part(reader)?;
        let hash = self
            .content
            .keep_in(&self.workspace, &self.app, &bytes)
            .map_err(tilo_content::ContentError::Io)?;
        Ok(tilo_content::Stored {
            hash,
            len: bytes.len() as u64,
        })
    }

    /// The daemon seals what the session keeps: it holds the members' secret, so the same bytes
    /// kept twice are one item under one reference.
    fn put_sealed(
        &self,
        reader: &mut dyn std::io::Read,
        mime: &str,
    ) -> Option<Result<tilo_content::Stored, tilo_content::ContentError>> {
        let bytes = match read_part(reader) {
            Ok(bytes) => bytes,
            Err(error) => return Some(Err(error)),
        };
        let sealed = self.content.on(&self.workspace, &self.app, |scoped| {
            scoped.put_sealed(&mut bytes.as_slice(), mime)
        });
        Some(
            sealed
                .map(|hash| tilo_content::Stored {
                    hash,
                    len: bytes.len() as u64,
                })
                .map_err(tilo_content::ContentError::Io),
        )
    }

    fn open(
        &self,
        hash: &str,
    ) -> Result<Option<tilo_content::ContentReader>, tilo_content::ContentError> {
        // Not here: the daemon is asked for it, and what arrives is kept where the next read
        // finds it.
        Ok(self
            .content
            .read(&self.workspace, &self.app, hash)
            .map(|bytes| Box::new(std::io::Cursor::new(bytes)) as tilo_content::ContentReader))
    }

    fn has(&self, hash: &str) -> bool {
        self.content.has(&self.workspace, &self.app, hash)
    }

    fn state(&self, hash: &str) -> tilo_content::ContentState {
        if self.has(hash) {
            tilo_content::ContentState::Resolved
        } else {
            // A want started by the miss: a later read answers it.
            let _ = self.content.want(&self.workspace, &self.app, hash);
            tilo_content::ContentState::Pending
        }
    }

    fn shared(&self) -> bool {
        true
    }

    /// The shelf serves the workspace's records, so what this store reads is as scoped as the
    /// workspace the record is in.
    fn workspace_scoped(&self) -> bool {
        true
    }

    fn max_item_bytes(&self) -> usize {
        usize::try_from(neighbour_wire::content::MAX_CONTENT_BYTES).unwrap_or(usize::MAX)
    }
}

impl tilo_runtime::offload::OutputContent for RecordContent {
    fn content<'a>(
        &'a self,
        workspace: &'a str,
        app: &'a str,
        digest: [u8; 32],
        within: Duration,
    ) -> tilo_runtime::offload::MediaFuture<'a, Option<Vec<u8>>> {
        Box::pin(self.wait(workspace, app, digest, within))
    }
}

/// This device's content as an offload route keeps it: the invoking app's scope in its workspace,
/// through the same shelf the app's own `offerBlob` and `wantBlob` reach.
impl tilo_runtime::offload::WorkspaceMedia for RecordContent {
    fn keep<'a>(
        &'a self,
        scope: &'a tilo_runtime::offload::BlobScope,
        bytes: Vec<u8>,
    ) -> tilo_runtime::offload::MediaFuture<'a, Result<[u8; 32], String>> {
        Box::pin(async move {
            let hash = self.keep_in(scope.workspace(), scope.app(), &bytes)?;
            let mut digest = [0; 32];
            hex::decode_to_slice(&hash, &mut digest)
                .map_err(|_| "the content store keeps these bytes under no address".to_owned())?;
            Ok(digest)
        })
    }
}

#[cfg(test)]
#[path = "record_content/tests.rs"]
mod tests;
