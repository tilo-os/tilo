//! Putting the crash-recovery copy on disk without making somebody wait.
//!
//! The recovery copy is the only durable form unsaved work has: the document
//! is a file the user has not written yet, and the record set the app runs on
//! is in memory. So it has to be written, and often.
//!
//! What it must not do is happen between a keypress and the letter appearing.
//! `std::fs::write` costs whatever the storage under it costs, and here that
//! can be a workspace on a peer-to-peer filesystem rather than a local disk.
//! Every keystroke arriving during such a write queues up and lands at once,
//! which is what a burst of letters is.
//!
//! So the editor keeps the part that needs the document — encoding it — and
//! hands over the part that needs the disk. The writer holds one copy at a
//! time: a newer one replaces an older one that has not been written yet,
//! because only the most recent is worth anything.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};

enum Msg {
    /// Put these bytes at this path, replacing whatever is there.
    Write(PathBuf, Vec<u8>),
    /// Reply once everything sent before this has landed.
    Flush(Sender<()>),
}

/// A handle to the writer thread. Dropping it lets the thread finish what it
/// is holding and exit.
pub struct RecoveryWriter {
    tx: Sender<Msg>,
    /// The most recent write failure, so a caller can tell "nothing to save"
    /// from "the save did not land". Surfaced rather than only logged.
    last_error: Arc<Mutex<Option<String>>>,
}

impl RecoveryWriter {
    /// Start the writer.
    pub fn spawn() -> Self {
        let (tx, rx) = channel::<Msg>();
        let last_error = Arc::new(Mutex::new(None));
        let errs = Arc::clone(&last_error);
        // If the thread cannot be spawned the handle still exists and every
        // write is a no-op that records why — the editor keeps running, and
        // the reason is visible rather than inferred from a missing file.
        if let Err(e) = std::thread::Builder::new()
            .name("wp-recovery-writer".into())
            .spawn(move || writer_loop(rx, errs))
        {
            *last_error.lock().expect("recovery error slot") =
                Some(format!("could not start the recovery writer: {e}"));
        }
        Self { tx, last_error }
    }

    /// Hand `bytes` over to be put at `path`. Returns immediately.
    pub fn write(&self, path: PathBuf, bytes: Vec<u8>) {
        // A closed channel means the thread is gone. Failing the keystroke
        // over that would be worse than the missing copy, and `last_error`
        // already says what happened.
        let _ = self.tx.send(Msg::Write(path, bytes));
    }

    /// Wait until everything handed over so far is on disk.
    ///
    /// For callers that need the file to exist before they look at it — a
    /// test, or teardown. The editing path never calls this.
    pub fn flush(&self) {
        let (ack, done) = channel();
        if self.tx.send(Msg::Flush(ack)).is_ok() {
            let _ = done.recv();
        }
    }

    /// The most recent write failure, if any. `None` means every write so far
    /// landed.
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|g| g.clone())
    }
}

fn writer_loop(rx: Receiver<Msg>, errs: Arc<Mutex<Option<String>>>) {
    // The copy handed over but not yet written. Held rather than written
    // immediately so a burst of edits collapses into one write.
    let mut pending: Option<(PathBuf, Vec<u8>)> = None;
    loop {
        // Take whatever is already queued without blocking, so a burst is
        // seen as a burst. When nothing is queued, block for the next
        // message — after writing anything held.
        let msg = match rx.try_recv() {
            Ok(m) => m,
            Err(TryRecvError::Empty) => {
                if let Some(copy) = pending.take() {
                    write_one(&copy, &errs);
                }
                match rx.recv() {
                    Ok(m) => m,
                    Err(_) => break,
                }
            }
            Err(TryRecvError::Disconnected) => break,
        };
        match msg {
            Msg::Write(path, bytes) => pending = Some((path, bytes)),
            Msg::Flush(ack) => {
                if let Some(copy) = pending.take() {
                    write_one(&copy, &errs);
                }
                let _ = ack.send(());
            }
        }
    }
    if let Some(copy) = pending.take() {
        write_one(&copy, &errs);
    }
}

fn write_one((path, bytes): &(PathBuf, Vec<u8>), errs: &Arc<Mutex<Option<String>>>) {
    let record = |e: String| {
        if let Ok(mut slot) = errs.lock() {
            *slot = Some(e);
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            record(format!("{}: {e}", parent.display()));
            return;
        }
    }
    match std::fs::write(path, bytes) {
        Ok(()) => {
            if let Ok(mut slot) = errs.lock() {
                *slot = None;
            }
        }
        Err(e) => record(format!("{}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What was handed over reaches the disk, and `flush` is what makes that
    /// observable — a test that looked without flushing would be racing the
    /// worker and would pass or fail by timing.
    #[test]
    fn a_handed_over_copy_lands_on_disk() {
        let dir = std::env::temp_dir().join("wp-recovery-writer-lands");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("copy.md");

        let w = RecoveryWriter::spawn();
        w.write(path.clone(), b"# Recovered\n".to_vec());
        w.flush();

        assert_eq!(
            std::fs::read(&path).expect("the copy was written").as_slice(),
            b"# Recovered\n",
            "the writer created the directory and wrote the bytes",
        );
        assert_eq!(w.last_error(), None, "a successful write records no error");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A burst of copies costs one write of the newest, not one write each.
    /// That is the property that keeps a fast typist from queueing disk work
    /// faster than the disk retires it.
    #[test]
    fn a_burst_collapses_to_the_newest_copy() {
        let dir = std::env::temp_dir().join("wp-recovery-writer-burst");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("copy.md");

        let w = RecoveryWriter::spawn();
        for i in 0..50 {
            w.write(path.clone(), format!("edit {i}\n").into_bytes());
        }
        w.flush();

        assert_eq!(
            String::from_utf8(std::fs::read(&path).expect("the copy was written")).unwrap(),
            "edit 49\n",
            "the file holds the most recent copy handed over",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A write that cannot land says so. Silence would look identical to
    /// success, and the thing being protected is unsaved work.
    #[test]
    fn a_failed_write_is_reported() {
        // A path whose parent is an existing *file* cannot be created.
        let dir = std::env::temp_dir().join("wp-recovery-writer-fail");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let blocker = dir.join("not-a-dir");
        std::fs::write(&blocker, b"x").expect("the blocking file");

        let w = RecoveryWriter::spawn();
        w.write(blocker.join("copy.md"), b"lost".to_vec());
        w.flush();

        assert!(
            w.last_error().is_some(),
            "a write that could not land reported nothing",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
