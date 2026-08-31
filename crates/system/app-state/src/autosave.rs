//! Debounced background writing for a [`DocStore`].
//!
//! Typing a character must not cost an `fsync`. [`Autosave`] takes the document's delta on
//! the UI thread — cheap, no I/O — hands it to a worker, and the worker coalesces a burst of
//! edits into a single write once the user pauses.
//!
//! Growth is bounded without paying a full rewrite per edit: the handle appends
//! `save_after(persisted_heads)` until the appended volume exceeds the compacted base, then
//! sends one full `save()` and resets — total bytes written stay linear in document size.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ChangeHash};

use crate::{DocStore, StoreError};

/// Debounce window apps get by default. Long enough to swallow a burst of keystrokes, short
/// enough that a `kill` a moment later still finds the edit on disk.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(400);

/// Below this, compaction isn't worth it — a small document rewrites in microseconds and the
/// append bookkeeping would dominate.
const MIN_COMPACT_BYTES: usize = 64 * 1024;

enum Msg {
    /// Replace the file with these bytes.
    Full(Vec<u8>),
    /// Extend the file with these change chunks.
    Append(Vec<u8>),
    /// Write anything pending now; ack when done.
    Flush(Sender<()>),
    Stop,
}

/// Handle to the background writer. Dropping it flushes and joins, so an app that keeps the
/// `Autosave` alive for the length of `main` cannot exit with an unwritten edit.
pub struct Autosave {
    /// `None` for a [`Autosave::disabled`] handle — there is no worker to talk to and every
    /// `request` is a no-op.
    tx: Option<Sender<Msg>>,
    join: Option<thread::JoinHandle<()>>,
    document: PathBuf,
    package_id: String,
    /// Heads already handed to the writer.
    sent: Vec<ChangeHash>,
    /// Bytes appended since the last full write.
    appended: usize,
    /// Size of the last full write.
    base: usize,
    /// How many times [`Autosave::request`] chose a full rewrite, and how many times it chose an
    /// append. Exposed so a test can assert on the decision itself, not infer it from file size
    /// (nearly identical either way for a small document).
    full_writes: usize,
    appends: usize,
    last_error: Arc<Mutex<Option<String>>>,
}

impl Autosave {
    /// Spawn the writer for `store`. Thread creation can fail under resource
    /// exhaustion; rather than panic on that, the handle comes back
    /// [`Autosave::disabled`] with the OS error as its reason, the same
    /// degraded-but-visible state a caller already gets when the state
    /// directory itself couldn't be opened.
    pub fn spawn(store: DocStore, debounce: Duration) -> Self {
        let (tx, rx) = mpsc::channel::<Msg>();
        let document = store.document_path().to_path_buf();
        let package_id = store.package_id().to_string();
        let last_error = Arc::new(Mutex::new(None));
        let errs = Arc::clone(&last_error);
        match thread::Builder::new()
            .name("app-state-writer".into())
            .spawn(move || writer_loop(&rx, &store, debounce, &errs))
        {
            Ok(join) => Self {
                tx: Some(tx),
                join: Some(join),
                document,
                package_id,
                sent: Vec::new(),
                appended: 0,
                base: 0,
                full_writes: 0,
                appends: 0,
                last_error,
            },
            Err(e) => {
                tracing::error!(error = %e, "could not spawn the app-state writer thread; persistence disabled");
                Self::disabled(format!("could not spawn the app-state writer thread: {e}"))
            }
        }
    }

    /// A handle that persists nothing, carrying `reason` as its permanent error. Used when the
    /// state directory cannot be opened at all — the app still runs, and `is_enabled` /
    /// `last_error` make the degraded state visible instead of silently pretending writes land.
    pub fn disabled(reason: impl Into<String>) -> Self {
        Self {
            tx: None,
            join: None,
            document: PathBuf::new(),
            package_id: String::new(),
            sent: Vec::new(),
            appended: 0,
            base: 0,
            full_writes: 0,
            appends: 0,
            last_error: Arc::new(Mutex::new(Some(reason.into()))),
        }
    }

    /// Whether this handle is backed by a real store.
    pub fn is_enabled(&self) -> bool {
        self.tx.is_some()
    }

    /// The file the writer targets.
    pub fn document_path(&self) -> &Path {
        &self.document
    }

    /// The package this writer persists for.
    pub fn package_id(&self) -> &str {
        &self.package_id
    }

    /// Adopt `heads` as already on disk. Call this right after loading, so the first `request`
    /// only ships what the user has changed since.
    pub fn seed(&mut self, doc: &mut AutoCommit) {
        if self.tx.is_none() {
            return;
        }
        self.sent = doc.get_heads();
        // Only used to decide compact-vs-append (see `request`); a length too big for
        // `usize` saturates rather than wrapping, which just biases toward compacting sooner.
        self.base = std::fs::metadata(&self.document)
            .map(|m| usize::try_from(m.len()).unwrap_or(usize::MAX))
            .unwrap_or(0);
        self.appended = 0;
    }

    /// Note that `doc` may have changed and schedule a write. Cheap and idempotent: when the
    /// document hasn't moved since the last call this compares two head vectors and returns.
    /// Safe to call every frame.
    pub fn request(&mut self, doc: &mut AutoCommit) {
        let Some(tx) = self.tx.as_ref() else {
            return;
        };
        let heads = doc.get_heads();
        if heads == self.sent {
            return;
        }
        let compact = self.base == 0 || self.appended >= self.base.max(MIN_COMPACT_BYTES);
        if compact {
            let bytes = doc.save();
            self.base = bytes.len();
            self.appended = 0;
            self.full_writes = self.full_writes.saturating_add(1);
            let _ = tx.send(Msg::Full(bytes));
        } else {
            let delta = doc.save_after(&self.sent);
            if !delta.is_empty() {
                self.appended = self.appended.saturating_add(delta.len());
                self.appends = self.appends.saturating_add(1);
                let _ = tx.send(Msg::Append(delta));
            }
        }
        self.sent = heads;
    }

    /// Write everything pending and wait for it to hit the disk. Used on exit and by tests; a
    /// plain `request` is fire-and-forget.
    pub fn flush(&self) {
        let Some(tx) = self.tx.as_ref() else {
            return;
        };
        let (ack, done) = mpsc::channel();
        if tx.send(Msg::Flush(ack)).is_ok() {
            let _ = done.recv();
        }
    }

    /// How many full document rewrites this handle has scheduled.
    pub fn full_writes(&self) -> usize {
        self.full_writes
    }

    /// How many incremental appends this handle has scheduled.
    pub fn appends(&self) -> usize {
        self.appends
    }

    /// The most recent write failure, if any. `None` means every write so far succeeded.
    /// Surfaced rather than only logged so a caller can tell "nothing to save" from "the save failed".
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|g| g.clone())
    }
}

impl Drop for Autosave {
    fn drop(&mut self) {
        self.flush();
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Msg::Stop);
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Bytes waiting for the debounce window to close.
enum Pending {
    Full(Vec<u8>),
    Append(Vec<u8>),
}

fn writer_loop(
    rx: &Receiver<Msg>,
    store: &DocStore,
    debounce: Duration,
    last_error: &Arc<Mutex<Option<String>>>,
) {
    let mut pending: Option<Pending> = None;
    let mut deadline: Option<Instant> = None;

    let commit = |pending: &mut Option<Pending>| -> Result<(), StoreError> {
        match pending.take() {
            Some(Pending::Full(bytes)) => store.write_full(&bytes),
            Some(Pending::Append(bytes)) => store.append_bytes(&bytes),
            None => Ok(()),
        }
    };
    let record = |last_error: &Arc<Mutex<Option<String>>>, r: Result<(), StoreError>| {
        if let Err(e) = r {
            tracing::error!(error = %e, "app state write failed");
            if let Ok(mut g) = last_error.lock() {
                *g = Some(format!("{e}"));
            }
        }
    };

    loop {
        let timeout = match deadline {
            Some(d) => d.saturating_duration_since(Instant::now()),
            None => Duration::from_hours(1),
        };
        match rx.recv_timeout(timeout) {
            Ok(Msg::Full(bytes)) => {
                pending = Some(Pending::Full(bytes));
                // On the extremely unlikely overflow, fall back to no deadline: the loop above
                // then waits up to an hour and commits on that timeout instead of panicking.
                deadline = Instant::now().checked_add(debounce);
            }
            Ok(Msg::Append(bytes)) => {
                match pending.take() {
                    // A full save followed by change chunks *is* the file format, so the two coalesce into one write.
                    Some(Pending::Full(mut base)) => {
                        base.extend_from_slice(&bytes);
                        pending = Some(Pending::Full(base));
                    }
                    Some(Pending::Append(mut acc)) => {
                        acc.extend_from_slice(&bytes);
                        pending = Some(Pending::Append(acc));
                    }
                    None => pending = Some(Pending::Append(bytes)),
                }
                deadline = Instant::now().checked_add(debounce);
            }
            Ok(Msg::Flush(ack)) => {
                record(last_error, commit(&mut pending));
                deadline = None;
                let _ = ack.send(());
            }
            // Stopping and losing the peer both mean "shut down after one last commit" — same body.
            Ok(Msg::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                record(last_error, commit(&mut pending));
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                record(last_error, commit(&mut pending));
                deadline = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ObjType, ROOT, ReadDoc};
    use tempfile::TempDir;

    fn scratch_home() -> TempDir {
        let tmp = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
            std::env::remove_var("QOS_PACKAGE_ID");
        }
        tmp
    }

    #[test]
    #[serial_test::serial]
    fn a_burst_of_edits_lands_on_disk_after_a_flush() {
        let _tmp = scratch_home();
        let mut store = DocStore::open_for("dev.qos.autosave").unwrap();
        let mut doc = store.new_document();
        let body = doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
        let mut saver = Autosave::spawn(store, Duration::from_millis(50));
        saver.seed(&mut doc);

        for (i, ch) in "persisted".chars().enumerate() {
            doc.splice_text(&body, i, 0, &ch.to_string()).unwrap();
            saver.request(&mut doc);
        }
        saver.flush();
        assert_eq!(saver.last_error(), None, "no write may have failed");

        let mut reopened = DocStore::open_for("dev.qos.autosave").unwrap();
        let loaded = reopened.load().unwrap().expect("document on disk");
        let body = loaded.get(&ROOT, "body").unwrap().unwrap().1;
        assert_eq!(loaded.text(&body).unwrap(), "persisted");
    }

    #[test]
    #[serial_test::serial]
    fn dropping_the_handle_flushes() {
        let _tmp = scratch_home();
        let mut store = DocStore::open_for("dev.qos.autosave-drop").unwrap();
        let mut doc = store.new_document();
        {
            let mut saver = Autosave::spawn(store, Duration::from_mins(10));
            saver.seed(&mut doc);
            doc.put(&ROOT, "k", "v").unwrap();
            saver.request(&mut doc);
            // Debounce is 10 minutes: only Drop's flush can get this out.
        }
        drop(doc);
        let mut reopened = DocStore::open_for("dev.qos.autosave-drop").unwrap();
        let loaded = reopened.load().unwrap().expect("document on disk");
        assert!(matches!(loaded.get(&ROOT, "k"), Ok(Some(_))));
    }

    /// A steady stream of edits must not rewrite the whole document each time — the O(n²) shape
    /// this store exists to avoid. Asserted on the decision the writer actually makes, since a
    /// rewrite and an append produce almost the same file size for a small document.
    #[test]
    #[serial_test::serial]
    fn steady_edits_append_rather_than_rewrite() {
        let _tmp = scratch_home();
        let mut store = DocStore::open_for("dev.qos.autosave-growth").unwrap();
        let mut doc = store.new_document();
        let body = doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
        doc.splice_text(&body, 0, 0, "seed").unwrap();
        store.save(&mut doc).unwrap();

        let path = store.document_path().to_path_buf();
        let base = std::fs::metadata(&path).unwrap().len();
        let mut saver = Autosave::spawn(store, Duration::from_millis(10));
        saver.seed(&mut doc);

        for i in 0..50 {
            doc.splice_text(&body, 4 + i, 0, "y").unwrap();
            saver.request(&mut doc);
        }
        saver.flush();

        assert_eq!(
            saver.full_writes(),
            0,
            "50 small edits on an already-saved document must not rewrite it"
        );
        assert_eq!(saver.appends(), 50, "each edit must ship exactly one delta");
        assert_eq!(saver.last_error(), None);

        let grown = std::fs::metadata(&path).unwrap().len();
        assert!(grown > base, "the edits must have reached the file");

        drop((saver, doc));
        let mut reopened = DocStore::open_for("dev.qos.autosave-growth").unwrap();
        let loaded = reopened.load().unwrap().unwrap();
        let body = loaded.get(&ROOT, "body").unwrap().unwrap().1;
        assert_eq!(
            loaded.text(&body).unwrap(),
            format!("seed{}", "y".repeat(50))
        );
    }

    /// Once the appended volume outgrows the compacted base, the writer must fold everything
    /// back into one document chunk instead of growing the append log forever.
    #[test]
    #[serial_test::serial]
    fn the_append_log_is_eventually_compacted() {
        let _tmp = scratch_home();
        let mut store = DocStore::open_for("dev.qos.autosave-compact").unwrap();
        let mut doc = store.new_document();
        let body = doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
        doc.splice_text(&body, 0, 0, "seed").unwrap();
        store.save(&mut doc).unwrap();

        let mut saver = Autosave::spawn(store, Duration::from_millis(5));
        saver.seed(&mut doc);
        // MIN_COMPACT_BYTES is 64 KiB and each single-character change chunk is about a hundred
        // bytes, comfortably past the threshold.
        for i in 0..1200 {
            doc.splice_text(&body, 4 + i, 0, "z").unwrap();
            saver.request(&mut doc);
        }
        saver.flush();
        assert!(
            saver.full_writes() >= 1,
            "the append log must be compacted once it outgrows the base \
             (appends: {}, appended bytes exceeded the {} byte floor)",
            saver.appends(),
            MIN_COMPACT_BYTES
        );
        assert_eq!(saver.last_error(), None);

        drop((saver, doc));
        let mut reopened = DocStore::open_for("dev.qos.autosave-compact").unwrap();
        let loaded = reopened.load().unwrap().unwrap();
        let body = loaded.get(&ROOT, "body").unwrap().unwrap().1;
        assert_eq!(loaded.text(&body).unwrap().chars().count(), 4 + 1200);
    }
}
