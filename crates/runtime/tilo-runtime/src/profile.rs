//! What every host of a runtime profile does the same way: hold the profile
//! exclusively and serve its apps their document media.

use crate::unix;
use std::{io, path::Path, sync::Arc};

/// Hold `root` exclusively for this process, recording the owner so a
/// second start can name it.
/// # Errors
/// Rejects a profile another process holds, or a public or foreign profile.
pub fn lock(root: &Path) -> io::Result<advisory_lock::FileLock> {
    unix::private_directory(root)?;
    let lock_path = root.join("runtime.lock");
    let lock = advisory_lock::FileLock::try_exclusive(unix::private_file(&lock_path)?).map_err(
        |error| match error {
            advisory_lock::TryLockError::WouldBlock => {
                let owner = advisory_lock::owner_pid(&lock_path)
                    .map(|pid| format!(" by process {pid}"))
                    .unwrap_or_default();
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!("runtime profile {} is in use{owner}", root.display()),
                )
            }
            advisory_lock::TryLockError::Error(error) => error,
        },
    )?;
    lock.record_owner()?;
    Ok(lock)
}

/// Serves the host's apps their document media: the filesystem daemon's
/// content store and capability, on the host's private profile.
pub struct MediaHost {
    store: Arc<tilo_fs::ContentStore>,
    _server: std::thread::JoinHandle<()>,
    runtime: Option<tokio::runtime::Runtime>,
}

impl MediaHost {
    /// Open the content store at `content` and serve it on `socket`.
    /// # Errors
    /// Returns runtime, store or socket errors.
    pub fn start(content: &Path, socket: &Path) -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("tilo-media")
            .enable_all()
            .build()?;
        let store = Arc::new(
            runtime
                .block_on(tilo_fs::ContentStore::open(content))
                .map_err(io::Error::other)?,
        );
        let media = tilo_fs::document_media::DocumentMedia::new(Arc::clone(&store));
        let server = {
            let _entered = runtime.enter();
            tilo_fs::document_media::serve_socket(socket, media).map_err(io::Error::other)?
        };
        Ok(Self {
            store,
            _server: server,
            runtime: Some(runtime),
        })
    }

    pub(crate) fn handle(&self) -> io::Result<tokio::runtime::Handle> {
        self.runtime
            .as_ref()
            .map(|runtime| runtime.handle().clone())
            .ok_or_else(|| io::Error::other("document media runtime is unavailable"))
    }

    /// The content store, which the profile's workspace files share.
    pub fn store(&self) -> Arc<tilo_fs::ContentStore> {
        Arc::clone(&self.store)
    }
}

impl Drop for MediaHost {
    /// Release the store's database so the profile can be served again in
    /// this process; the server thread then only answers with errors.
    fn drop(&mut self) {
        let Some(runtime) = self.runtime.take() else {
            return;
        };
        let store = Arc::clone(&self.store);
        let _ = std::thread::spawn(move || {
            let _ = runtime.block_on(store.shutdown());
            runtime.shutdown_background();
        })
        .join();
    }
}

/// The content service of one room, read as each app reads it.
pub struct BlobMedia {
    blob_socket: std::path::PathBuf,
    room: String,
}

impl BlobMedia {
    #[must_use]
    pub fn new(blob_socket: &Path, room: &str) -> Self {
        Self {
            blob_socket: blob_socket.to_path_buf(),
            room: room.to_owned(),
        }
    }
}

impl crate::MediaReader for BlobMedia {
    fn read(&self, app_id: &str, hash: &str) -> io::Result<Option<Vec<u8>>> {
        read_media(&self.blob_socket, &self.room, app_id, hash)
    }
}

/// Read document media an app in `room` stored, through the same scoped
/// capability the app holds.
/// # Errors
/// Returns connection errors; `Ok(None)` when the content is not readable.
pub fn read_media(
    blob_socket: &Path,
    room: &str,
    app_id: &str,
    hash: &str,
) -> io::Result<Option<Vec<u8>>> {
    use runtime_fs::blobs::{BlobClient, ReadOutcome};
    let session = BlobClient::connect_session(blob_socket).map_err(io::Error::other)?;
    let scoped = session.scope(room, app_id).map_err(io::Error::other)?;
    match scoped.read_all(hash).map_err(io::Error::other)? {
        ReadOutcome::Ready { bytes, .. } => Ok(Some(bytes)),
        ReadOutcome::Pending | ReadOutcome::Unavailable => Ok(None),
    }
}
