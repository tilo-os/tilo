//! Dropbox `content_hash`: the SHA-256 of the concatenated SHA-256 digests of
//! every 4 MiB block of a file, as lowercase hex.
//!
//! <https://www.dropbox.com/developers/reference/content-hash>

use std::io::{self, Read};

use sha2::{Digest, Sha256};

pub const BLOCK: usize = 4 * 1024 * 1024;

#[derive(Clone, Default)]
pub struct ContentHasher {
    blocks: Vec<[u8; 32]>,
    current: Sha256,
    in_block: usize,
}

impl ContentHasher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Continue a hash whose completed blocks are `blocks` (hex digests),
    /// positioned on a block boundary.
    pub fn from_blocks<S: AsRef<str>>(blocks: &[S]) -> Option<Self> {
        let blocks = blocks
            .iter()
            .map(|hex_digest| {
                let mut digest = [0_u8; 32];
                hex::decode_to_slice(hex_digest.as_ref(), &mut digest).ok()?;
                Some(digest)
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            blocks,
            current: Sha256::new(),
            in_block: 0,
        })
    }

    pub fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            let room = BLOCK.saturating_sub(self.in_block);
            let (head, rest) = data.split_at(room.min(data.len()));
            self.current.update(head);
            self.in_block = self.in_block.saturating_add(head.len());
            data = rest;
            if self.in_block == BLOCK {
                self.close_block();
            }
        }
    }

    fn close_block(&mut self) {
        let digest = std::mem::take(&mut self.current).finalize();
        let mut block = [0_u8; 32];
        block.copy_from_slice(&digest);
        self.blocks.push(block);
        self.in_block = 0;
    }

    /// The completed block digests as hex, when the hash sits on a block
    /// boundary (so it can be persisted and continued).
    pub fn boundary_blocks(&self) -> Option<Vec<String>> {
        (self.in_block == 0).then(|| self.blocks.iter().map(hex::encode).collect())
    }

    pub fn finish(mut self) -> String {
        if self.in_block > 0 {
            self.close_block();
        }
        let mut outer = Sha256::new();
        for block in &self.blocks {
            outer.update(block);
        }
        hex::encode(outer.finalize())
    }
}

pub fn content_hash(data: &[u8]) -> String {
    let mut hasher = ContentHasher::new();
    hasher.update(data);
    hasher.finish()
}

/// A reader that hashes what passes through and fails at the end of the
/// stream when the bytes do not match `expected`, so a corrupted download is
/// an error rather than a silently wrong file.
pub struct Verifying<R> {
    inner: R,
    hasher: Option<ContentHasher>,
    expected: String,
}

impl<R: Read> Verifying<R> {
    pub fn new(inner: R, expected: String) -> Self {
        Self {
            inner,
            hasher: Some(ContentHasher::new()),
            expected,
        }
    }
}

impl<R: Read> Read for Verifying<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        if read == 0 {
            if let Some(hasher) = self.hasher.take()
                && !hasher.finish().eq_ignore_ascii_case(&self.expected)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "downloaded bytes do not match the content hash",
                ));
            }
            return Ok(0);
        }
        if let (Some(hasher), Some(bytes)) = (self.hasher.as_mut(), buf.get(..read)) {
            hasher.update(bytes);
        }
        Ok(read)
    }
}
