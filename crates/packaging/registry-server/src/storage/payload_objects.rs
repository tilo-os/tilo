//! Reviewed payload blobs kept as chunks.
//!
//! A payload blob larger than one object is never reassembled here: its
//! verified chunks sit in `payload-blobs/` beside whole blobs, each under its
//! own BLAKE3, and `payload-blobs/<blob>.chunks` lists them. The list is
//! written only after every chunk and the joined blob verified, so it never
//! names bytes this registry did not check. While a chunked blob is being
//! fetched, `payload-blobs/<blob>.chunks-pending` keeps its chunks reachable
//! for pruning. Downloads stream into `.incoming-*` files in the same
//! directory and are renamed into place once verified, so nothing large is
//! held in memory or staged on other disks.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tilo_app_package::BlobHash;
use tilo_app_package::chunks::{ChunkList, ChunkRef, ChunkedReader, MAX_CHUNK_LIST_BYTES};

use super::{Storage, StorageError, atomic_write, delete_if_exists, sync_dir};

/// Suffix of a verified chunk list.
pub(super) const CHUNKS_SUFFIX: &str = ".chunks";
/// Suffix of the chunk list of a blob still being fetched.
pub(super) const PENDING_CHUNKS_SUFFIX: &str = ".chunks-pending";
/// Suffix of a payload object's compressed twin.
pub(super) const TWIN_SUFFIX: &str = tilo_app_package::compressed::SUFFIX;
/// Prefix of a download not yet verified.
pub(super) const INCOMING_PREFIX: &str = ".incoming-";

static INCOMING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A payload object being downloaded into the registry's blob directory.
/// Dropped without [`Self::commit`], it removes itself.
pub struct IncomingObject {
    file: Option<File>,
    path: PathBuf,
    dir: PathBuf,
}

impl Write for IncomingObject {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("incoming object already committed"))?
            .write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().map_or(Ok(()), Write::flush)
    }
}

impl IncomingObject {
    /// Durably publish the verified bytes as the object named `hash`, and
    /// write its compressed twin (see `twins`).
    ///
    /// # Errors
    /// Returns an error if the file cannot be synced or renamed, or its twin
    /// cannot be written.
    pub fn commit(mut self, hash: &BlobHash) -> Result<(), StorageError> {
        let file = self
            .file
            .take()
            .ok_or_else(|| StorageError::Io("incoming object already committed".into()))?;
        file.sync_all()
            .map_err(|error| StorageError::Io(format!("sync payload object: {error}")))?;
        drop(file);
        let object = self.dir.join(hash.to_hex());
        std::fs::rename(&self.path, &object)
            .map_err(|error| StorageError::Io(format!("publish payload object: {error}")))?;
        sync_dir(&self.dir);
        let twin = self.dir.join(format!("{}{TWIN_SUFFIX}", hash.to_hex()));
        if !twin.exists() {
            // The caller checked these bytes against `hash` before committing.
            let bytes = std::fs::read(&object)
                .map_err(|error| StorageError::Io(format!("read payload object: {error}")))?;
            super::twins::write_twin(&twin, &bytes)?;
        }
        Ok(())
    }
}

impl Drop for IncomingObject {
    fn drop(&mut self) {
        if self.file.take().is_some() {
            let _removed = std::fs::remove_file(&self.path);
        }
    }
}

impl Storage {
    pub(super) fn payload_dir(&self) -> PathBuf {
        self.root.join("payload-blobs")
    }

    fn chunk_list_path(&self, hash: &BlobHash) -> PathBuf {
        self.payload_dir()
            .join(format!("{}{CHUNKS_SUFFIX}", hash.to_hex()))
    }

    fn pending_chunk_list_path(&self, hash: &BlobHash) -> PathBuf {
        self.payload_dir()
            .join(format!("{}{PENDING_CHUNKS_SUFFIX}", hash.to_hex()))
    }

    /// Start downloading a payload object; see [`IncomingObject`].
    ///
    /// # Errors
    /// Returns an error if the temporary file cannot be created.
    pub fn incoming_payload_object(&self) -> Result<IncomingObject, StorageError> {
        let dir = self.payload_dir();
        let sequence = INCOMING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(
            "{INCOMING_PREFIX}{}-{sequence}",
            std::process::id()
        ));
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| {
                StorageError::Io(format!("create incoming payload object: {error}"))
            })?;
        Ok(IncomingObject {
            file: Some(file),
            path,
            dir,
        })
    }

    /// The stored object (whole blob or chunk) named `hash`, opened for
    /// reading, or `None`.
    ///
    /// # Errors
    /// Returns an error if the file exists but cannot be opened.
    pub fn open_payload_object(&self, hash: &BlobHash) -> Result<Option<File>, StorageError> {
        open_if_exists(&self.payload_dir().join(hash.to_hex()))
    }

    /// Record the chunk list of a blob about to be fetched, so pruning keeps
    /// its chunks while they arrive. Nothing serves this list.
    ///
    /// # Errors
    /// Returns an error if the list cannot be written.
    pub fn begin_chunked_payload(&self, list: &ChunkList) -> Result<(), StorageError> {
        atomic_write(&self.pending_chunk_list_path(&list.blob), &list.encode())
    }

    /// Publish the chunk list of a blob whose every chunk is stored and whose
    /// joined bytes verified.
    ///
    /// # Errors
    /// Returns an error if a chunk is missing or the list cannot be written.
    pub fn finish_chunked_payload(&self, list: &ChunkList) -> Result<(), StorageError> {
        for chunk in list.distinct_chunks() {
            if self.open_payload_object(&chunk)?.is_none() {
                return Err(StorageError::Io(format!(
                    "chunk {chunk} of payload blob {} is missing",
                    list.blob
                )));
            }
        }
        atomic_write(&self.chunk_list_path(&list.blob), &list.encode())?;
        delete_if_exists(&self.pending_chunk_list_path(&list.blob))?;
        Ok(())
    }

    /// The verified chunk list of the chunked blob `blake3_hex`, or `None`
    /// when this registry keeps no such list.
    ///
    /// # Errors
    /// Returns an error when the stored list cannot be read or does not
    /// describe its blob.
    pub fn load_chunk_list(&self, blake3_hex: &str) -> Result<Option<ChunkList>, StorageError> {
        let Some(hash) = parse_hash(blake3_hex) else {
            return Ok(None);
        };
        self.chunk_list(&hash)
    }

    fn chunk_list(&self, hash: &BlobHash) -> Result<Option<ChunkList>, StorageError> {
        let Some(file) = open_if_exists(&self.chunk_list_path(hash))? else {
            return Ok(None);
        };
        read_list(file, hash).map(Some)
    }

    /// The length of the stored payload blob `hash`: a whole blob's size, or
    /// a chunked blob's listed length when every chunk is present. `None`
    /// when this registry does not hold it completely.
    ///
    /// # Errors
    /// Returns an error when stored state cannot be read.
    pub fn payload_blob_len(&self, hash: &BlobHash) -> Result<Option<u64>, StorageError> {
        if let Some(file) = self.open_payload_object(hash)? {
            let len = file
                .metadata()
                .map_err(|error| StorageError::Io(format!("stat payload blob: {error}")))?
                .len();
            return Ok(Some(len));
        }
        let Some(list) = self.chunk_list(hash)? else {
            return Ok(None);
        };
        for chunk in list.distinct_chunks() {
            if self.open_payload_object(&chunk)?.is_none() {
                return Ok(None);
            }
        }
        Ok(Some(list.len))
    }

    /// The stored payload blob `hash` as a stream: whole, or its chunks
    /// joined and each checked as it is read.
    pub(super) fn open_payload_stream(
        &self,
        hash: &BlobHash,
    ) -> io::Result<Option<(u64, Box<dyn Read + '_>)>> {
        if let Some(file) = self.open_payload_object(hash).map_err(io::Error::other)? {
            let len = file.metadata()?.len();
            return Ok(Some((len, Box::new(io::BufReader::new(file)))));
        }
        let Some(list) = self.chunk_list(hash).map_err(io::Error::other)? else {
            return Ok(None);
        };
        let len = list.len;
        let reader = ChunkedReader::new(list, move |chunk: &ChunkRef| {
            let file = self
                .open_payload_object(&chunk.hash)
                .map_err(io::Error::other)?
                .ok_or_else(|| io::Error::other(format!("chunk {} is missing", chunk.hash)))?;
            let mut bytes = Vec::new();
            file.take(chunk.len).read_to_end(&mut bytes)?;
            Ok(bytes)
        });
        Ok(Some((len, Box::new(reader))))
    }

    /// Chunks named by every verified chunk list of `blobs`, and by every
    /// chunk list still being fetched.
    pub(super) fn live_chunks(
        &self,
        blobs: &std::collections::BTreeSet<String>,
    ) -> Result<std::collections::BTreeSet<String>, StorageError> {
        let mut chunks = std::collections::BTreeSet::new();
        for blob in blobs {
            let Some(hash) = parse_hash(blob) else {
                continue;
            };
            if let Some(list) = self.chunk_list(&hash)? {
                chunks.extend(list.distinct_chunks().iter().map(BlobHash::to_hex));
            }
        }
        for blob in super::list_digests(&self.payload_dir(), PENDING_CHUNKS_SUFFIX)? {
            let Some(hash) = parse_hash(&blob) else {
                continue;
            };
            let path = self.pending_chunk_list_path(&hash);
            let Some(file) = open_if_exists(&path)? else {
                continue;
            };
            // A torn or foreign pending list protects nothing.
            if let Ok(list) = read_list(file, &hash) {
                chunks.extend(list.distinct_chunks().iter().map(BlobHash::to_hex));
            }
        }
        Ok(chunks)
    }

    /// Downloads abandoned for longer than `age` (a crashed review).
    pub(super) fn stale_incoming(
        &self,
        age: std::time::Duration,
    ) -> Result<Vec<PathBuf>, StorageError> {
        let dir = self.payload_dir();
        let entries = std::fs::read_dir(&dir)
            .map_err(|error| StorageError::Io(format!("list {}: {error}", dir.display())))?;
        let mut stale = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| StorageError::Io(error.to_string()))?;
            if !entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(INCOMING_PREFIX))
            {
                continue;
            }
            let old = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|elapsed| elapsed > age);
            if old {
                stale.push(entry.path());
            }
        }
        Ok(stale)
    }
}

fn parse_hash(hex: &str) -> Option<BlobHash> {
    super::is_hex_digest(hex)
        .then(|| BlobHash::from_hex(&hex.to_ascii_lowercase()))
        .flatten()
}

fn read_list(file: File, hash: &BlobHash) -> Result<ChunkList, StorageError> {
    let mut bytes = Vec::new();
    file.take(MAX_CHUNK_LIST_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| StorageError::Io(format!("read chunk list: {error}")))?;
    ChunkList::decode(&bytes, hash)
        .map_err(|error| StorageError::Parse(format!("stored chunk list: {error}")))
}

fn open_if_exists(path: &Path) -> Result<Option<File>, StorageError> {
    match File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StorageError::Io(format!(
            "open {}: {error}",
            path.display()
        ))),
    }
}
