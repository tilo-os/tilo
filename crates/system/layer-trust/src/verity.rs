//! fs-verity: the kernel's per-file Merkle tree.
//!
//! Once fs-verity is enabled on a file its contents can never change again
//! (every write fails), every page read from disk is checked against the
//! tree, and `FS_IOC_MEASURE_VERITY` returns the file's digest in a few
//! microseconds without reading it. A launch that finds a per-user runtime
//! file's measured digest equal to the publisher-signed one therefore knows
//! the bytes are the signed bytes without hashing them.
//!
//! Tilo uses one set of parameters: SHA-256, 4 KiB blocks, no salt. The digest
//! is defined by the kernel (`Documentation/filesystems/fsverity.rst`):
//! SHA-256 of the 256-byte `fsverity_descriptor` whose root hash is the top of
//! a Merkle tree over the zero-padded 4 KiB data blocks.

use sha2::{Digest, Sha256};

/// Merkle tree block size and data block size.
pub const BLOCK_SIZE: usize = 4096;
/// `log2(BLOCK_SIZE)`, as the descriptor records it.
const LOG_BLOCK_SIZE: u8 = 12;
/// Bytes of one SHA-256 hash.
const HASH_SIZE: usize = 32;
/// Hashes per tree block.
const HASHES_PER_BLOCK: usize = BLOCK_SIZE / HASH_SIZE;
/// `FS_VERITY_HASH_ALG_SHA256`.
const HASH_ALG_SHA256: u8 = 1;
/// Zero padding source: a partial block is hashed as if zero-filled.
static ZEROS: [u8; BLOCK_SIZE] = [0; BLOCK_SIZE];

/// Streaming fs-verity digest of a file's bytes, exactly as
/// `FS_IOC_MEASURE_VERITY` reports it once verity is enabled with Tilo's
/// parameters.
#[derive(Debug, Default)]
pub struct VerityHasher {
    block: Vec<u8>,
    leaves: Vec<[u8; HASH_SIZE]>,
    size: u64,
}

impl VerityHasher {
    /// An empty hasher.
    #[must_use]
    pub fn new() -> Self {
        Self {
            block: Vec::with_capacity(BLOCK_SIZE),
            leaves: Vec::new(),
            size: 0,
        }
    }

    /// Feed the next bytes of the file.
    pub fn update(&mut self, mut data: &[u8]) {
        self.size = self.size.saturating_add(data.len() as u64);
        while !data.is_empty() {
            let room = BLOCK_SIZE.saturating_sub(self.block.len());
            let (head, rest) = data
                .split_at_checked(room.min(data.len()))
                .unwrap_or((data, &[]));
            self.block.extend_from_slice(head);
            if self.block.len() == BLOCK_SIZE {
                self.leaves.push(sha256(&[&self.block]));
                self.block.clear();
            }
            data = rest;
        }
    }

    /// The file's fs-verity digest.
    #[must_use]
    pub fn finalize(mut self) -> [u8; HASH_SIZE] {
        if !self.block.is_empty() {
            self.block.resize(BLOCK_SIZE, 0);
            self.leaves.push(sha256(&[&self.block]));
        }
        let root = merkle_root(self.leaves);
        // struct fsverity_descriptor: version, hash_algorithm, log_blocksize,
        // salt_size, 4 reserved bytes, le64 data_size, root_hash[64],
        // salt[32], 144 reserved bytes.
        let header = [1, HASH_ALG_SHA256, LOG_BLOCK_SIZE, 0, 0, 0, 0, 0];
        let size = self.size.to_le_bytes();
        let root_padding = ZEROS.get(..64 - HASH_SIZE).unwrap_or_default();
        let tail = ZEROS.get(..32 + 144).unwrap_or_default();
        sha256(&[&header, &size, &root, root_padding, tail])
    }
}

/// The fs-verity digest of `bytes`.
#[must_use]
pub fn digest(bytes: &[u8]) -> [u8; HASH_SIZE] {
    let mut hasher = VerityHasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

/// Top of the tree over `level` (the data block hashes): hashes are packed
/// into zero-padded blocks and hashed until one remains. An empty file's root
/// is all zeros; a one-block file's root is that block's hash.
fn merkle_root(mut level: Vec<[u8; HASH_SIZE]>) -> [u8; HASH_SIZE] {
    while level.len() > 1 {
        level = level
            .chunks(HASHES_PER_BLOCK)
            .map(|hashes| {
                let used = hashes.len().saturating_mul(HASH_SIZE);
                let padding = ZEROS
                    .get(..BLOCK_SIZE.saturating_sub(used))
                    .unwrap_or_default();
                let mut hasher = Sha256::new();
                for hash in hashes {
                    hasher.update(hash);
                }
                hasher.update(padding);
                finish(hasher)
            })
            .collect();
    }
    level.first().copied().unwrap_or_default()
}

fn sha256(parts: &[&[u8]]) -> [u8; HASH_SIZE] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    finish(hasher)
}

fn finish(hasher: Sha256) -> [u8; HASH_SIZE] {
    let mut out = [0; HASH_SIZE];
    out.copy_from_slice(&hasher.finalize());
    out
}

/// Enable fs-verity with Tilo's parameters on the file at `path`. A file
/// that already has it is left as it is (its measured digest says whether it
/// is usable).
///
/// # Errors
///
/// The `FS_IOC_ENABLE_VERITY` error: `Unsupported` where the filesystem or
/// kernel has no fs-verity, or on a platform other than Linux; the file must
/// not be open for writing anywhere.
pub fn enable(path: &std::path::Path) -> std::io::Result<()> {
    sys::enable(path)
}

/// The SHA-256 fs-verity digest of the open file `file`, or `None` when the
/// file has no fs-verity, has it with another hash algorithm, or the
/// filesystem or platform does not support it. Reads no file data.
#[must_use]
pub fn measure(file: &std::fs::File) -> Option<[u8; HASH_SIZE]> {
    sys::measure(file)
}

#[cfg(target_os = "linux")]
mod sys {
    use super::{HASH_ALG_SHA256, HASH_SIZE, LOG_BLOCK_SIZE};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    /// `_IOW('f', 133, struct fsverity_enable_arg)`.
    const FS_IOC_ENABLE_VERITY: libc::Ioctl = 0x4080_6685;
    /// `_IOWR('f', 134, struct fsverity_digest)`.
    const FS_IOC_MEASURE_VERITY: libc::Ioctl = 0xC004_6686;

    /// `struct fsverity_enable_arg` (128 bytes).
    #[repr(C)]
    struct EnableArg {
        version: u32,
        hash_algorithm: u32,
        block_size: u32,
        salt_size: u32,
        salt_ptr: u64,
        sig_size: u32,
        reserved1: u32,
        sig_ptr: u64,
        reserved2: [u64; 11],
    }

    /// `struct fsverity_digest` followed by room for the largest digest.
    #[repr(C)]
    struct MeasureArg {
        digest_algorithm: u16,
        digest_size: u16,
        digest: [u8; 64],
    }

    pub(super) fn enable(path: &std::path::Path) -> std::io::Result<()> {
        // The kernel requires a read-only descriptor and no writer anywhere.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let arg = EnableArg {
            version: 1,
            hash_algorithm: u32::from(HASH_ALG_SHA256),
            block_size: 1 << LOG_BLOCK_SIZE,
            salt_size: 0,
            salt_ptr: 0,
            sig_size: 0,
            reserved1: 0,
            sig_ptr: 0,
            reserved2: [0; 11],
        };
        // SAFETY: the descriptor is live for the call and `arg` is a fully
        // initialised `struct fsverity_enable_arg` the kernel only reads.
        let result = unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                FS_IOC_ENABLE_VERITY,
                std::ptr::from_ref(&arg),
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EEXIST) => Ok(()),
            Some(libc::EOPNOTSUPP | libc::ENOTTY) => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                format!("this filesystem has no fs-verity: {error}"),
            )),
            _ => Err(error),
        }
    }

    pub(super) fn measure(file: &std::fs::File) -> Option<[u8; HASH_SIZE]> {
        let mut arg = MeasureArg {
            digest_algorithm: 0,
            digest_size: 64,
            digest: [0; 64],
        };
        // SAFETY: the descriptor is live for the call; `arg` holds a
        // `struct fsverity_digest` whose `digest_size` announces the 64 bytes
        // of `digest` after it, which is all the kernel writes.
        let result = unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                FS_IOC_MEASURE_VERITY,
                std::ptr::from_mut(&mut arg),
            )
        };
        if result != 0
            || arg.digest_algorithm != u16::from(HASH_ALG_SHA256)
            || usize::from(arg.digest_size) != HASH_SIZE
        {
            return None;
        }
        let mut digest = [0; HASH_SIZE];
        digest.copy_from_slice(arg.digest.get(..HASH_SIZE)?);
        Some(digest)
    }
}

#[cfg(not(target_os = "linux"))]
mod sys {
    use super::HASH_SIZE;

    pub(super) fn enable(_path: &std::path::Path) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "fs-verity exists only on Linux",
        ))
    }

    pub(super) fn measure(_file: &std::fs::File) -> Option<[u8; HASH_SIZE]> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Digests of `0x5a` repeated `size` times. "hello world" is the vector
    /// composefs-rs checks against the kernel's `FS_IOC_MEASURE_VERITY`, the
    /// empty file is fsverity-utils' known digest, and the multi-level sizes
    /// were cross-checked against a port of composefs-rs' kernel-verified
    /// streaming implementation: one block, a partial second block, a full
    /// first tree block, a second tree level, and a ragged second level.
    const VECTORS: &[(usize, &str)] = &[
        (
            0,
            "3d248ca542a24fc62d1c43b916eae5016878e2533c88238480b26128a1f1af95",
        ),
        (
            1,
            "ed1ca3d522e4b885a33e9cfc3803b4aab52c718d99b449eef0caeb3170ff641d",
        ),
        (
            4096,
            "dae733f4cb5a63767cb1041a2bd774ca90c91dfd490554e5edab17886d474600",
        ),
        (
            4097,
            "d2619d9db529bfcc1b0179d7f22a3275ef40456580ebd246fb1c0faa70cc1f6c",
        ),
        (
            128 * 4096,
            "fa6dd50c6117479a8e0691104fd19c93263b8fc12c56a7c581d36a1fe2990845",
        ),
        (
            128 * 4096 + 1,
            "44a3ceba1c1a2896d0002c3b1e15f332291e8ef3c4760f14531061f17d6dc423",
        ),
        (
            129 * 4096 + 7,
            "1a736d18ed84b3a82b94445c3e197f3e05bebffc04837dbb1d82aec69c941c43",
        ),
    ];

    #[test]
    fn digests_match_the_kernel_definition() {
        assert_eq!(
            hex::encode(digest(b"hello world")),
            "1e2eaa4202d750a41174ee454970b92c1bc2f925b1e35076d8c7d5f56362ba64"
        );
        for (size, expected) in VECTORS {
            assert_eq!(
                hex::encode(digest(&vec![0x5a; *size])),
                *expected,
                "size {size}"
            );
        }
    }

    #[test]
    fn streaming_in_any_chunking_gives_the_same_digest() {
        let bytes: Vec<u8> = (0..=250u8).cycle().take(130 * 4096 + 123).collect();
        let whole = digest(&bytes);
        for chunk in [1, 7, 4095, 4096, 4097, 65536] {
            let mut hasher = VerityHasher::new();
            for piece in bytes.chunks(chunk) {
                hasher.update(piece);
            }
            assert_eq!(hasher.finalize(), whole, "chunk {chunk}");
        }
    }
}
