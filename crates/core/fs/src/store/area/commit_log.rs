//! The area's commits not yet on disk, kept in a log beside the index so a killed process loses
//! none of them.
//!
//! A commit that does not wait for the disk (`Durability::None`) lives only in the process: the
//! index database names it in a header it keeps in memory until the next commit that does wait. A
//! process killed before that comes back without it. So each such commit's writes are added to
//! `area.log` with one `write` before the commit is made, and a commit that waits for the disk
//! empties the log: the database then holds everything the log held. Opening the area applies
//! what the log holds, in order, and empties it.
//!
//! The log is never synced on its own: what it adds over the database is that a process killed at
//! any instant keeps what it committed. Against a power loss, the barrier that syncs the database
//! ([`super::FileArea::sync_db`]) is what makes writes durable, as before.
//!
//! A frame is the length of its writes, a checksum of them and the writes: a frame cut off by a
//! kill in the middle of its `write`, or not whole on disk, ends the log there.

use std::borrow::Cow;
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use super::AreaKey;

/// What one commit of the area writes, as the log keeps it and the database applies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Write<'a> {
    /// Small blobs stored inline under their hashes.
    Inline(Vec<(AreaKey, Cow<'a, [u8]>)>),
    RemoveInline(AreaKey),
    Outboard(AreaKey, Cow<'a, [u8]>),
    Reference(Cow<'a, str>, AreaKey),
    /// An entry's chunk index: each chunk's hash, offset and length.
    Chunks(AreaKey, Cow<'a, [([u8; 32], u64, u32)]>),
    Origin(AreaKey, Cow<'a, str>),
    /// Entries' stored states, each already encoded.
    Entries(Vec<(AreaKey, Cow<'a, [u8]>)>),
    /// An entry gone: its stored state, outboard, origin and chunk index.
    Unpersist(AreaKey),
    /// Writes committed as one, as an entry and its chunk index are.
    Batch(Vec<Write<'a>>),
}

const INLINE: u8 = 1;
const REMOVE_INLINE: u8 = 2;
const OUTBOARD: u8 = 3;
const REFERENCE: u8 = 4;
const CHUNKS: u8 = 5;
const ORIGIN: u8 = 6;
const ENTRIES: u8 = 7;
const UNPERSIST: u8 = 8;
const BATCH: u8 = 9;

/// A frame's length and checksum before its writes.
const HEADER: usize = 8;

impl Write<'_> {
    fn encode(&self, out: &mut Vec<u8>) {
        fn bytes(out: &mut Vec<u8>, bytes: &[u8]) {
            out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            out.extend_from_slice(bytes);
        }
        fn blobs(out: &mut Vec<u8>, tag: u8, blobs: &[(AreaKey, Cow<'_, [u8]>)]) {
            out.push(tag);
            out.extend_from_slice(&(blobs.len() as u64).to_le_bytes());
            for (key, value) in blobs {
                out.extend_from_slice(key);
                bytes(out, value);
            }
        }
        match self {
            Self::Inline(list) => blobs(out, INLINE, list),
            Self::Entries(list) => blobs(out, ENTRIES, list),
            Self::RemoveInline(key) => {
                out.push(REMOVE_INLINE);
                out.extend_from_slice(key);
            }
            Self::Outboard(key, data) => {
                out.push(OUTBOARD);
                out.extend_from_slice(key);
                bytes(out, data);
            }
            Self::Reference(tag, key) => {
                out.push(REFERENCE);
                bytes(out, tag.as_bytes());
                out.extend_from_slice(key);
            }
            Self::Chunks(key, chunks) => {
                out.push(CHUNKS);
                out.extend_from_slice(key);
                out.extend_from_slice(&(chunks.len() as u64).to_le_bytes());
                for (hash, offset, len) in chunks.iter() {
                    out.extend_from_slice(hash);
                    out.extend_from_slice(&offset.to_le_bytes());
                    out.extend_from_slice(&len.to_le_bytes());
                }
            }
            Self::Origin(key, origin) => {
                out.push(ORIGIN);
                out.extend_from_slice(key);
                bytes(out, origin.as_bytes());
            }
            Self::Unpersist(key) => {
                out.push(UNPERSIST);
                out.extend_from_slice(key);
            }
            Self::Batch(writes) => {
                out.push(BATCH);
                out.extend_from_slice(&(writes.len() as u64).to_le_bytes());
                for write in writes {
                    write.encode(out);
                }
            }
        }
    }

    fn decode(frame: &mut Reader<'_>) -> Option<Write<'static>> {
        Some(match frame.u8()? {
            INLINE => Write::Inline(frame.blobs()?),
            ENTRIES => Write::Entries(frame.blobs()?),
            REMOVE_INLINE => Write::RemoveInline(frame.key()?),
            OUTBOARD => Write::Outboard(frame.key()?, Cow::Owned(frame.bytes()?.to_vec())),
            REFERENCE => {
                let tag = String::from_utf8(frame.bytes()?.to_vec()).ok()?;
                Write::Reference(Cow::Owned(tag), frame.key()?)
            }
            CHUNKS => {
                let key = frame.key()?;
                let count = frame.count(44)?;
                let mut chunks = Vec::with_capacity(count);
                for _ in 0..count {
                    chunks.push((frame.key()?, frame.u64()?, frame.u32()?));
                }
                Write::Chunks(key, Cow::Owned(chunks))
            }
            ORIGIN => {
                let key = frame.key()?;
                let origin = String::from_utf8(frame.bytes()?.to_vec()).ok()?;
                Write::Origin(key, Cow::Owned(origin))
            }
            UNPERSIST => Write::Unpersist(frame.key()?),
            BATCH => {
                let count = frame.count(1)?;
                let mut writes = Vec::with_capacity(count);
                for _ in 0..count {
                    writes.push(Write::decode(frame)?);
                }
                Write::Batch(writes)
            }
            _ => return None,
        })
    }
}

/// Reads a frame's writes, `None` once they do not read.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn key(&mut self) -> Option<AreaKey> {
        self.take(32)?.try_into().ok()
    }

    /// A count of items at least `each` bytes long, never more than the frame holds.
    fn count(&mut self, each: usize) -> Option<usize> {
        let count = usize::try_from(self.u64()?).ok()?;
        (count.checked_mul(each)? <= self.0.len()).then_some(count)
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.u64()?).ok()?;
        self.take(len)
    }

    fn blobs(&mut self) -> Option<Vec<(AreaKey, Cow<'static, [u8]>)>> {
        let count = self.count(40)?;
        let mut blobs = Vec::with_capacity(count);
        for _ in 0..count {
            blobs.push((self.key()?, Cow::Owned(self.bytes()?.to_vec())));
        }
        Some(blobs)
    }
}

fn checksum(writes: &[u8]) -> [u8; 4] {
    let hash = blake3::hash(writes);
    let (sum, _) = hash
        .as_bytes()
        .split_first_chunk::<4>()
        .unwrap_or((&[0; 4], &[]));
    *sum
}

/// The log of an area kept under one directory.
pub(super) struct CommitLog {
    path: PathBuf,
    file: File,
    /// Bytes the log holds.
    len: u64,
}

impl CommitLog {
    pub(super) fn open(dir: &Path) -> io::Result<Self> {
        let path = dir.join("area.log");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;
        let len = file.metadata()?.len();
        Ok(Self { path, file, len })
    }

    /// The writes the log holds, in the order they were committed, up to the first frame that
    /// is not whole.
    pub(super) fn read(&self) -> io::Result<Vec<Write<'static>>> {
        let mut bytes = Vec::new();
        File::open(&self.path)?.read_to_end(&mut bytes)?;
        let mut writes = Vec::new();
        let mut rest = bytes.as_slice();
        while let Some((header, after)) = rest.split_first_chunk::<HEADER>() {
            let (len, sum) = header.split_at(4);
            let len = u32::from_le_bytes(len.try_into().unwrap_or_default()) as usize;
            let Some((frame, after)) = after.split_at_checked(len) else {
                break;
            };
            if checksum(frame) != sum {
                break;
            }
            let mut reader = Reader(frame);
            let Some(write) = Write::decode(&mut reader) else {
                break;
            };
            writes.push(write);
            rest = after;
        }
        Ok(writes)
    }

    /// Adds `write` to the log, in one `write` of the file.
    pub(super) fn append(&mut self, write: &Write<'_>) -> io::Result<()> {
        let mut writes = Vec::new();
        write.encode(&mut writes);
        let len = u32::try_from(writes.len())
            .map_err(|_| io::Error::other("a commit too large for the log"))?;
        let mut frame = Vec::with_capacity(writes.len().saturating_add(HEADER));
        frame.extend_from_slice(&len.to_le_bytes());
        frame.extend_from_slice(&checksum(&writes));
        frame.extend_from_slice(&writes);
        self.file.write_all(&frame)?;
        self.len = self.len.saturating_add(frame.len() as u64);
        Ok(())
    }

    /// Bytes the log holds.
    pub(super) const fn len(&self) -> u64 {
        self.len
    }

    /// Empties the log: the database holds all of it on disk now.
    pub(super) fn clear(&mut self) -> io::Result<()> {
        if self.len > 0 {
            self.file.set_len(0)?;
            self.len = 0;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writes() -> Vec<Write<'static>> {
        vec![
            Write::Inline(vec![
                ([1; 32], Cow::Owned(b"a chunk".to_vec())),
                ([2; 32], Cow::Owned(vec![7; 70_000])),
            ]),
            Write::RemoveInline([3; 32]),
            Write::Outboard([4; 32], Cow::Owned(vec![1, 2, 3])),
            Write::Reference(Cow::Owned("media/one".into()), [5; 32]),
            Write::Chunks(
                [6; 32],
                Cow::Owned(vec![([7; 32], 0, 10), ([8; 32], 10, 20)]),
            ),
            Write::Origin([9; 32], Cow::Owned("https://example.org/a".into())),
            Write::Entries(vec![([10; 32], Cow::Owned(vec![0, 1]))]),
            Write::Unpersist([11; 32]),
        ]
    }

    #[test]
    fn every_write_reads_back_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = CommitLog::open(dir.path()).unwrap();
        for write in writes() {
            log.append(&write).unwrap();
        }
        assert_eq!(
            CommitLog::open(dir.path()).unwrap().read().unwrap(),
            writes()
        );
    }

    #[test]
    fn a_frame_cut_off_ends_the_log_and_loses_only_itself() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = CommitLog::open(dir.path()).unwrap();
        for write in writes() {
            log.append(&write).unwrap();
        }
        let whole = std::fs::read(dir.path().join("area.log")).unwrap();
        let cut = whole.len() - 3;
        std::fs::write(dir.path().join("area.log"), &whole[..cut]).unwrap();
        let read = CommitLog::open(dir.path()).unwrap().read().unwrap();
        assert_eq!(read, writes()[..writes().len() - 1]);
        // A frame whose bytes are not the ones written (a page never written) ends it too.
        let mut damaged = whole;
        let last = damaged.len() - 1;
        damaged[last] ^= 0xff;
        std::fs::write(dir.path().join("area.log"), &damaged).unwrap();
        let read = CommitLog::open(dir.path()).unwrap().read().unwrap();
        assert_eq!(read.len(), writes().len() - 1);
    }

    #[test]
    fn a_cleared_log_holds_nothing_and_takes_new_writes() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = CommitLog::open(dir.path()).unwrap();
        log.append(&writes()[0]).unwrap();
        log.clear().unwrap();
        assert_eq!(log.len(), 0);
        log.append(&writes()[1]).unwrap();
        assert_eq!(log.read().unwrap(), vec![writes()[1].clone()]);
    }
}
