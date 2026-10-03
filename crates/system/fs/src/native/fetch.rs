//! Placeholder bytes from the daemon's content plane.
//!
//! A placeholder's content id is the record's content hash (the manifest
//! the record plane published). Filling a range makes it resident in the
//! content store first: file-backed content lands in the store's area and
//! is then copied into the placeholder at the same offsets, which on the
//! store volume clones extents instead of writing the bytes twice. Small
//! content is read and written.

use std::fs::File;
use std::io;
use std::ops::Range;
use std::os::fd::BorrowedFd;
use std::os::unix::fs::FileExt as _;
use std::sync::Arc;

use iroh_blobs::Hash;

use crate::hydration::{Hydrator, Priority};

/// Chunks fetched concurrently for one fill of small content.
pub const DEFAULT_PARALLEL_CHUNKS: usize = 16;

pub struct ContentFetcher {
    content: Arc<Hydrator>,
    runtime: tokio::runtime::Handle,
    parallel: usize,
}

impl ContentFetcher {
    pub fn new(content: Arc<Hydrator>, runtime: tokio::runtime::Handle, parallel: usize) -> Self {
        Self {
            content,
            runtime,
            parallel: parallel.max(1),
        }
    }
}

fn unavailable(error: &anyhow::Error) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, error.to_string())
}

impl fs_native::hydrate::Fetcher for ContentFetcher {
    fn fetch(&self, content: &[u8; 32], offset: u64, len: u64) -> io::Result<Vec<u8>> {
        let hydrator = Arc::clone(&self.content);
        let hash = Hash::from(*content);
        let parallel = self.parallel;
        self.runtime
            .block_on(async move { hydrator.read_parallel(hash, offset, len, parallel).await })
            .map_err(|error| unavailable(&error))
    }

    fn fill(
        &self,
        content: &[u8; 32],
        range: Range<u64>,
        dest: BorrowedFd<'_>,
        ahead: bool,
    ) -> io::Result<()> {
        let hydrator = Arc::clone(&self.content);
        let hash = Hash::from(*content);
        let priority = if ahead {
            Priority::ReadAhead
        } else {
            Priority::Foreground
        };
        let (start, len) = (range.start, range.end.saturating_sub(range.start));
        let manifest = self
            .runtime
            .block_on(async move { hydrator.hydrate_range(hash, start, len, priority).await })
            .map_err(|error| unavailable(&error))?;
        // A duplicate shares the event descriptor's open file, which raises
        // no events of its own.
        let dest = File::from(dest.try_clone_to_owned()?);
        if crate::content::is_file_backed(&manifest) {
            return self.content.store.files().copy_to(content, &range, &dest);
        }
        let bytes = self.fetch(content, start, len)?;
        if bytes.len() as u64 != len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "content is shorter than the range",
            ));
        }
        dest.write_all_at(&bytes, start)
    }
}
