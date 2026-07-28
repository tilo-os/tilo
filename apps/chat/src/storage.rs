//! Disk persistence for the chat workspace.
//!
//! The entire convergent document — workspaces, categories, channels,
//! messages, reactions, edit history, members, and voice membership — is
//! saved as the raw Automerge bytes to a single file:
//!
//! ```text
//! $XDG_DATA_HOME/qos-chat/workspace.automerge   (≈ ~/.local/share/qos-chat/…)
//! ```
//!
//! Saving the unmodified Automerge save (not a lossy JSON projection) means
//! a reload is a faithful restore and a future peer can merge against the
//! file directly. Writes are debounced on a background thread and committed
//! atomically (temp file + rename) so a crash mid-write can't corrupt the
//! store.
//!
//! `ui` itself never touches disk (its `State::save`/`load` only convert
//! to/from bytes in memory), so persistence is owned here by the app.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// File name of the Automerge save inside the data directory.
pub const WORKSPACE_FILE: &str = "workspace.automerge";

/// Resolve the data directory: `$QOS_CHAT_DATA` when set (the OS points chat
/// at a per-sandbox store, and tests pin a temp dir here to stay hermetic),
/// else `$XDG_DATA_HOME/qos-chat`, falling back to `$HOME/.local/share/qos-chat`.
/// `None` only if none can be located (extreme minimal containers) — the binary
/// then runs in-memory only.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("QOS_CHAT_DATA") {
        return Some(PathBuf::from(dir));
    }
    dirs::data_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/share")))
        .map(|d| d.join("qos-chat"))
}

/// Path of the Automerge save inside `dir`.
pub fn workspace_path(dir: &Path) -> PathBuf {
    dir.join(WORKSPACE_FILE)
}

/// Read the persisted Automerge bytes, if a store exists. `Ok(None)` on
/// first launch (no file yet) — the caller seeds a fresh workspace.
pub fn load_bytes(dir: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(workspace_path(dir)) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Write the Automerge bytes atomically (temp + rename).
pub fn save_bytes(dir: &Path, bytes: &[u8]) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let dest = workspace_path(dir);
    let tmp = dest.with_extension("automerge.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &dest)?;
    Ok(())
}

/// A debounced background saver for the document bytes. The UI thread calls
/// [`Saver::request`] with the latest `doc.save()` after each mutation; the
/// worker coalesces a burst of edits and writes once the user pauses.
pub struct Saver {
    tx: Sender<Msg>,
    _join: Option<thread::JoinHandle<()>>,
}

enum Msg {
    Save(Vec<u8>),
    Flush,
    Stop,
}

impl Saver {
    /// Spawn the saver targeting `dir` with the given debounce window.
    /// Returns `None` if the data directory can't be resolved — the app
    /// keeps working purely in memory.
    pub fn spawn(dir: PathBuf, debounce: Duration) -> Self {
        let (tx, rx) = mpsc::channel::<Msg>();
        let pending: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
        let join = thread::Builder::new()
            .name("chat-saver".into())
            .spawn(move || saver_loop(rx, pending, dir, debounce))
            .expect("spawn chat saver thread");
        Self { tx, _join: Some(join) }
    }

    /// Request a save of `bytes`; the write lands after the debounce window
    /// elapses without further requests.
    pub fn request(&self, bytes: Vec<u8>) {
        let _ = self.tx.send(Msg::Save(bytes));
    }

    /// Force an immediate flush of any pending bytes.
    pub fn flush(&self) {
        let _ = self.tx.send(Msg::Flush);
    }
}

impl Drop for Saver {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Flush);
        let _ = self.tx.send(Msg::Stop);
        if let Some(j) = self._join.take() {
            let _ = j.join();
        }
    }
}

fn saver_loop(
    rx: Receiver<Msg>,
    pending: Arc<Mutex<Option<Vec<u8>>>>,
    dir: PathBuf,
    debounce: Duration,
) {
    let mut last_request: Option<Instant> = None;
    loop {
        let timeout = match (last_request, pending.lock().unwrap().is_some()) {
            (Some(t), true) => {
                let elapsed = t.elapsed();
                if elapsed >= debounce { Duration::ZERO } else { debounce - elapsed }
            }
            _ => Duration::from_secs(60 * 60),
        };
        let msg = if timeout == Duration::ZERO {
            match rx.try_recv() {
                Ok(m) => Some(m),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        } else {
            match rx.recv_timeout(timeout) {
                Ok(m) => Some(m),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        };
        match msg {
            Some(Msg::Save(bytes)) => {
                *pending.lock().unwrap() = Some(bytes);
                last_request = Some(Instant::now());
            }
            Some(Msg::Stop) => return,
            Some(Msg::Flush) | None => {
                let to_write = pending.lock().unwrap().take();
                if let Some(bytes) = to_write {
                    if let Err(e) = save_bytes(&dir, &bytes) {
                        tracing::error!(error = %e, "chat: workspace save failed");
                    }
                }
                last_request = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_returns_none_when_missing() {
        let tmp = TempDir::new().unwrap();
        assert!(load_bytes(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn save_then_load_roundtrips_bytes() {
        let tmp = TempDir::new().unwrap();
        let bytes = b"automerge-save-bytes".to_vec();
        save_bytes(tmp.path(), &bytes).unwrap();
        let back = load_bytes(tmp.path()).unwrap().expect("present");
        assert_eq!(back, bytes);
    }

    #[test]
    fn save_overwrites_in_place() {
        let tmp = TempDir::new().unwrap();
        save_bytes(tmp.path(), b"v1").unwrap();
        save_bytes(tmp.path(), b"v2").unwrap();
        assert_eq!(load_bytes(tmp.path()).unwrap().unwrap(), b"v2");
    }
}
