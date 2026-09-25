//! Placeholder bytes from the daemon's content plane.
//!
//! A placeholder's content id is the record's content hash (the manifest
//! the record plane published). Filling a range resolves the manifest,
//! takes the chunks that overlap the range from the local content store and
//! fetches the missing ones from peers, several at a time.

use std::io;
use std::sync::Arc;

use iroh_blobs::Hash;

use crate::hydration::Hydrator;

/// Chunks fetched concurrently for one fill.
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

impl fs_native::hydrate::Fetcher for ContentFetcher {
    fn fetch(&self, content: &[u8; 32], offset: u64, len: u64) -> io::Result<Vec<u8>> {
        let hydrator = Arc::clone(&self.content);
        let hash = Hash::from(*content);
        let parallel = self.parallel;
        self.runtime
            .block_on(async move { hydrator.read_parallel(hash, offset, len, parallel).await })
            .map_err(|error| io::Error::new(io::ErrorKind::TimedOut, error.to_string()))
    }
}
