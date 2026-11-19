//! What an app wrote, kept in its keeper's directory where a desktop that comes later still reads
//! it.
//!
//! The app's streams are the keeper's to read: the launcher passes the keeper's own standard
//! streams on to the session, and the session reading them is gone while the desktop restarts.
//! The keeper therefore writes each stream both on (as before) and into a bounded file here, and
//! the desktop that follows reads on from where the desktop before it stopped: no line an app
//! wrote before, during or after an in-place restart is lost with the one that heard it, and none
//! is read twice by the one that follows. The position read up to is kept beside the file, so a
//! desktop that comes to a launch a second time picks up where the last one left it.
//!
//! The file is bounded because a runtime directory is memory: an app that prints forever
//! overwrites its oldest output instead of filling it. Its first eight bytes are how many bytes
//! were written in all, little endian; the body that follows holds the newest [`BYTES`] of them.

use std::fs::File;
use std::io;
use std::io::{Seek as _, SeekFrom};
use std::os::unix::fs::FileExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// How much of one stream a keeper keeps: a start-up's warnings and the lines just before a
/// crash, and nothing an app could grow without limit.
pub const BYTES: usize = 64 * 1024;

/// The body's first byte: the eight before it are the count it follows.
const HEADER: usize = 8;

/// How long a reader that has read everything waits before it looks again.
const POLL: Duration = Duration::from_millis(50);

/// Where the desktop that read a stream stopped, beside the stream's file: the position a
/// following desktop reads on from instead of replaying what the one before it already logged.
#[must_use]
pub fn cursor(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".read");
    path.with_file_name(name)
}

/// Where the keeper keeps what its app wrote on `stream` (`"stdout"` or `"stderr"`).
#[must_use]
pub fn path(dir: &Path, stream: &str) -> PathBuf {
    dir.join(format!("output.{stream}"))
}

/// How much of its own log a keeper keeps: a keeper says a few lines per launch, and one that
/// somehow said more still never fills the runtime directory its app's streams live in.
pub const LOG_BYTES: u64 = 64 * 1024;

/// Where the keeper writes what it says of itself: beside what its app wrote, never in the app's
/// streams, which carry the app's own words to the session.
#[must_use]
pub fn log_path(dir: &Path) -> PathBuf {
    dir.join("keeper.log")
}

/// The keeper's own log, as its tracing is written: [`LOG_BYTES`] long at most, and shared by
/// every thread that logs.
#[derive(Clone, Debug)]
pub struct Log(Arc<Mutex<LogFile>>);

#[derive(Debug)]
struct LogFile {
    file: File,
    /// How many bytes were written since the log last started over.
    written: u64,
}

impl Log {
    /// Start the log at `path`, empty: a keeper's directory is one launch's, so the launch before
    /// it said what it had to say.
    ///
    /// # Errors
    /// The file could not be made in the keeper's directory.
    pub fn create(path: &Path) -> io::Result<Self> {
        let file = File::options()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        Ok(Self(Arc::new(Mutex::new(LogFile { file, written: 0 }))))
    }
}

impl io::Write for Log {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut log = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        log.file.flush()
    }
}

impl io::Write for LogFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if self.written.saturating_add(len) > LOG_BYTES {
            // The newest lines are what a diagnosis reads: a full log starts over.
            self.file.seek(SeekFrom::Start(0))?;
            self.file.set_len(0)?;
            self.written = 0;
        }
        self.file.write_all(bytes)?;
        self.written = self.written.saturating_add(len);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// One stream's bounded file, as the keeper writes it.
#[derive(Debug)]
pub struct Ring {
    file: File,
    /// How many bytes were written into the body in all.
    written: usize,
}

impl Ring {
    /// Make the bounded file at `path`, empty, ahead of the app's first write.
    ///
    /// # Errors
    /// The file could not be made in the keeper's directory.
    pub fn create(path: &Path) -> io::Result<Self> {
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        file.set_len(offset(HEADER.saturating_add(BYTES)))?;
        Ok(Self { file, written: 0 })
    }

    /// Append what the app wrote, keeping the newest [`BYTES`] of it.
    ///
    /// # Errors
    /// The file could not be written.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        // A single write longer than the file keeps nothing before its own tail.
        let bytes = bytes
            .get(bytes.len().saturating_sub(BYTES)..)
            .unwrap_or_default();
        let start = self.written.checked_rem(BYTES).unwrap_or(0);
        let first = BYTES.saturating_sub(start).min(bytes.len());
        let (head, _) = bytes.split_at(first);
        self.file
            .write_all_at(head, offset(HEADER.saturating_add(start)))?;
        if let Some(tail) = bytes.get(first..) {
            self.file.write_all_at(tail, offset(HEADER))?;
        }
        self.written = self.written.saturating_add(bytes.len());
        self.file
            .write_all_at(&offset(self.written).to_le_bytes(), 0)?;
        Ok(())
    }
}

/// One stream's bounded file, as a desktop reads it: what the keeper kept before this desktop,
/// then what the app writes from now on, in order, until the desktop says it stopped. Where the
/// desktop before this one stopped is kept beside the file, and this one reads on from there.
#[derive(Debug)]
pub struct Follow {
    file: File,
    /// The next body byte to read, counted from the body's own beginning.
    at: usize,
    /// Where the position is kept, so the next desktop reads on from it.
    cursor: PathBuf,
    /// Whether this read moves that position: a whole read of the file for a report leaves it as
    /// the desktop before left it.
    keeps: bool,
    stopping: Arc<AtomicBool>,
}

impl Follow {
    /// Read the bounded file at `path` until `stopping` is set, from where the desktop before this
    /// one stopped.
    ///
    /// # Errors
    /// The file is not there, or cannot be read.
    pub fn open(path: &Path, stopping: Arc<AtomicBool>) -> io::Result<Self> {
        let at = std::fs::read(cursor(path))
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0);
        Self::from(path, at, true, stopping)
    }

    /// Read all the bounded file at `path` retains, whatever desktops read before: what an end
    /// report is made of, which names the last lines the app wrote whether or not they reached a
    /// log.
    ///
    /// # Errors
    /// The file is not there, or cannot be read.
    pub fn whole(path: &Path, stopping: Arc<AtomicBool>) -> io::Result<Self> {
        Self::from(path, 0, false, stopping)
    }

    fn from(path: &Path, at: usize, keeps: bool, stopping: Arc<AtomicBool>) -> io::Result<Self> {
        Ok(Self {
            file: File::open(path)?,
            at,
            cursor: cursor(path),
            keeps,
            stopping,
        })
    }

    /// Keep `at` beside the file: what this desktop read, and the next one does not read again.
    fn mark(&self) {
        if !self.keeps {
            return;
        }
        // The position is a hint, never what this desktop's log stands on: a write that fails leaves the
        // desktop reading from where it last said, which reads a line twice rather than losing it.
        let _ = std::fs::write(&self.cursor, self.at.to_string());
    }
}

impl std::io::Read for Follow {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut header = [0_u8; HEADER];
            if self.file.read_at(&mut header, 0)? < HEADER {
                // The keeper has not written its first header yet.
                if self.stopping.load(Ordering::Acquire) {
                    return Ok(0);
                }
                std::thread::sleep(POLL);
                continue;
            }
            let written = usize::try_from(u64::from_le_bytes(header)).unwrap_or(usize::MAX);
            // What the app overwrote is gone: what remains starts at the oldest byte kept.
            self.at = self.at.max(written.saturating_sub(BYTES));
            if self.at < written {
                let want = written.saturating_sub(self.at).min(out.len());
                let start = self.at.checked_rem(BYTES).unwrap_or(0);
                let first = BYTES.saturating_sub(start).min(want);
                let (head, _) = out.split_at_mut(first);
                self.file
                    .read_exact_at(head, offset(HEADER.saturating_add(start)))?;
                if let Some(tail) = out.get_mut(first..want) {
                    self.file.read_exact_at(tail, offset(HEADER))?;
                }
                self.at = self.at.saturating_add(want);
                self.mark();
                return Ok(want);
            }
            if self.stopping.load(Ordering::Acquire) {
                return Ok(0);
            }
            std::thread::sleep(POLL);
        }
    }
}

/// `at` as a file offset.
fn offset(at: usize) -> u64 {
    u64::try_from(at).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    /// The bytes `follow` reads after `chunks` were written and the app stopped.
    fn followed(
        ring: &mut Ring,
        follow: &mut Follow,
        stopping: &AtomicBool,
        chunks: &[&[u8]],
    ) -> Vec<u8> {
        for chunk in chunks {
            ring.write(chunk).expect("the keeper writes");
        }
        stopping.store(true, Ordering::Release);
        let mut kept = Vec::new();
        follow
            .read_to_end(&mut kept)
            .expect("the follower reads them");
        kept
    }

    /// What one desktop read is not read again by the one after it: each reads on from where the
    /// last stopped, and what the app wrote while no desktop read is read once.
    #[test]
    fn a_following_desktop_reads_on_from_where_the_last_one_stopped() {
        let directory = tempfile::tempdir().expect("a keeper's directory");
        let path = path(directory.path(), "stderr");
        let mut ring = Ring::create(&path).expect("the bounded file");
        ring.write(b"before\n").expect("the keeper writes");
        let read = |ring: &mut Ring, chunks: &[&[u8]]| {
            let stopping = Arc::new(AtomicBool::new(false));
            let mut follow = Follow::open(&path, Arc::clone(&stopping)).expect("a follower");
            followed(ring, &mut follow, &stopping, chunks)
        };
        assert_eq!(
            read(&mut ring, &[]),
            b"before\n",
            "the first desktop reads what the app wrote before it"
        );
        ring.write(b"during\n").expect("the keeper writes");
        assert_eq!(
            read(&mut ring, &[]),
            b"during\n",
            "the desktop that follows reads on, not again from the start"
        );
        ring.write(b"after\n").expect("the keeper writes");
        assert_eq!(
            read(&mut ring, &[]),
            b"after\n",
            "a third desktop reads only what was written since the second"
        );
        // An end report reads all the file still keeps, and leaves the position as the desktop
        // before it left it: the next desktop reads only what comes after.
        let stopping = Arc::new(AtomicBool::new(false));
        let mut all = Follow::whole(&path, Arc::clone(&stopping)).expect("a whole read");
        assert_eq!(
            followed(&mut ring, &mut all, &stopping, &[]),
            b"before\nduring\nafter\n",
            "a report names every line the keeper still holds"
        );
        ring.write(b"later\n").expect("the keeper writes");
        assert_eq!(
            read(&mut ring, &[]),
            b"later\n",
            "a whole read for a report does not move the position a desktop reads from"
        );
    }

    /// What an app wrote before, between and after a desktop's looks reaches the following one in
    /// the order it wrote it, and its file never grows past what the keeper keeps.
    #[test]
    fn a_stream_is_kept_in_order_and_bounded() {
        let directory = tempfile::tempdir().expect("a keeper's directory");
        let path = path(directory.path(), "stderr");
        let mut ring = Ring::create(&path).expect("the bounded file");
        let stopping = Arc::new(AtomicBool::new(false));
        let mut follow = Follow::open(&path, Arc::clone(&stopping)).expect("a follower");
        assert_eq!(
            followed(
                &mut ring,
                &mut follow,
                &stopping,
                &[b"before\n", b"during\n", b"after\n"]
            ),
            b"before\nduring\nafter\n",
            "what the app wrote, in every gap, in order"
        );
        assert_eq!(
            std::fs::metadata(&path).expect("the file").len(),
            offset(HEADER.saturating_add(BYTES)),
            "the file is as large as a keeper's stream ever is"
        );

        // Far more than the file holds: the newest bytes are what remain, and the follower reads
        // on rather than failing.
        let flood = vec![b'x'; BYTES * 2];
        ring.write(&flood).expect("a flood");
        let mut kept = Vec::new();
        follow
            .read_to_end(&mut kept)
            .expect("the newest bytes remain");
        assert!(
            kept.len() <= BYTES,
            "a follower reads no more than the file keeps: {}",
            kept.len()
        );
        assert!(kept.ends_with(b"x"), "the newest bytes are what remained");
        assert_eq!(
            std::fs::metadata(&path).expect("the file").len(),
            offset(HEADER.saturating_add(BYTES)),
            "a flood does not grow the file"
        );
    }
}
