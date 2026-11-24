//! The record content a workspace's documents reference, on the workspace
//! service's links: the profile's content store, and the workspaces the
//! service holds.
//!
//! The routing itself is the workspace service's ([`tilo_workspace_service::content`]);
//! what a profile adds is where its content is kept — the profile's own
//! content store, reached through the scope a record's content lives under —
//! and the retry that asks again for content no device handed over.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime_fs::blobs::{BlobClient, BlobError, ReadOutcome};
use tilo_lock::MutexRecover as _;

use neighbour_wire::content::PIECE_PATIENCE_MS;
use tilo_workspace_service::content::{ContentScope, ContentShelf, RouteContent, routes_content};
use tilo_workspace_service::files::socket::Daemons;

/// The app a workspace's files are the records of.
const FILES_APP: &str = fs_protocol::tree::FILES_APP;

use super::workspaces::ServiceWorkspaces;

/// The profile's content store, read and written as the desktop session
/// reaches its own: through the scope a record's content lives under.
///
/// It holds one connection to the store and the scoped client of each record scope on it. A
/// client is a connection with a thread of its own: made afresh for every read and keep, each
/// cost a connect, a thread and three round trips, and a workspace whose files land asks this
/// shelf thousands of times in a minute, a member's asks for what it holds among them.
struct ProfileShelf {
    socket: std::path::PathBuf,
    connection: Mutex<Option<Connection>>,
    /// The files daemons mounting this device's workspaces: the store a mount's file bytes are
    /// kept in, asked before the profile's own.
    files: Arc<Daemons>,
}

/// One connection to the profile's content store, and the scopes opened on it.
struct Connection {
    session: BlobClient,
    scopes: HashMap<ContentScope, BlobClient>,
}

/// Scopes one connection keeps open at once; past it, they are opened again as they are used.
const OPEN_SCOPES: usize = 64;

impl ProfileShelf {
    fn new(socket: &Path, files: Arc<Daemons>) -> Self {
        Self {
            socket: socket.to_path_buf(),
            connection: Mutex::new(None),
            files,
        }
    }

    /// `call` on `scope`'s client. A connection the store closed is made again, once.
    fn on<T>(
        &self,
        scope: &ContentScope,
        call: impl Fn(&BlobClient) -> Result<T, BlobError>,
    ) -> Result<T, BlobError> {
        match call(&self.scoped(scope)?) {
            Err(BlobError::Disconnected(_)) => {
                self.connection.lock_recover().take();
                call(&self.scoped(scope)?)
            }
            answered => answered,
        }
    }

    fn scoped(&self, scope: &ContentScope) -> Result<BlobClient, BlobError> {
        let mut guard = self.connection.lock_recover();
        let connection = match &mut *guard {
            Some(connection) => connection,
            None => guard.insert(Connection {
                session: BlobClient::connect_session(&self.socket)?,
                scopes: HashMap::new(),
            }),
        };
        if let Some(scoped) = connection.scopes.get(scope) {
            return Ok(scoped.clone());
        }
        let scoped = connection.session.scope(&scope.workspace, &scope.app)?;
        if connection.scopes.len() >= OPEN_SCOPES {
            connection.scopes.clear();
        }
        connection.scopes.insert(scope.clone(), scoped.clone());
        drop(guard);
        Ok(scoped)
    }
}

impl ContentShelf for ProfileShelf {
    fn read(&self, scope: &ContentScope, hash: &str) -> Option<Vec<u8>> {
        if scope.app == FILES_APP
            && let Some(bytes) = self.files.read(&scope.workspace, hash)
        {
            return Some(bytes);
        }
        let outcome = self
            .on(scope, |scoped| {
                if !scoped.has(hash)? {
                    return Ok(None);
                }
                scoped.read_all(hash).map(Some)
            })
            .ok()??;
        match outcome {
            ReadOutcome::Ready { bytes, .. } => Some(bytes),
            ReadOutcome::Pending | ReadOutcome::Unavailable => None,
        }
    }

    fn keep(&self, scope: &ContentScope, hash: &str, bytes: &[u8]) -> Result<(), String> {
        if scope.app == FILES_APP && self.files.serves(&scope.workspace) {
            return self.files.keep(&scope.workspace, hash, bytes);
        }
        let kept = self
            .on(scope, |scoped| {
                scoped.put(bytes, "application/octet-stream")
            })
            .map_err(|error| error.to_string())?;
        if kept.hash == hash {
            Ok(())
        } else {
            Err("the content store keeps these bytes under another address".into())
        }
    }

    fn has(&self, scope: &ContentScope, hash: &str) -> bool {
        self.on(scope, |scoped| scoped.has(hash)).unwrap_or(false)
    }

    fn size(&self, scope: &ContentScope, hash: &str) -> Option<u64> {
        if let Some(size) = self.on(scope, |scoped| scoped.size(hash)).ok().flatten() {
            return Some(size);
        }
        // A mount of the workspace may keep it where the profile's store does not:
        // content put there before this device's store was reached.
        if scope.app == FILES_APP
            && let Some(bytes) = self.files.read(&scope.workspace, hash)
        {
            return Some(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        }
        None
    }

    fn read_range(
        &self,
        scope: &ContentScope,
        hash: &str,
        offset: u64,
        len: usize,
    ) -> Option<Vec<u8>> {
        let max_len = u32::try_from(len).unwrap_or(u32::MAX);
        match self.on(scope, |scoped| scoped.read(hash, offset, max_len)) {
            Ok(ReadOutcome::Ready { bytes, .. }) => return Some(bytes),
            Ok(ReadOutcome::Pending | ReadOutcome::Unavailable) | Err(_) => {}
        }
        if scope.app == FILES_APP {
            let bytes = self.files.read(&scope.workspace, hash)?;
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(bytes.len());
            let end = start.saturating_add(len).min(bytes.len());
            return bytes.get(start..end).map(<[u8]>::to_vec);
        }
        None
    }

    fn keep_file(
        &self,
        scope: &ContentScope,
        hash: &str,
        path: &std::path::Path,
        size: u64,
    ) -> Result<(), String> {
        let owned = path.to_owned();
        let kept = self
            .on(scope, |scoped| {
                let mut file = std::fs::File::open(&owned)
                    .map_err(|error| BlobError::Input(error.to_string()))?;
                scoped.put_file(&mut file)
            })
            .map_err(|error| error.to_string())?;
        if kept.hash == hash && kept.len == size {
            Ok(())
        } else {
            Err("the store kept other content than the file's address names".to_owned())
        }
    }
}

/// The profile's record-content service: its content store, served on
/// `blob_socket`, the stores of the files daemons in `files`, and the workspaces the workspace
/// service holds.
pub(super) fn document_content(
    workspaces: &Arc<ServiceWorkspaces>,
    blob_socket: &Path,
    files: Arc<Daemons>,
) -> Arc<RouteContent> {
    let shelf: Arc<dyn ContentShelf> = Arc::new(ProfileShelf::new(blob_socket, files));
    record_content_on(workspaces, shelf)
}

/// The record-content service of `workspaces`' workspaces over `shelf`.
pub(super) fn record_content_on(
    workspaces: &Arc<ServiceWorkspaces>,
    shelf: Arc<dyn ContentShelf>,
) -> Arc<RouteContent> {
    let served = routes_content(
        workspaces.copies(),
        Arc::clone(&shelf),
        workspaces.device_cert().device_public_key,
    );
    workspaces.install_content(&served.content, shelf);
    workspaces.install_keeper(Arc::clone(&served.keeper));
    // A want that got no content is asked again once the patience of its
    // last answer passed: a holder that said it keeps none may keep it a
    // moment later, and a large content crosses in windows of one answer.
    let ticking = Arc::clone(&served.content);
    tilo_rt::app().spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(PIECE_PATIENCE_MS)).await;
            ticking.retry(tilo_clock::unix_now_ms());
        }
    });
    served.content
}
