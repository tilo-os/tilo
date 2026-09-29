//! Disk IO for the notes workspace.
//!
//! Storage layout under `~/notes/` (or any other configured root):
//!
//! ```text
//! workspace.automerge       # automerge save bytes; the inner Text holds JSON(WorkspaceData)
//! pages/{uuid}.md           # one markdown body per page
//! ```
//!
//! Page bodies remain plain Markdown so they can be recovered with another
//! editor independently of the Notes application.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use ui::time::{Duration, Instant};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ReadDoc, Value};

use crate::model::{PageId, Workspace, WorkspaceData};

/// Snapshot of "where is the saver right now" — surfaced by both
/// [`Saver`] and [`PageSaver`] so the UI status bar can render a
/// "Saved / Saving… / Save failed" indicator. The numeric repr is an
/// `AtomicU8` storage detail (encoded by `SaveStatus::as_u8`) so the
/// background thread can publish state to the UI thread without locks.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum SaveStatus {
    /// No pending writes, nothing in flight — disk reflects RAM.
    #[default]
    Saved,
    /// A write has been requested and is queued (still inside the
    /// debounce window) or actively being written.
    Saving,
    /// The most recent write failed. Stays sticky until a fresh
    /// write succeeds.
    Failed,
    /// The record exists only in this session and has no durable writer.
    SessionOnly,
}

impl SaveStatus {
    fn as_u8(self) -> u8 {
        match self {
            SaveStatus::Saved => 0,
            SaveStatus::Saving => 1,
            SaveStatus::Failed => 2,
            SaveStatus::SessionOnly => 3,
        }
    }
    fn from_u8(v: u8) -> Self {
        match v {
            1 => SaveStatus::Saving,
            2 => SaveStatus::Failed,
            3 => SaveStatus::SessionOnly,
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

/// Resolve `TILO_NOTES_ROOT`, or the native home directory's `notes` folder.
/// Returns `None` only when no home directory can be located.
///
/// # Errors
/// An explicit override must be nonempty and absolute. Invalid configuration
/// never falls back to another notebook.
pub fn default_root() -> Result<Option<PathBuf>, String> {
    if let Some(value) = std::env::var_os("TILO_NOTES_ROOT") {
        let root = PathBuf::from(value);
        if root.as_os_str().is_empty() || !root.is_absolute() {
            return Err("TILO_NOTES_ROOT must be a nonempty absolute directory path".into());
        }
        return Ok(Some(root));
    }
    Ok(tilo_paths::home().map(|home| home.join("notes")))
}

/// Ensure the workspace root + the `pages/` subdirectory exist. Idempotent.
///
/// # Errors
///
/// Returns `Err` if creating the directories fails (e.g. permissions).
pub fn ensure_layout(root: &Path) -> io::Result<()> {
    crate::files::create_dir_all(&root.join(PAGES_DIR)).map_err(crate::files::io)?;
    Ok(())
}

/// Path of the per-page markdown file for `id`, under `root`.
pub fn page_file_path(root: &Path, id: PageId) -> PathBuf {
    root.join(PAGES_DIR).join(format!("{id}.md"))
}

/// Path of the automerge save inside `root`.
pub fn workspace_file_path(root: &Path) -> PathBuf {
    root.join(WORKSPACE_FILE)
}

/// Read the body of one page off disk. A missing file is an empty page.
///
/// # Errors
///
/// Returns `Err` for any IO failure other than the file not existing
/// (permissions, invalid UTF-8, etc.).
pub fn read_page_body(root: &Path, id: PageId) -> io::Result<String> {
    let path = page_file_path(root, id);
    match crate::files::read_to_string(&path) {
        Ok(s) => Ok(s),
        Err(tilo_files::FsError::NotFound) => Ok(String::new()),
        Err(e) => Err(crate::files::io(e)),
    }
}

/// Write `body` to the page file for `id`, creating the `pages/`
/// directory if it doesn't exist yet.
///
/// # Errors
///
/// Returns `Err` if the directory can't be created or the atomic
/// write fails.
pub fn write_page_body(root: &Path, id: PageId, body: &str) -> io::Result<()> {
    ensure_layout(root)?;
    // These are the user's real markdown files. A plain write truncates in
    // place, so a crash between the truncate and the write loses the note
    // outright; replacing the file leaves the previous text readable.
    crate::files::write(&page_file_path(root, id), body.as_bytes()).map_err(crate::files::io)
}

/// Delete the page file for `id`. Missing-file is success (the caller
/// only cares that the file isn't there after this returns).
///
/// # Errors
///
/// Returns `Err` for any IO failure other than the file already being
/// absent (e.g. permissions).
pub fn delete_page_file(root: &Path, id: PageId) -> io::Result<()> {
    let path = page_file_path(root, id);
    match crate::files::remove(&path) {
        Ok(()) | Err(tilo_files::FsError::NotFound) => Ok(()),
        Err(e) => Err(crate::files::io(e)),
    }
}

/// Serialise a [`Workspace`] into the automerge save bytes used on
/// disk. Stores the JSON-encoded [`WorkspaceData`] inside a top-level Text key.
///
/// # Errors
///
/// Returns `Err` only if `workspace.data` fails to serialize to JSON;
/// the automerge operations that follow are all against a
/// freshly-created document and cannot themselves fail.
pub fn encode_workspace(workspace: &Workspace) -> Result<Vec<u8>, String> {
    let mut doc = AutoCommit::new();
    let json = serde_json::to_string(&workspace.data).map_err(|e| e.to_string())?;
    let text_id = doc
        .put_object(&automerge::ROOT, WORKSPACE_TEXT_KEY, ObjType::Text)
        .map_err(|e| e.to_string())?;
    doc.splice_text(&text_id, 0, 0, &json)
        .map_err(|e| e.to_string())?;
    doc.commit();
    Ok(doc.save())
}

/// Inverse of [`encode_workspace`]. Returns `Err` only on truly
/// corrupted bytes (bad automerge save, missing Text key, malformed
/// JSON).
///
/// # Errors
///
/// Returns `Err` if `bytes` isn't a valid automerge save, is missing
/// the workspace Text key, or its contents aren't valid
/// [`WorkspaceData`] JSON.
pub fn decode_workspace(bytes: &[u8]) -> Result<Workspace, String> {
    let doc = AutoCommit::load(bytes).map_err(|e| e.to_string())?;
    let text_id = match doc
        .get(&automerge::ROOT, WORKSPACE_TEXT_KEY)
        .map_err(|e| e.to_string())?
    {
        Some((Value::Object(ObjType::Text), id)) => id,
        Some(_) => {
            return Err(format!("{WORKSPACE_TEXT_KEY} key is not a Text object"));
        }
        None => return Err(format!("missing {WORKSPACE_TEXT_KEY} key")),
    };
    let json = doc.text(&text_id).map_err(|e| e.to_string())?;
    let data: WorkspaceData = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    Ok(Workspace { data })
}

/// Load the workspace metadata from `root/workspace.automerge`. Returns
/// `Ok(None)` when the file simply doesn't exist yet — that's the
/// first-launch signal the binary needs to seed a "Welcome" page.
///
/// # Errors
///
/// Returns `Err` if the file exists but can't be read, or its
/// contents fail to decode (see [`decode_workspace`]).
pub fn load_workspace(root: &Path) -> Result<Option<Workspace>, String> {
    let path = workspace_file_path(root);
    let bytes = match crate::files::read(&path) {
        Ok(b) => b,
        Err(tilo_files::FsError::NotFound) => return Ok(None),
        Err(e) => return Err(format!("read {}: {}", path.display(), e)),
    };
    decode_workspace(&bytes).map(Some)
}

/// Save the workspace metadata to `root/workspace.automerge`. Writes
/// atomically (temp file + rename) so a crash mid-write can't leave a
/// half-written file behind.
///
/// # Errors
///
/// Returns `Err` if the workspace directory can't be created, the
/// workspace fails to encode, or the atomic write fails.
pub fn save_workspace(root: &Path, workspace: &Workspace) -> io::Result<()> {
    ensure_layout(root)?;
    let bytes = encode_workspace(workspace).map_err(io::Error::other)?;
    crate::files::write(&workspace_file_path(root), &bytes).map_err(crate::files::io)?;
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
    join_handle: Option<thread::JoinHandle<()>>,
}

enum SaverMsg {
    Save(Workspace),
    Flush(Option<Sender<Result<(), String>>>),
    Stop,
}

impl Saver {
    /// Spawn the background save thread targeting `root` with the
    /// given debounce window.
    ///
    /// # Errors
    ///
    /// Returns `Err` only if the OS refuses to spawn the background
    /// save thread (e.g. resource exhaustion).
    pub fn spawn(root: PathBuf, debounce: Duration) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel::<SaverMsg>();
        let pending: Arc<Mutex<Option<Workspace>>> = Arc::new(Mutex::new(None));
        let pending_thread = pending;
        let root_thread = root;
        let debounce_thread = debounce;
        let status = Arc::new(AtomicU8::new(SaveStatus::Saved.as_u8()));
        let status_thread = status.clone();
        let join_handle = ui::rt::spawn_thread("notes-saver", move || {
            saver_loop(
                &rx,
                &pending_thread,
                &root_thread,
                debounce_thread,
                &status_thread,
            );
        })?;
        Ok(Self {
            tx,
            debounce,
            status,
            join_handle: Some(join_handle),
        })
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

    /// Queue an immediate flush of any pending workspace.
    pub fn flush(&self) {
        let _ = self.tx.send(SaverMsg::Flush(None));
    }

    /// Wait for queued saves and report failed or disconnected storage.
    ///
    /// # Errors
    /// Returns a message when the saver thread has gone away, when the flush
    /// does not complete within 30 seconds, or when a queued save failed.
    pub fn try_flush(&self) -> Result<(), String> {
        let (sender, receiver) = mpsc::channel();
        self.tx
            .send(SaverMsg::Flush(Some(sender)))
            .map_err(|_| "Notebook writer disconnected".to_string())?;
        receiver
            .recv_timeout(Duration::from_secs(30))
            .map_err(|error| format!("Notebook flush did not complete: {error}"))?
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
        let _ = self.tx.send(SaverMsg::Flush(None));
        let _ = self.tx.send(SaverMsg::Stop);
        if let Some(j) = self.join_handle.take() {
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
    join_handle: Option<thread::JoinHandle<()>>,
}

enum PageSaverMsg {
    Save(PageId, String),
    Flush(Option<Sender<Result<(), String>>>),
    Stop,
}

impl PageSaver {
    /// Spawn the background save thread targeting `root` with the
    /// given debounce window.
    ///
    /// # Errors
    ///
    /// Returns `Err` only if the OS refuses to spawn the background
    /// save thread (e.g. resource exhaustion).
    pub fn spawn(root: PathBuf, debounce: Duration) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel::<PageSaverMsg>();
        let pending: Arc<Mutex<std::collections::HashMap<PageId, String>>> =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let pending_thread = pending;
        let root_thread = root;
        let debounce_thread = debounce;
        let status = Arc::new(AtomicU8::new(SaveStatus::Saved.as_u8()));
        let status_thread = status.clone();
        let join_handle = ui::rt::spawn_thread("notes-page-saver", move || {
            page_saver_loop(
                &rx,
                &pending_thread,
                &root_thread,
                debounce_thread,
                &status_thread,
            );
        })?;
        Ok(Self {
            tx,
            debounce,
            status,
            join_handle: Some(join_handle),
        })
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
        let _ = self.tx.send(PageSaverMsg::Flush(None));
    }

    /// Wait for queued bodies and report failed or disconnected storage.
    ///
    /// # Errors
    /// Returns a message when the body saver thread has gone away, when the
    /// flush does not complete within 30 seconds, or when a queued write failed.
    pub fn try_flush(&self) -> Result<(), String> {
        let (sender, receiver) = mpsc::channel();
        self.tx
            .send(PageSaverMsg::Flush(Some(sender)))
            .map_err(|_| "Note body writer disconnected".to_string())?;
        receiver
            .recv_timeout(Duration::from_secs(30))
            .map_err(|error| format!("Note body flush did not complete: {error}"))?
    }

    /// Compatibility helper for callers that need a completed body flush.
    /// Prefer `try_flush` when the caller can report storage failures.
    pub fn flush_blocking(&self, _root: &std::path::Path) {
        if let Err(error) = self.try_flush() {
            tracing::error!(%error, "notes: blocking body flush failed");
        }
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
        let _ = self.tx.send(PageSaverMsg::Flush(None));
        let _ = self.tx.send(PageSaverMsg::Stop);
        if let Some(j) = self.join_handle.take() {
            let _ = j.join();
        }
    }
}

/// Lock `m`, recovering the inner value if a previous holder panicked
/// mid-write rather than propagating the poison — a poisoned lock here
/// would otherwise take down the whole background save thread, and
/// carrying on with the last-known-good state beats not saving at all.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn page_saver_loop(
    rx: &Receiver<PageSaverMsg>,
    pending: &Arc<Mutex<std::collections::HashMap<PageId, String>>>,
    root: &Path,
    debounce: Duration,
    status: &Arc<AtomicU8>,
) {
    let mut last_request: Option<Instant> = None;
    loop {
        let has_pending = !lock(pending).is_empty();
        let timeout = match (last_request, has_pending) {
            (Some(t), true) => {
                let elapsed = t.elapsed();
                if elapsed >= debounce {
                    Duration::ZERO
                } else {
                    debounce.checked_sub(elapsed).unwrap_or(Duration::ZERO)
                }
            }
            _ => Duration::from_secs(3600),
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
                lock(pending).insert(id, body);
                last_request = Some(ui::time::now());
                status.store(SaveStatus::Saving.as_u8(), Ordering::Release);
            }
            Some(PageSaverMsg::Flush(ack)) => {
                let result = flush_page_bodies(pending, root);
                last_request = None;
                status.store(
                    if result.is_ok() {
                        SaveStatus::Saved
                    } else {
                        SaveStatus::Failed
                    }
                    .as_u8(),
                    Ordering::Release,
                );
                if let Some(ack) = ack {
                    let _ = ack.send(result);
                }
            }
            Some(PageSaverMsg::Stop) => return,
            None => {
                let result = flush_page_bodies(pending, root);
                last_request = None;
                status.store(
                    if result.is_ok() {
                        SaveStatus::Saved
                    } else {
                        SaveStatus::Failed
                    }
                    .as_u8(),
                    Ordering::Release,
                );
            }
        }
    }
}

fn flush_page_bodies(
    pending: &Arc<Mutex<std::collections::HashMap<PageId, String>>>,
    root: &Path,
) -> Result<(), String> {
    let drained: Vec<_> = lock(pending).drain().collect();
    let mut errors = Vec::new();
    for (id, body) in drained {
        if let Err(error) = write_page_body(root, id, &body) {
            errors.push(error.to_string());
            lock(pending).insert(id, body);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn flush_workspace(pending: &Arc<Mutex<Option<Workspace>>>, root: &Path) -> Result<(), String> {
    let Some(workspace) = lock(pending).take() else {
        return Ok(());
    };
    if let Err(error) = save_workspace(root, &workspace) {
        *lock(pending) = Some(workspace);
        return Err(error.to_string());
    }
    Ok(())
}

fn saver_loop(
    rx: &Receiver<SaverMsg>,
    pending: &Arc<Mutex<Option<Workspace>>>,
    root: &Path,
    debounce: Duration,
    status: &Arc<AtomicU8>,
) {
    let mut last_request: Option<Instant> = None;
    loop {
        // Wait for the next message — block forever if there's nothing
        // pending, otherwise time out at the debounce deadline so we
        // can flush.
        let has_pending = lock(pending).is_some();
        let timeout = match (last_request, has_pending) {
            (Some(t), true) => {
                let elapsed = t.elapsed();
                if elapsed >= debounce {
                    Duration::ZERO
                } else {
                    debounce.checked_sub(elapsed).unwrap_or(Duration::ZERO)
                }
            }
            _ => Duration::from_secs(3600),
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
                *lock(pending) = Some(w);
                last_request = Some(ui::time::now());
                status.store(SaveStatus::Saving.as_u8(), Ordering::Release);
            }
            Some(SaverMsg::Flush(ack)) => {
                let result = flush_workspace(pending, root);
                last_request = None;
                status.store(
                    if result.is_ok() {
                        SaveStatus::Saved
                    } else {
                        SaveStatus::Failed
                    }
                    .as_u8(),
                    Ordering::Release,
                );
                if let Some(ack) = ack {
                    let _ = ack.send(result);
                }
            }
            Some(SaverMsg::Stop) => return,
            None => {
                let result = flush_workspace(pending, root);
                last_request = None;
                status.store(
                    if result.is_ok() {
                        SaveStatus::Saved
                    } else {
                        SaveStatus::Failed
                    }
                    .as_u8(),
                    Ordering::Release,
                );
            }
        }
    }
}

/// Exclusive notebook-directory custody, held until every writer has stopped.
/// Older binaries and external editors do not participate: migration still
/// requires an explicit assertion that those writers have been stopped.
/// # Errors
/// Refuses a directory with an existing cooperating writer or unusable lock.
pub fn lock_workspace(root: &Path) -> Result<advisory_lock::FileLock, String> {
    use advisory_lock::{FileLock, LockMode, TryLockError};
    crate::files::create_dir_all(root).map_err(|e| e.to_string())?;
    let path = root.join(".notes-writer.lock");
    let lock = FileLock::try_open(&path, LockMode::Exclusive).map_err(|e| match e {
        TryLockError::WouldBlock => {
            let owner = advisory_lock::owner_pid(&path)
                .map(|pid| format!(" (by process {pid})"))
                .unwrap_or_default();
            format!(
                "notebook is already open{owner}; stop all legacy Notes writers before migration"
            )
        }
        TryLockError::Error(e) => e.to_string(),
    })?;
    lock.record_owner().map_err(|e| e.to_string())?;
    Ok(lock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn configured_root_is_used_by_production_spawn_and_invalid_values_fail() {
        const CHILD: &str = "TILO_NOTES_ROOT_TEST_CHILD";
        if let Ok(mode) = std::env::var(CHILD) {
            if mode == "valid" {
                let root = PathBuf::from(std::env::var_os("TILO_NOTES_ROOT").unwrap());
                ensure_layout(&root).unwrap();
                let mut workspace = Workspace::new();
                let page = workspace.create_page(None);
                workspace.rename(page, "Isolated notebook");
                workspace.set_selected(Some(page));
                save_workspace(&root, &workspace).unwrap();
                std::fs::write(page_file_path(&root, page), "Private fixture body").unwrap();
                let app = crate::NotesApp::spawn_default().unwrap();
                assert_eq!(app.selected_for_test(), Some(page));
                assert_eq!(app.editor_text_for_test(), "Private fixture body");
            } else {
                assert!(default_root().is_err());
                assert!(crate::NotesApp::spawn_default().is_err());
            }
            return;
        }
        let temp = TempDir::new().unwrap();
        for (mode, value) in [
            ("valid", temp.path().join("Notebook ü")),
            ("empty", PathBuf::new()),
            ("relative", PathBuf::from("relative-notebook")),
        ] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "storage::tests::configured_root_is_used_by_production_spawn_and_invalid_values_fail",
                    "--nocapture",
                ])
                .env(CHILD, mode)
                .env("TILO_NOTES_ROOT", value)
                .current_dir(temp.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{mode}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            // libtest exits 0 when `--exact` selects nothing; require the child ran.
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed"),
                "{mode}: the child ran no test: {}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
        assert!(!temp.path().join("relative-notebook").exists());
    }

    #[test]
    fn workspace_storage_round_trips_and_reports_missing_or_failed_writes() {
        eprintln!("phase encode_decode_roundtrip");
        {
            let mut w = Workspace::new();
            let a = w.create_page(None);
            let b = w.create_page(Some(a));
            w.rename(a, "Root");
            w.rename(b, "Child");
            w.set_selected(Some(b));
            let bytes = encode_workspace(&w).unwrap();
            let back = decode_workspace(&bytes).unwrap();
            assert_eq!(back.pages().len(), 2);
            assert_eq!(back.get(a).unwrap().title, "Root");
            assert_eq!(back.get(b).unwrap().title, "Child");
            assert_eq!(back.selected(), Some(b));
        }

        eprintln!("phase save_load_workspace_roundtrip");
        {
            let tmp = TempDir::new().unwrap();
            let mut w = Workspace::new();
            w.create_page(None);
            save_workspace(tmp.path(), &w).unwrap();
            let loaded = load_workspace(tmp.path()).unwrap().expect("present");
            assert_eq!(loaded.pages().len(), 1);
        }

        eprintln!("phase load_workspace_returns_none_when_missing");
        {
            let tmp = TempDir::new().unwrap();
            let res = load_workspace(tmp.path()).unwrap();
            assert!(res.is_none());
        }

        eprintln!("phase checked_flush_reports_and_retains_failed_writes_until_recovery");
        {
            let temp = TempDir::new().unwrap();
            ensure_layout(temp.path()).unwrap();
            let mut workspace = Workspace::new();
            let page = workspace.create_page(None);
            let body_path = page_file_path(temp.path(), page);
            std::fs::create_dir(&body_path).unwrap();
            let bodies =
                PageSaver::spawn(temp.path().to_path_buf(), Duration::from_secs(60)).unwrap();
            bodies.request(page, "Retained body".into());
            assert!(bodies.try_flush().is_err());
            assert_eq!(bodies.status(), SaveStatus::Failed);
            assert!(
                bodies.try_flush().is_err(),
                "empty subsequent flush must not erase a failed write"
            );
            std::fs::remove_dir(&body_path).unwrap();
            bodies.try_flush().unwrap();
            assert_eq!(std::fs::read_to_string(body_path).unwrap(), "Retained body");

            let metadata_path = workspace_file_path(temp.path());
            std::fs::create_dir(&metadata_path).unwrap();
            let metadata =
                Saver::spawn(temp.path().to_path_buf(), Duration::from_secs(60)).unwrap();
            metadata.request(workspace);
            assert!(metadata.try_flush().is_err());
            assert!(metadata.try_flush().is_err());
            std::fs::remove_dir(metadata_path).unwrap();
            metadata.try_flush().unwrap();
            assert_eq!(
                load_workspace(temp.path()).unwrap().unwrap().pages().len(),
                1
            );
        }
    }
}
