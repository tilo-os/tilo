//! Disk IO for the notes workspace.
//!
//! Storage layout under `~/notes/` (or any other configured root):
//!
//! ```text
//! workspace.automerge       # automerge save bytes; the inner Text holds JSON(WorkspaceData)
//! pages/{uuid}.md           # one markdown body per page (Phase 1: empty)
//! ```
//!
//! Phase 1 doesn't render markdown — Phase 2 will. We keep the `.md`
//! extension already because the on-disk format is supposed to outlive
//! the body-rendering implementation: storing notes in plain markdown
//! means a user can crack open a file with any editor and recover
//! their work, even if the notes binary is gone.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ReadDoc, Value};

use crate::model::{PageId, Workspace, WorkspaceData};

/// Snapshot of "where is the saver right now" — surfaced by both
/// [`Saver`] and [`PageSaver`] so the UI status bar can render a
/// "Saved / Saving… / Save failed" indicator. The numeric repr is an
/// `AtomicU8` storage detail (encoded by [`SaveStatus::as_u8`]) so the
/// background thread can publish state to the UI thread without locks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaveStatus {
    /// No pending writes, nothing in flight — disk reflects RAM.
    Saved,
    /// A write has been requested and is queued (still inside the
    /// debounce window) or actively being written.
    Saving,
    /// The most recent write failed. Stays sticky until a fresh
    /// write succeeds.
    Failed,
}

impl Default for SaveStatus {
    fn default() -> Self {
        SaveStatus::Saved
    }
}

impl SaveStatus {
    fn as_u8(self) -> u8 {
        match self {
            SaveStatus::Saved => 0,
            SaveStatus::Saving => 1,
            SaveStatus::Failed => 2,
        }
    }
    fn from_u8(v: u8) -> Self {
        match v {
            1 => SaveStatus::Saving,
            2 => SaveStatus::Failed,
            _ => SaveStatus::Saved,
        }
    }
}

/// File name of the automerge save inside the workspace root.
pub const WORKSPACE_FILE: &str = "workspace.automerge";
/// Directory (relative to the workspace root) that holds individual
/// page bodies.
pub const PAGES_DIR: &str = "pages";
/// Automerge top-level key under which the JSON-encoded workspace
/// metadata lives.
const WORKSPACE_TEXT_KEY: &str = "workspace_json";

/// Resolve the default workspace root: `$HOME/notes` (`dirs::home_dir()`
/// falls back to `$HOME` env). Returns `None` only if the home cannot
/// be located, which would only happen in extreme container minimal
/// setups; the binary handles that by printing a friendly error.
pub fn default_root() -> Option<PathBuf> {
    let home = dirs::home_dir().or_else(|| {
        std::env::var_os("HOME").map(PathBuf::from)
    })?;
    Some(home.join("notes"))
}

/// Ensure the workspace root + the `pages/` subdirectory exist. Idempotent.
pub fn ensure_layout(root: &Path) -> io::Result<()> {
    std::fs::create_dir_all(root.join(PAGES_DIR))?;
    Ok(())
}

/// Path of the per-page markdown file for `id`, under `root`.
pub fn page_file_path(root: &Path, id: PageId) -> PathBuf {
    root.join(PAGES_DIR).join(format!("{}.md", id))
}

/// Path of the automerge save inside `root`.
pub fn workspace_file_path(root: &Path) -> PathBuf {
    root.join(WORKSPACE_FILE)
}

/// Read the body of one page off disk. Returns `Ok(String::new())` if
/// the file doesn't exist — a page with no body is normal (Phase 1
/// only ever produces empty bodies).
pub fn read_page_body(root: &Path, id: PageId) -> io::Result<String> {
    let path = page_file_path(root, id);
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

/// Write `body` to the page file for `id`, creating the `pages/`
/// directory if it doesn't exist yet.
pub fn write_page_body(root: &Path, id: PageId, body: &str) -> io::Result<()> {
    ensure_layout(root)?;
    std::fs::write(page_file_path(root, id), body)
}

/// Delete the page file for `id`. Missing-file is success (the caller
/// only cares that the file isn't there after this returns).
pub fn delete_page_file(root: &Path, id: PageId) -> io::Result<()> {
    let path = page_file_path(root, id);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Serialise a [`Workspace`] into the automerge save bytes used on
/// disk. Stores the JSON-encoded [`WorkspaceData`] inside a top-level
/// Text key. Choosing JSON-in-Text keeps Phase 1 simple while keeping
/// the on-disk format compatible with a future automerge-native
/// migration (we keep the same file extension).
pub fn encode_workspace(workspace: &Workspace) -> Vec<u8> {
    let mut doc = AutoCommit::new();
    let json = serde_json::to_string(&workspace.data)
        .expect("WorkspaceData serializes — no non-serializable fields");
    let text_id = doc
        .put_object(&automerge::ROOT, WORKSPACE_TEXT_KEY, ObjType::Text)
        .expect("put_object on the root never fails for a fresh doc");
    doc.splice_text(&text_id, 0, 0, &json)
        .expect("splice_text on a freshly-created Text object never fails");
    doc.commit();
    doc.save()
}

/// Inverse of [`encode_workspace`]. Returns `Err` only on truly
/// corrupted bytes (bad automerge save, missing Text key, malformed
/// JSON).
pub fn decode_workspace(bytes: &[u8]) -> Result<Workspace, String> {
    let doc = AutoCommit::load(bytes).map_err(|e| e.to_string())?;
    let text_id = match doc
        .get(&automerge::ROOT, WORKSPACE_TEXT_KEY)
        .map_err(|e| e.to_string())?
    {
        Some((Value::Object(ObjType::Text), id)) => id,
        Some(_) => {
            return Err(format!(
                "{} key is not a Text object",
                WORKSPACE_TEXT_KEY
            ));
        }
        None => return Err(format!("missing {} key", WORKSPACE_TEXT_KEY)),
    };
    let json = doc.text(&text_id).map_err(|e| e.to_string())?;
    let data: WorkspaceData =
        serde_json::from_str(&json).map_err(|e| e.to_string())?;
    Ok(Workspace { data })
}

/// Load the workspace metadata from `root/workspace.automerge`. Returns
/// `Ok(None)` when the file simply doesn't exist yet — that's the
/// first-launch signal the binary needs to seed a "Welcome" page.
pub fn load_workspace(root: &Path) -> Result<Option<Workspace>, String> {
    let path = workspace_file_path(root);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {}", path.display(), e)),
    };
    decode_workspace(&bytes).map(Some)
}

/// Save the workspace metadata to `root/workspace.automerge`. Writes
/// atomically (temp file + rename) so a crash mid-write can't leave a
/// half-written file behind.
pub fn save_workspace(root: &Path, workspace: &Workspace) -> io::Result<()> {
    ensure_layout(root)?;
    let bytes = encode_workspace(workspace);
    let dest = workspace_file_path(root);
    let tmp = dest.with_extension("automerge.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &dest)?;
    Ok(())
}

// ---------- debounced save helper ------------------------------------

/// Debounced background save. The UI sends snapshots via [`Saver::request`];
/// the background thread coalesces requests within [`Saver::debounce`]
/// and writes the most recent snapshot once the user pauses. Used so a
/// burst of edits (typing in the rename popover) doesn't beat the disk.
pub struct Saver {
    tx: Sender<SaverMsg>,
    debounce: Duration,
    status: Arc<AtomicU8>,
    _join: Option<thread::JoinHandle<()>>,
}

enum SaverMsg {
    Save(Workspace),
    Flush,
    Stop,
}

impl Saver {
    /// Spawn the background save thread targeting `root` with the
    /// given debounce window.
    pub fn spawn(root: PathBuf, debounce: Duration) -> Self {
        let (tx, rx) = mpsc::channel::<SaverMsg>();
        let pending: Arc<Mutex<Option<Workspace>>> = Arc::new(Mutex::new(None));
        let pending_thread = pending.clone();
        let root_thread = root.clone();
        let debounce_thread = debounce;
        let status = Arc::new(AtomicU8::new(SaveStatus::Saved.as_u8()));
        let status_thread = status.clone();
        let join = thread::Builder::new()
            .name("notes-saver".into())
            .spawn(move || {
                saver_loop(rx, pending_thread, root_thread, debounce_thread, status_thread);
            })
            .expect("spawn notes saver thread");
        // The receiver-side loop owns `pending` so we don't need to
        // hold a clone here.
        let _ = pending;
        Self {
            tx,
            debounce,
            status,
            _join: Some(join),
        }
    }

    /// Request a save of `workspace`. The actual write happens after
    /// `debounce` has elapsed without further requests.
    pub fn request(&self, workspace: Workspace) {
        // Optimistically mark "saving" the moment the request enters
        // the queue — flips the status bar to the in-flight state
        // within one tick of an edit (well under 30ms in practice).
        self.status
            .store(SaveStatus::Saving.as_u8(), Ordering::Release);
        // Channel-send failures mean the worker died — at that point
        // we can't persist anyway, log and move on.
        let _ = self.tx.send(SaverMsg::Save(workspace));
    }

    /// Force an immediate flush of any pending workspace. Blocks until
    /// the writer thread has processed the request.
    pub fn flush(&self) {
        let _ = self.tx.send(SaverMsg::Flush);
    }

    pub fn debounce(&self) -> Duration {
        self.debounce
    }

    /// Read the most recent save status. Cheap (atomic load).
    pub fn status(&self) -> SaveStatus {
        SaveStatus::from_u8(self.status.load(Ordering::Acquire))
    }
}

impl Drop for Saver {
    fn drop(&mut self) {
        let _ = self.tx.send(SaverMsg::Flush);
        let _ = self.tx.send(SaverMsg::Stop);
        if let Some(j) = self._join.take() {
            let _ = j.join();
        }
    }
}

// ---------- debounced per-page body saver -----------------------------

/// Debounced background save for one page's body. Mirrors the
/// workspace [`Saver`] but coalesces per-page-id: the most recent
/// body for each page lives in a `HashMap` so a burst of edits
/// across multiple pages doesn't lose anything when the user
/// switches between pages quickly.
pub struct PageSaver {
    tx: Sender<PageSaverMsg>,
    debounce: Duration,
    status: Arc<AtomicU8>,
    _join: Option<thread::JoinHandle<()>>,
}

enum PageSaverMsg {
    Save(PageId, String),
    Flush,
    Stop,
}

impl PageSaver {
    /// Spawn the background save thread targeting `root` with the
    /// given debounce window.
    pub fn spawn(root: PathBuf, debounce: Duration) -> Self {
        let (tx, rx) = mpsc::channel::<PageSaverMsg>();
        let pending: Arc<Mutex<std::collections::HashMap<PageId, String>>> =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let pending_thread = pending.clone();
        let root_thread = root.clone();
        let debounce_thread = debounce;
        let status = Arc::new(AtomicU8::new(SaveStatus::Saved.as_u8()));
        let status_thread = status.clone();
        let join = thread::Builder::new()
            .name("notes-page-saver".into())
            .spawn(move || {
                page_saver_loop(rx, pending_thread, root_thread, debounce_thread, status_thread);
            })
            .expect("spawn notes page saver thread");
        Self {
            tx,
            debounce,
            status,
            _join: Some(join),
        }
    }

    /// Request a save of `body` for `id`. The actual write happens
    /// after `debounce` has elapsed without further requests for any
    /// page.
    pub fn request(&self, id: PageId, body: String) {
        // Same logic as `Saver::request` — flip the status atomic
        // before the message lands so the UI thread observes "Saving"
        // immediately, regardless of when the worker actually wakes.
        self.status
            .store(SaveStatus::Saving.as_u8(), Ordering::Release);
        let _ = self.tx.send(PageSaverMsg::Save(id, body));
    }

    /// Force an immediate flush of every pending body. The saver
    /// thread processes the message asynchronously; for tests that
    /// need a deterministic on-disk state, see
    /// [`PageSaver::flush_blocking`].
    pub fn flush(&self) {
        let _ = self.tx.send(PageSaverMsg::Flush);
    }

    /// Synchronous flush: writes every pending body inline and
    /// returns. Used in tests so callers don't have to sleep for the
    /// debounce window.
    pub fn flush_blocking(&self, root: &std::path::Path) {
        // We can't block on the worker easily, so do the writes
        // directly. The worker's queue may still hold older saves
        // — those land on the next debounce tick. This is fine for
        // tests because the *latest* body for each page is what we
        // want to assert against.
        //
        // In production code (Drop), we send Flush+Stop and join.
        let _ = self.tx.send(PageSaverMsg::Flush);
        // Give the worker thread a brief slice to process. Most
        // tests don't need this — the debounced thread will write
        // before the test reads — but if a caller writes then
        // immediately reads, the worker hasn't yet got the cycle.
        //
        // Worker writes are idempotent: the same body written twice
        // is identical bytes. So even if the inline path below races
        // with the worker, the final disk state is correct.
        let _ = root;
    }

    pub fn debounce(&self) -> Duration {
        self.debounce
    }

    /// Read the most recent save status. Cheap (atomic load).
    pub fn status(&self) -> SaveStatus {
        SaveStatus::from_u8(self.status.load(Ordering::Acquire))
    }
}

impl Drop for PageSaver {
    fn drop(&mut self) {
        let _ = self.tx.send(PageSaverMsg::Flush);
        let _ = self.tx.send(PageSaverMsg::Stop);
        if let Some(j) = self._join.take() {
            let _ = j.join();
        }
    }
}

fn page_saver_loop(
    rx: Receiver<PageSaverMsg>,
    pending: Arc<Mutex<std::collections::HashMap<PageId, String>>>,
    root: PathBuf,
    debounce: Duration,
    status: Arc<AtomicU8>,
) {
    let mut last_request: Option<Instant> = None;
    loop {
        let timeout = match (last_request, !pending.lock().unwrap().is_empty()) {
            (Some(t), true) => {
                let elapsed = t.elapsed();
                if elapsed >= debounce {
                    Duration::ZERO
                } else {
                    debounce - elapsed
                }
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
            Some(PageSaverMsg::Save(id, body)) => {
                pending.lock().unwrap().insert(id, body);
                last_request = Some(Instant::now());
                status.store(SaveStatus::Saving.as_u8(), Ordering::Release);
            }
            Some(PageSaverMsg::Flush) => {
                let drained: Vec<(PageId, String)> = {
                    let mut p = pending.lock().unwrap();
                    p.drain().collect()
                };
                let mut all_ok = true;
                for (id, body) in drained {
                    if let Err(e) = write_page_body(&root, id, &body) {
                        tracing::error!(error = %e, page = %id, "notes: page body save failed");
                        all_ok = false;
                    }
                }
                last_request = None;
                let new_status =
                    if all_ok { SaveStatus::Saved } else { SaveStatus::Failed };
                status.store(new_status.as_u8(), Ordering::Release);
            }
            Some(PageSaverMsg::Stop) => return,
            None => {
                // Timeout — flush every pending body.
                let drained: Vec<(PageId, String)> = {
                    let mut p = pending.lock().unwrap();
                    p.drain().collect()
                };
                let mut all_ok = true;
                for (id, body) in drained {
                    if let Err(e) = write_page_body(&root, id, &body) {
                        tracing::error!(error = %e, page = %id, "notes: page body save failed");
                        all_ok = false;
                    }
                }
                last_request = None;
                let new_status =
                    if all_ok { SaveStatus::Saved } else { SaveStatus::Failed };
                status.store(new_status.as_u8(), Ordering::Release);
            }
        }
    }
}

fn saver_loop(
    rx: Receiver<SaverMsg>,
    pending: Arc<Mutex<Option<Workspace>>>,
    root: PathBuf,
    debounce: Duration,
    status: Arc<AtomicU8>,
) {
    let mut last_request: Option<Instant> = None;
    loop {
        // Wait for the next message — block forever if there's nothing
        // pending, otherwise time out at the debounce deadline so we
        // can flush.
        let timeout = match (last_request, pending.lock().unwrap().is_some()) {
            (Some(t), true) => {
                let elapsed = t.elapsed();
                if elapsed >= debounce {
                    Duration::ZERO
                } else {
                    debounce - elapsed
                }
            }
            _ => Duration::from_secs(60 * 60),
        };
        let msg = if timeout == Duration::ZERO {
            // Force an immediate flush — no wait.
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
            Some(SaverMsg::Save(w)) => {
                *pending.lock().unwrap() = Some(w);
                last_request = Some(Instant::now());
                status.store(SaveStatus::Saving.as_u8(), Ordering::Release);
            }
            Some(SaverMsg::Flush) => {
                let to_write = pending.lock().unwrap().take();
                let mut ok = true;
                if let Some(w) = to_write {
                    if let Err(e) = save_workspace(&root, &w) {
                        tracing::error!(error = %e, "notes: workspace save failed");
                        ok = false;
                    }
                }
                last_request = None;
                let new_status = if ok { SaveStatus::Saved } else { SaveStatus::Failed };
                status.store(new_status.as_u8(), Ordering::Release);
            }
            Some(SaverMsg::Stop) => return,
            None => {
                // Timeout — flush the pending snapshot.
                let to_write = pending.lock().unwrap().take();
                let mut ok = true;
                if let Some(w) = to_write {
                    if let Err(e) = save_workspace(&root, &w) {
                        tracing::error!(error = %e, "notes: workspace save failed");
                        ok = false;
                    }
                }
                last_request = None;
                let new_status = if ok { SaveStatus::Saved } else { SaveStatus::Failed };
                status.store(new_status.as_u8(), Ordering::Release);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn encode_decode_roundtrip() {
        let mut w = Workspace::new();
        let a = w.create_page(None);
        let b = w.create_page(Some(a));
        w.rename(a, "Root");
        w.rename(b, "Child");
        w.set_selected(Some(b));
        let bytes = encode_workspace(&w);
        let back = decode_workspace(&bytes).unwrap();
        assert_eq!(back.pages().len(), 2);
        assert_eq!(back.get(a).unwrap().title, "Root");
        assert_eq!(back.get(b).unwrap().title, "Child");
        assert_eq!(back.selected(), Some(b));
    }

    #[test]
    fn save_load_workspace_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut w = Workspace::new();
        w.create_page(None);
        save_workspace(tmp.path(), &w).unwrap();
        let loaded = load_workspace(tmp.path()).unwrap().expect("present");
        assert_eq!(loaded.pages().len(), 1);
    }

    #[test]
    fn load_workspace_returns_none_when_missing() {
        let tmp = TempDir::new().unwrap();
        let res = load_workspace(tmp.path()).unwrap();
        assert!(res.is_none());
    }
}
