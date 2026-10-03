//! Dropbox `content_hash`: the SHA-256 of the concatenated SHA-256 digests of
//! every 4 MiB block of a file, as lowercase hex.
//!
//! <https://www.dropbox.com/developers/reference/content-hash>

use std::io::{self, Read};

use sha2::{Digest, Sha256};

/// Bytes per block.
pub const BLOCK: usize = 4 * 1024 * 1024;

/// A streaming `content_hash`.
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

    /// The final hash, lowercase hex.
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

/// `content_hash` of an in-memory buffer.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Dropbox's published test vector is `milky-way-nasa.jpg` (9 711 423
    /// bytes, three blocks) with content hash
    /// `485291fa0ee50c016982abbfa943957bcd231aae0492ccbaa22c58e3997b35e0`.
    /// These are the SHA-256 digests of its three 4 MiB blocks; combining
    /// them must give the published hash.
    const MILKY_WAY_BLOCKS: [&str; 3] = [
        "2a846fa617c3361fc117e1c5c1e1838c336b6a5cef982c1a2d9bdf68f2f1992a",
        "c68469027410ea393eba6551b9fa1e26db775f00eae70a0c3c129a0011a39cf9",
        "7376192de020925ce6c5ef5a8a0405e931b0a9a8c75517aacd9ca24a8a56818b",
    ];
    const MILKY_WAY_HASH: &str = "485291fa0ee50c016982abbfa943957bcd231aae0492ccbaa22c58e3997b35e0";
    const MILKY_WAY_LEN: usize = 9_711_423;

    #[test]
    fn the_published_vector_combines_its_block_digests() {
        let hasher = ContentHasher::from_blocks(&MILKY_WAY_BLOCKS).unwrap();
        assert_eq!(hasher.finish(), MILKY_WAY_HASH);
    }

    /// The whole published vector, when the image is at hand
    /// (`TILO_DROPBOX_HASH_VECTOR=/path/to/milky-way-nasa.jpg`; download it
    /// from <https://www.dropbox.com/static/images/developers/milky-way-nasa.jpg>).
    #[test]
    fn the_published_vector_hashes_from_its_bytes() {
        let Ok(path) = std::env::var("TILO_DROPBOX_HASH_VECTOR") else {
            return;
        };
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(bytes.len(), MILKY_WAY_LEN);
        assert_eq!(content_hash(&bytes), MILKY_WAY_HASH);
        for (block, expected) in bytes.chunks(BLOCK).zip(MILKY_WAY_BLOCKS) {
            assert_eq!(hex::encode(Sha256::digest(block)), expected);
        }
    }

    #[test]
    fn an_empty_file_hashes_no_blocks() {
        assert_eq!(
            content_hash(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    fn reference(data: &[u8]) -> String {
        let mut outer = Sha256::new();
        for block in data.chunks(BLOCK) {
            outer.update(Sha256::digest(block));
        }
        hex::encode(outer.finalize())
    }

    #[test]
    fn streaming_splits_blocks_exactly_at_four_mebibytes() {
        let data: Vec<u8> = (0..=BLOCK * 2)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        for len in [1, BLOCK - 1, BLOCK, BLOCK + 1, 2 * BLOCK, 2 * BLOCK + 1] {
            let slice = &data[..len];
            let mut hasher = ContentHasher::new();
            for piece in slice.chunks(1_000_003) {
                hasher.update(piece);
            }
            assert_eq!(hasher.finish(), reference(slice), "{len} bytes");
        }
    }

    #[test]
    fn a_boundary_state_continues_the_same_hash() {
        let data: Vec<u8> = (0..BLOCK + 10)
            .map(|i| u8::try_from(i % 7).unwrap())
            .collect();
        let mut first = ContentHasher::new();
        first.update(&data[..BLOCK]);
        let blocks = first.boundary_blocks().unwrap();
        let mut resumed = ContentHasher::from_blocks(&blocks).unwrap();
        resumed.update(&data[BLOCK..]);
        assert_eq!(resumed.finish(), content_hash(&data));
        let mut unaligned = ContentHasher::new();
        unaligned.update(b"x");
        assert!(unaligned.boundary_blocks().is_none());
    }

    #[test]
    fn verifying_reader_rejects_corruption() {
        let data = b"hello dropbox".to_vec();
        let mut ok = Verifying::new(&data[..], content_hash(&data));
        let mut out = Vec::new();
        ok.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
        let mut bad = Verifying::new(&b"hello dropbax"[..], content_hash(&data));
        let error = bad.read_to_end(&mut Vec::new()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
