//! Files this device keeps for itself, written away from the frame that
//! changed them.
//!
//! What a session keeps beside its desk is written while the person is
//! working — leaving a workspace, moving a window — which is inside a frame.
//! An atomic replacement syncs the file and its directory, and on a busy disk
//! that has held a frame for seconds. So the value goes to memory, and a
//! worker writes the newest of a burst once the burst is over; a read never
//! waits for it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

/// How long a save waits for the last of a burst before it is written.
const QUIET: std::time::Duration = std::time::Duration::from_millis(250);

/// One file, kept up to date by a worker.
pub(crate) struct Kept {
    path: PathBuf,
    /// The newest bytes to write, or nothing when the file is up to date.
    pending: Arc<Mutex<Option<Vec<u8>>>>,
    /// Raised when there is something to write.
    asked: std::sync::mpsc::Sender<()>,
}

impl Kept {
    /// Keep `path` up to date, once its burst of saves is over.
    pub(crate) fn new(path: PathBuf) -> Self {
        let pending: Arc<Mutex<Option<Vec<u8>>>> = Arc::default();
        let (asked, asks) = std::sync::mpsc::channel::<()>();
        let written = Arc::clone(&pending);
        let at = path.clone();
        let spawned = std::thread::Builder::new()
            .name("tilo-kept-file".to_owned())
            .spawn(move || {
                while asks.recv().is_ok() {
                    // A burst of saves is one write of the newest.
                    while asks.recv_timeout(QUIET).is_ok() {}
                    let bytes = written.lock_recover().take();
                    let Some(bytes) = bytes else {
                        continue;
                    };
                    if let Some(dir) = at.parent()
                        && let Err(error) = std::fs::create_dir_all(dir)
                    {
                        tracing::debug!(%error, path = %at.display(), "a kept file is not written");
                        continue;
                    }
                    if let Err(error) =
                        tilo_files::native::atomic::write_with_mode(&at, &bytes, 0o600)
                    {
                        tracing::debug!(%error, path = %at.display(), "a kept file is not written");
                    }
                }
            });
        if let Err(error) = spawned {
            tracing::debug!(%error, path = %path.display(), "a kept file is not followed");
        }
        Self {
            path,
            pending,
            asked,
        }
    }

    /// What the file held when the session started.
    pub(crate) fn read(&self) -> Option<Vec<u8>> {
        std::fs::read(&self.path).ok()
    }

    /// Keep `bytes` as the file, once the burst of saves is over.
    pub(crate) fn save(&self, bytes: Vec<u8>) {
        *self.pending.lock_recover() = Some(bytes);
        let _ = self.asked.send(());
    }
}
