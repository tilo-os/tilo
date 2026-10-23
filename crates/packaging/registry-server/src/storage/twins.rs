//! Compressed twins of the objects this registry serves by content.
//!
//! Beside every payload object `payload-blobs/<blake3>` (a whole blob or a
//! chunk) sits `payload-blobs/<blake3>.zst`, and beside every imported
//! `artifacts/<blake3>.tapp` sits `artifacts/<blake3>.tapp.zst`: the same
//! bytes compressed once, when the object is stored, so clients download a
//! third of them (`tilo_app_package::compressed`). A twin is written only
//! from bytes already checked against their name, and checked again the
//! first time each stored twin file is served (see `checked`): it must
//! decompress, within the length of the object it stands for, to bytes that
//! hash to that name.
//!
//! A twin is an optimisation, never the only copy. A missing one is served as
//! 404 and the client takes the raw object; `compress-payloads` writes the
//! twins of objects stored before twins existed.

use std::path::{Path, PathBuf};

use tilo_app_package::BlobHash;
use tilo_app_package::compressed::{self, SUFFIX};

use super::checked::CheckedFiles;
use super::{Storage, StorageError, atomic_write, is_hex_digest, list_digests};

/// What [`Storage::compress_stored_objects`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct TwinReport {
    pub written: usize,
    pub present: usize,
    /// Bytes of the objects twinned by this run.
    pub raw_bytes: u64,
    /// Bytes of the twins this run wrote.
    pub twin_bytes: u64,
}

impl Storage {
    fn payload_twin_path(&self, hash: &BlobHash) -> PathBuf {
        self.payload_dir()
            .join(format!("{}{SUFFIX}", hash.to_hex()))
    }

    fn artifact_twin_path(&self, blake3_hex: &str) -> Option<PathBuf> {
        self.artifact_path(blake3_hex)
            .map(|path| path.with_extension(format!("tapp{SUFFIX}")))
    }

    /// Write the twin of the stored payload object `hash` from its verified
    /// `bytes`, unless it has one. Returns the twin's length when written.
    pub(super) fn write_payload_twin(
        &self,
        hash: &BlobHash,
        bytes: &[u8],
    ) -> Result<Option<u64>, StorageError> {
        write_twin(&self.payload_twin_path(hash), bytes)
    }

    /// Write the twin of the stored payload object `hash` from the object
    /// on disk, checked against its name first, unless it has one.
    ///
    /// # Errors
    /// Returns an error if the object is missing, does not match its name,
    /// or the twin cannot be written.
    pub fn ensure_payload_twin(&self, hash: &BlobHash) -> Result<Option<u64>, StorageError> {
        if self.payload_twin_path(hash).exists() {
            return Ok(None);
        }
        let bytes = self
            .load_payload_blob(&hash.to_hex())?
            .ok_or_else(|| StorageError::Io(format!("payload object {hash} is missing")))?;
        self.write_payload_twin(hash, &bytes)
    }

    /// The twin of the payload object `blake3_hex`, checked to decompress to
    /// that object, or `None` when either is missing.
    ///
    /// # Errors
    /// Returns an error when a stored twin cannot be read or does not stand
    /// for its object.
    pub fn load_payload_twin(&self, blake3_hex: &str) -> Result<Option<Vec<u8>>, StorageError> {
        if !is_hex_digest(blake3_hex) {
            return Ok(None);
        }
        let Some(hash) = BlobHash::from_hex(&blake3_hex.to_ascii_lowercase()) else {
            return Ok(None);
        };
        load_twin(
            &self.checked,
            &self.payload_twin_path(&hash),
            &self.payload_dir().join(hash.to_hex()),
            &hash,
        )
    }

    /// Write the twin of the imported artifact `blake3_hex` from its verified
    /// `bytes`, unless it has one.
    pub(super) fn write_artifact_twin(
        &self,
        blake3_hex: &str,
        bytes: &[u8],
    ) -> Result<Option<u64>, StorageError> {
        let path = self
            .artifact_twin_path(blake3_hex)
            .ok_or_else(|| StorageError::Encode("invalid artifact digest".into()))?;
        write_twin(&path, bytes)
    }

    /// The twin of the imported artifact `blake3_hex`, checked to decompress
    /// to that artifact, or `None` when either is missing.
    ///
    /// # Errors
    /// Returns an error when a stored twin cannot be read or does not stand
    /// for its artifact.
    pub fn load_artifact_twin(&self, blake3_hex: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let (Some(twin), Some(raw), Some(hash)) = (
            self.artifact_twin_path(blake3_hex),
            self.artifact_path(blake3_hex),
            BlobHash::from_hex(&blake3_hex.to_ascii_lowercase()),
        ) else {
            return Ok(None);
        };
        load_twin(&self.checked, &twin, &raw, &hash)
    }

    /// Write the missing twin of every stored payload object and imported
    /// artifact: the one-time backfill for objects stored before twins
    /// existed. Each object is checked against its name before it is
    /// compressed. Safe to rerun and to run beside `serve`.
    ///
    /// # Errors
    /// Returns an error if an object cannot be read, does not match its
    /// name, or a twin cannot be written.
    pub fn compress_stored_objects(&self) -> Result<TwinReport, StorageError> {
        let mut report = TwinReport::default();
        let mut count = |raw: u64, written: Option<u64>| match written {
            Some(twin) => {
                report.written = report.written.saturating_add(1);
                report.raw_bytes = report.raw_bytes.saturating_add(raw);
                report.twin_bytes = report.twin_bytes.saturating_add(twin);
            }
            None => report.present = report.present.saturating_add(1),
        };
        let payload_dir = self.payload_dir();
        if payload_dir.is_dir() {
            for blob in list_digests(&payload_dir, "")? {
                let Some(hash) = BlobHash::from_hex(&blob) else {
                    continue;
                };
                let raw = file_len(&payload_dir.join(&blob));
                count(raw, self.ensure_payload_twin(&hash)?);
            }
        }
        let artifact_dir = self.root.join("artifacts");
        if artifact_dir.is_dir() {
            for digest in list_digests(&artifact_dir, ".tapp")? {
                let written = match self.artifact_twin_path(&digest) {
                    Some(path) if path.exists() => None,
                    _ => {
                        let bytes = self.load_artifact(&digest)?.ok_or_else(|| {
                            StorageError::Io(format!("artifact {digest} is missing"))
                        })?;
                        self.write_artifact_twin(&digest, &bytes)?
                    }
                };
                count(
                    file_len(&artifact_dir.join(format!("{digest}.tapp"))),
                    written,
                );
            }
        }
        Ok(report)
    }
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

pub(super) fn write_twin(path: &Path, bytes: &[u8]) -> Result<Option<u64>, StorageError> {
    if path.exists() {
        return Ok(None);
    }
    let twin = compressed::compress(bytes)
        .map_err(|error| StorageError::Encode(format!("compress {}: {error}", path.display())))?;
    atomic_write(path, &twin)?;
    Ok(Some(twin.len() as u64))
}

/// Read the twin at `twin` and check, the first time this file is served,
/// that it decompresses within the length of the object at `raw` to bytes
/// hashing to `hash`.
fn load_twin(
    checked: &CheckedFiles,
    twin: &Path,
    raw: &Path,
    hash: &BlobHash,
) -> Result<Option<Vec<u8>>, StorageError> {
    let Ok(raw_len) = std::fs::metadata(raw).map(|metadata| metadata.len()) else {
        // A twin is served only beside the object it stands for.
        return Ok(None);
    };
    checked.read(
        twin,
        "twin",
        |_| Ok(compressed::compressed_bound(raw_len)),
        |bytes| {
            let mut hasher = blake3::Hasher::new();
            let len =
                compressed::decompress_bounded(bytes, raw_len, &mut hasher).map_err(|error| {
                    StorageError::Parse(format!("twin does not decompress: {error}"))
                })?;
            if len != raw_len || hasher.finalize().as_bytes() != hash.as_bytes() {
                return Err(StorageError::Parse(
                    "twin does not decompress to its object".into(),
                ));
            }
            Ok(())
        },
    )
}
