//! What an app wrote, kept in its keeper's directory where a desktop that comes later still reads
//! it.
//!
//! The app's streams are the keeper's to read: the launcher passes the keeper's own standard
//! streams on to the session, and the session reading them is gone while the desktop restarts.
//! The keeper therefore writes each stream both on (as before) and into a bounded file here, and
//! the desktop that follows reads the file from its start: no line an app wrote before, during or
//! after an in-place restart is lost with the one that heard it.
//!
//! The file is bounded because a runtime directory is memory: an app that prints forever
//! overwrites its oldest output instead of filling it. Its first eight bytes are how many bytes
//! were written in all, little endian; the body that follows holds the newest [`BYTES`] of them.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How much of one stream a keeper keeps: a start-up's warnings and the lines just before a
/// crash, and nothing an app could grow without limit.
pub const BYTES: usize = 64 * 1024;

/// The body's first byte: the eight before it are the count it follows.
const HEADER: usize = 8;

/// How long a reader that has read everything waits before it looks again.
const POLL: Duration = Duration::from_millis(50);

/// Where the keeper keeps what its app wrote on `stream` (`"stdout"` or `"stderr"`).
#[must_use]
pub fn path(dir: &Path, stream: &str) -> PathBuf {
    dir.join(format!("output.{stream}"))
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
/// then what the app writes from now on, in order, until the desktop says it stopped.
#[derive(Debug)]
pub struct Follow {
    file: File,
    /// The next body byte to read, counted from the body's own beginning.
    at: usize,
    stopping: Arc<AtomicBool>,
}

impl Follow {
    /// Read the bounded file at `path` until `stopping` is set.
    ///
    /// # Errors
    /// The file is not there, or cannot be read.
    pub fn open(path: &Path, stopping: Arc<AtomicBool>) -> io::Result<Self> {
        Ok(Self {
            file: File::open(path)?,
            at: 0,
            stopping,
        })
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
