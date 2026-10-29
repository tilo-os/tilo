//! An environment's content store on this device: where what a `write` or
//! `append` names by reference is kept.
//!
//! An action carries at most [`MAX_TEXT_BYTES`] inside the record. Anything
//! larger — `cat big.log > copy`, a `tee`, a `sort -o` — is streamed into
//! this store as the command writes it ([`EnvironmentContent::keep`]), and
//! the action names it by its hash (`tilo.content:<hash>`). The device that
//! carries the action out reads it back from here. It is Tilo's content
//! store ([`tilo_content`]) in a directory of the environment's own, beside
//! its `/var` and outside every guest, with no bound on an item's size: a
//! write is never refused for being large.
//!
//! Content is kept while a task of the record still names it: a redo of a
//! write the record still lists writes the same bytes again. What no task
//! names any more is collected ([`EnvironmentContent::collect`]) — never
//! while a session is still streaming content its action has not named yet:
//! every [`Kept`] holds the store's keeping lock shared until its action is
//! recorded, and collection only runs when it can take that lock alone.
//!
//! [`MAX_TEXT_BYTES`]: tilo_environment::files::MAX_TEXT_BYTES

use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use tilo_content::{ContentLimits, ContentStore, DirBackend};
use tilo_environment::Environment;
use tilo_environment::files::FileAction;
use ush_tilo::{Keeping, Kept};

/// Held shared by every content on its way to being named by an action;
/// collection takes it alone.
const KEEPING_LOCK: &str = ".keeping.lock";

/// One environment's content store on this device.
#[derive(Debug, Clone)]
pub struct EnvironmentContent {
    dir: PathBuf,
}

impl EnvironmentContent {
    #[must_use]
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The store itself. No item and no total is too large: what bounds it
    /// is the disk, and what the record still names.
    #[must_use]
    pub fn store(&self) -> ContentStore {
        ContentStore::new(DirBackend::with_limits(
            self.dir.clone(),
            ContentLimits {
                max_item_bytes: usize::MAX,
                max_total_bytes: usize::MAX,
                ..ContentLimits::default()
            },
        ))
    }

    fn keeping_lock(&self, mode: advisory_lock::LockMode) -> io::Result<advisory_lock::FileLock> {
        std::fs::create_dir_all(&self.dir)?;
        advisory_lock::FileLock::open(&self.dir.join(KEEPING_LOCK), mode)
    }

    /// Start keeping content: what is written is streamed into the store
    /// on a thread of its own, never held whole in memory.
    ///
    /// # Errors
    ///
    /// The store's directory or its lock could not be made.
    pub fn keep(&self) -> io::Result<Box<dyn Keeping>> {
        let hold = self.keeping_lock(advisory_lock::LockMode::Shared)?;
        let (mut reader, writer) = std::io::pipe()?;
        let store = self.store();
        let putting = std::thread::Builder::new()
            .name("ush-content".into())
            .spawn(move || {
                store
                    .put_reader(&mut reader, "application/octet-stream")
                    .map_err(|e| io::Error::other(e.to_string()))
            })?;
        Ok(Box::new(Streaming {
            writer: Some(writer),
            putting: Some(putting),
            hold: Some(hold),
        }))
    }

    /// Remove what no task of `environment` names any more, unless a
    /// session is still keeping content its action has not named yet; then
    /// it is left for the next time.
    ///
    /// # Errors
    ///
    /// The store could not be read or changed.
    pub fn collect(&self, environment: &Environment) -> io::Result<()> {
        if !self.dir.is_dir() {
            return Ok(());
        }
        let Ok(_alone) = advisory_lock::FileLock::try_open(
            &self.dir.join(KEEPING_LOCK),
            advisory_lock::LockMode::Exclusive,
        ) else {
            return Ok(());
        };
        let live = named(environment);
        self.store()
            .collect_unreferenced(&live)
            .map(drop)
            .map_err(|e| io::Error::other(e.to_string()))
    }
}

/// Every content hash a task of `environment` names.
#[must_use]
pub fn named(environment: &Environment) -> BTreeSet<String> {
    environment
        .files
        .entries()
        .filter_map(|(_, entry)| match &entry.value.action {
            FileAction::Write(op) => op.content.clone(),
            FileAction::Append(op) => op.content.clone(),
            _ => None,
        })
        .collect()
}

/// Content being streamed into the store.
struct Streaming {
    writer: Option<std::io::PipeWriter>,
    putting: Option<std::thread::JoinHandle<io::Result<tilo_content::Stored>>>,
    hold: Option<advisory_lock::FileLock>,
}

impl Streaming {
    /// Why the store stopped taking content, once its thread has ended.
    fn stopped(&mut self) -> io::Error {
        drop(self.writer.take());
        match self.putting.take().map(std::thread::JoinHandle::join) {
            Some(Ok(Err(error))) => error,
            _ => io::Error::other("the content store stopped taking content"),
        }
    }
}

impl Write for Streaming {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Some(writer) = self.writer.as_mut() else {
            return Err(io::Error::other("the content was already finished"));
        };
        match writer.write_all(buf) {
            Ok(()) => Ok(buf.len()),
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Err(self.stopped()),
            Err(error) => Err(error),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Keeping for Streaming {
    fn finish(mut self: Box<Self>) -> io::Result<Kept> {
        drop(self.writer.take());
        let stored = self
            .putting
            .take()
            .ok_or_else(|| io::Error::other("the content was already finished"))?
            .join()
            .map_err(|_| io::Error::other("the content store's thread stopped"))??;
        Ok(Kept {
            hash: stored.hash,
            hold: self.hold.take().map(|hold| Box::new(hold) as Box<dyn Send>),
        })
    }
}

impl Drop for Streaming {
    fn drop(&mut self) {
        drop(self.writer.take());
        if let Some(putting) = self.putting.take() {
            let _ = putting.join();
        }
    }
}
