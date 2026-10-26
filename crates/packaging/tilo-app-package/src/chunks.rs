//! Payload objects: how the payload blobs of a thin `.tapp` are hosted as
//! plain files.
//!
//! A thin package carries only metadata; its payload blobs live elsewhere as
//! *objects*, each a file named by a BLAKE3 hash:
//!
//! - `<blob>.blob` holds a whole payload blob, or one chunk of a large one.
//! - `<blob>.chunks` describes a blob larger than [`CHUNK_BYTES`] as a
//!   [`ChunkList`]: the blob is cut into fixed-size slices (the last one
//!   shorter), each stored as `<chunk>.blob`. Chunks are content addressed,
//!   so identical slices are stored once.
//!
//! A publisher hosts the objects beside the thin `.tapp` (same URL
//! directory); a registry keeps the ones it reviewed and serves them as
//! `/blobs/<hash>` and `/blobs/<hash>.chunks`. No object is trusted:
//! [`Reassembly`] checks every chunk against its name and the joined stream
//! against the whole-blob hash from the signed collection before the last
//! byte is released, so a chunk list can only ever fail a download, never
//! change what it delivers. [`split`] turns a full `.tapp` into its thin form
//! plus these objects with bounded memory.

use std::collections::BTreeSet;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use blake3::Hasher;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::blobs::{BlobHash, BlobSink, BlobSource, HashingReader, OpenBlob};
use crate::package::PackageMeta;
use crate::tapp::TappError;

/// Size of every chunk but the last of a chunked blob. Blobs up to this size
/// are hosted whole.
pub const CHUNK_BYTES: u64 = 16 * 1024 * 1024;

/// Largest single object (whole blob or chunk) a reader accepts.
pub const MAX_OBJECT_BYTES: u64 = 64 * 1024 * 1024;

/// Largest chunk list a reader accepts. A list entry is about 100 bytes, so
/// this describes blobs far beyond any payload limit.
pub const MAX_CHUNK_LIST_BYTES: u64 = 1024 * 1024;

/// Version of the [`ChunkList`] encoding: the package format's.
pub const CHUNK_LIST_FORMAT: u32 = crate::tapp::TAPP_FORMAT_VERSION;

/// File-name suffix of a whole blob or a chunk.
pub const BLOB_SUFFIX: &str = ".blob";

/// File-name suffix of a chunk list.
pub const CHUNKS_SUFFIX: &str = ".chunks";

/// `<hash>.blob`: the object holding the bytes named `hash`.
pub fn blob_object_name(hash: &BlobHash) -> String {
    format!("{hash}{BLOB_SUFFIX}")
}

/// `<hash>.chunks`: the chunk list of the blob named `hash`.
pub fn chunk_list_name(hash: &BlobHash) -> String {
    format!("{hash}{CHUNKS_SUFFIX}")
}

/// One slice of a chunked blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRef {
    #[serde(with = "hex_hash")]
    pub hash: BlobHash,
    pub len: u64,
}

/// How a blob larger than one object is cut into chunks:
///
/// ```json
/// {"format":2,"blob":"<hex>","len":N,"chunk_bytes":16777216,
///  "chunks":[{"hash":"<hex>","len":n},...]}
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkList {
    pub format: u32,
    #[serde(with = "hex_hash")]
    pub blob: BlobHash,
    pub len: u64,
    pub chunk_bytes: u64,
    pub chunks: Vec<ChunkRef>,
}

#[derive(Debug, Error)]
pub enum ChunkError {
    #[error("chunk list of {blob}: {reason}")]
    BadList { blob: BlobHash, reason: String },
    #[error("chunk {chunk} of {blob} does not match its name")]
    ChunkMismatch { blob: BlobHash, chunk: BlobHash },
    #[error("chunk {chunk} of {blob} is longer than its {len} listed bytes")]
    ChunkTooLong {
        blob: BlobHash,
        chunk: BlobHash,
        len: u64,
    },
    #[error("chunks of {0} do not join into the blob")]
    BlobMismatch(BlobHash),
    #[error("{blob} ended after {chunks} of its chunks")]
    Truncated { blob: BlobHash, chunks: usize },
}

impl From<ChunkError> for io::Error {
    fn from(error: ChunkError) -> Self {
        Self::new(io::ErrorKind::InvalidData, error)
    }
}

impl ChunkList {
    /// The list cutting the `len` bytes of `blob` into `chunks`, in order.
    pub fn new(blob: BlobHash, len: u64, chunk_bytes: u64, chunks: Vec<ChunkRef>) -> Self {
        Self {
            format: CHUNK_LIST_FORMAT,
            blob,
            len,
            chunk_bytes,
            chunks,
        }
    }

    /// Canonical JSON encoding.
    pub fn encode(&self) -> Vec<u8> {
        // Serializing plain integers, strings and vectors cannot fail.
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Decode a list fetched for `blob` and check its shape.
    ///
    /// # Errors
    ///
    /// Returns [`ChunkError::BadList`] for an oversized, malformed or
    /// inconsistent list, or one describing another blob.
    pub fn decode(bytes: &[u8], blob: &BlobHash) -> Result<Self, ChunkError> {
        let bad = |reason: String| ChunkError::BadList {
            blob: *blob,
            reason,
        };
        if bytes.len() as u64 > MAX_CHUNK_LIST_BYTES {
            return Err(bad(format!("exceeds {MAX_CHUNK_LIST_BYTES} bytes")));
        }
        let list: Self = serde_json::from_slice(bytes).map_err(|error| bad(error.to_string()))?;
        list.check(blob)?;
        Ok(list)
    }

    /// Check that the list describes `blob` as consecutive fixed-size
    /// slices. Says nothing about the bytes: [`Reassembly`] checks those.
    ///
    /// # Errors
    ///
    /// Returns [`ChunkError::BadList`] naming the first inconsistency.
    pub fn check(&self, blob: &BlobHash) -> Result<(), ChunkError> {
        let bad = |reason: &str| {
            Err(ChunkError::BadList {
                blob: *blob,
                reason: reason.to_string(),
            })
        };
        if self.format != CHUNK_LIST_FORMAT {
            return bad("unsupported format");
        }
        if self.blob != *blob {
            return bad("describes another blob");
        }
        if self.chunk_bytes == 0 || self.chunk_bytes > MAX_OBJECT_BYTES {
            return bad("chunk size out of range");
        }
        let expected = self.len.div_ceil(self.chunk_bytes);
        if self.len == 0 || self.chunks.len() as u64 != expected {
            return bad("chunk count does not match the blob length");
        }
        let mut total: u64 = 0;
        let last = self.chunks.len().saturating_sub(1);
        for (index, chunk) in self.chunks.iter().enumerate() {
            let fits = if index == last {
                chunk.len > 0 && chunk.len <= self.chunk_bytes
            } else {
                chunk.len == self.chunk_bytes
            };
            if !fits {
                return bad("chunk lengths are not consecutive fixed-size slices");
            }
            total = total.saturating_add(chunk.len);
        }
        if total != self.len {
            return bad("chunk lengths do not add up to the blob length");
        }
        Ok(())
    }

    pub fn distinct_chunks(&self) -> BTreeSet<BlobHash> {
        self.chunks.iter().map(|chunk| chunk.hash).collect()
    }
}

/// Verifies a chunked blob as its chunks stream in, in list order.
///
/// Feed each chunk's bytes through [`Self::update`], then call
/// [`Self::finish_chunk`], which checks the chunk against its name and, for
/// the last one, the joined bytes against the blob hash. A caller that holds
/// each chunk back until `finish_chunk` succeeds never releases a byte that
/// does not belong to the blob, and never releases the final byte of a blob
/// that does not verify.
pub struct Reassembly {
    list: ChunkList,
    next: usize,
    chunk: Hasher,
    chunk_len: u64,
    whole: Hasher,
}

impl Reassembly {
    /// Start verifying the blob `list` describes. The list must have passed
    /// [`ChunkList::check`] (as [`ChunkList::decode`] ensures).
    pub fn new(list: ChunkList) -> Self {
        Self {
            list,
            next: 0,
            chunk: Hasher::new(),
            chunk_len: 0,
            whole: Hasher::new(),
        }
    }

    /// The list being verified.
    pub fn list(&self) -> &ChunkList {
        &self.list
    }

    /// The chunk whose bytes come next, or `None` once all are verified.
    pub fn current(&self) -> Option<ChunkRef> {
        self.list.chunks.get(self.next).copied()
    }

    /// Bytes of the current chunk.
    ///
    /// # Errors
    ///
    /// Returns [`ChunkError::ChunkTooLong`] past the chunk's listed length,
    /// or [`ChunkError::BadList`] when every chunk is already verified.
    pub fn update(&mut self, bytes: &[u8]) -> Result<(), ChunkError> {
        let current = self.current().ok_or_else(|| ChunkError::BadList {
            blob: self.list.blob,
            reason: "bytes past the last chunk".into(),
        })?;
        self.chunk_len = self.chunk_len.saturating_add(bytes.len() as u64);
        if self.chunk_len > current.len {
            return Err(ChunkError::ChunkTooLong {
                blob: self.list.blob,
                chunk: current.hash,
                len: current.len,
            });
        }
        self.chunk.update(bytes);
        self.whole.update(bytes);
        Ok(())
    }

    /// End the current chunk: check it against its name and length, and
    /// after the last chunk the whole blob against its hash. Returns whether
    /// the blob is complete.
    ///
    /// # Errors
    ///
    /// Returns [`ChunkError::ChunkMismatch`] for a chunk that is short or
    /// does not hash to its name, or [`ChunkError::BlobMismatch`] when
    /// verified chunks do not join into the blob.
    pub fn finish_chunk(&mut self) -> Result<bool, ChunkError> {
        let current = self.current().ok_or_else(|| ChunkError::BadList {
            blob: self.list.blob,
            reason: "no chunk to finish".into(),
        })?;
        let actual = BlobHash(*self.chunk.finalize().as_bytes());
        if actual != current.hash || self.chunk_len != current.len {
            return Err(ChunkError::ChunkMismatch {
                blob: self.list.blob,
                chunk: current.hash,
            });
        }
        self.chunk.reset();
        self.chunk_len = 0;
        self.next = self.next.saturating_add(1);
        if self.next < self.list.chunks.len() {
            return Ok(false);
        }
        if BlobHash(*self.whole.finalize().as_bytes()) != self.list.blob {
            return Err(ChunkError::BlobMismatch(self.list.blob));
        }
        Ok(true)
    }

    /// Whether every chunk has been verified, and with it the blob.
    pub fn is_complete(&self) -> bool {
        self.next >= self.list.chunks.len()
    }
}

/// A reader over a chunked blob that fetches one chunk at a time and
/// releases a chunk's bytes only after [`Reassembly`] verified them. At most
/// one chunk is held in memory. Reading past the end of a blob that did not
/// verify is an error, never a clean end of stream.
pub struct ChunkedReader<F> {
    assembly: Reassembly,
    fetch: F,
    buffer: Vec<u8>,
    at: usize,
}

impl<F> ChunkedReader<F>
where
    F: FnMut(&ChunkRef) -> io::Result<Vec<u8>>,
{
    /// Read the blob `list` describes, asking `fetch` for each chunk's bytes.
    /// `fetch` should refuse more than the chunk's listed length.
    pub fn new(list: ChunkList, fetch: F) -> Self {
        Self {
            assembly: Reassembly::new(list),
            fetch,
            buffer: Vec::new(),
            at: 0,
        }
    }

    /// Whether every byte has been verified and handed out.
    pub fn is_complete(&self) -> bool {
        self.assembly.is_complete() && self.at >= self.buffer.len()
    }
}

impl<F> Read for ChunkedReader<F>
where
    F: FnMut(&ChunkRef) -> io::Result<Vec<u8>>,
{
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.at >= self.buffer.len() {
            let Some(chunk) = self.assembly.current() else {
                return Ok(0);
            };
            let bytes = (self.fetch)(&chunk)?;
            self.assembly.update(&bytes)?;
            self.assembly.finish_chunk()?;
            self.buffer = bytes;
            self.at = 0;
        }
        let rest = self.buffer.get(self.at..).unwrap_or_default();
        let n = rest.len().min(out.len());
        if let (Some(dst), Some(src)) = (out.get_mut(..n), rest.get(..n)) {
            dst.copy_from_slice(src);
        }
        self.at = self.at.saturating_add(n);
        Ok(n)
    }
}

/// What [`split`] wrote.
#[derive(Clone, Debug)]
pub struct Split {
    /// The package's metadata; its thin form is what the objects complete.
    pub meta: PackageMeta,
    pub whole_blobs: usize,
    pub chunked_blobs: usize,
    /// Distinct chunk objects written for the chunked blobs.
    pub chunks: usize,
    /// Distinct payload bytes.
    pub payload_bytes: u64,
}

#[derive(Debug, Error)]
pub enum SplitError {
    #[error(transparent)]
    Tapp(#[from] TappError),
    #[error("the package does not carry payload blob {0}; split needs the full .tapp")]
    NotFull(BlobHash),
    #[error("chunk size must be between 1 and {MAX_OBJECT_BYTES} bytes")]
    ChunkSize,
}

/// Decode the full `.tapp` from `reader` and write every payload blob it
/// carries into `out_dir` as payload objects: `<blob>.blob` for blobs up to
/// [`CHUNK_BYTES`], `<blob>.chunks` plus `<chunk>.blob` files for larger
/// ones. Streams: at most one chunk is held in memory.
///
/// # Errors
///
/// Returns an error when the archive does not decode, lacks a payload blob,
/// or an object cannot be written.
pub fn split(reader: impl Read, out_dir: &Path) -> Result<Split, SplitError> {
    split_with_chunk_bytes(reader, out_dir, CHUNK_BYTES)
}

/// [`split`] with a chunk size other than [`CHUNK_BYTES`], for tests that
/// need several chunks without hundreds of megabytes.
///
/// # Errors
///
/// As [`split`], or [`SplitError::ChunkSize`] for a size out of range.
pub fn split_with_chunk_bytes(
    reader: impl Read,
    out_dir: &Path,
    chunk_bytes: u64,
) -> Result<Split, SplitError> {
    if chunk_bytes == 0 || chunk_bytes > MAX_OBJECT_BYTES {
        return Err(SplitError::ChunkSize);
    }
    let mut writer = ObjectWriter {
        dir: out_dir,
        chunk_bytes,
        written: BTreeSet::new(),
        whole_blobs: 0,
        chunked_blobs: 0,
        chunks: 0,
        payload_bytes: 0,
        buffer: Vec::new(),
    };
    let tapp = crate::tapp::read(reader, &mut writer)?;
    if let Some(missing) = tapp.missing().first() {
        return Err(SplitError::NotFull(*missing));
    }
    Ok(Split {
        meta: tapp.meta,
        whole_blobs: writer.whole_blobs,
        chunked_blobs: writer.chunked_blobs,
        chunks: writer.chunks,
        payload_bytes: writer.payload_bytes,
    })
}

struct ObjectWriter<'a> {
    dir: &'a Path,
    chunk_bytes: u64,
    /// Objects written by this run, so a repeated chunk is written once.
    written: BTreeSet<BlobHash>,
    whole_blobs: usize,
    chunked_blobs: usize,
    chunks: usize,
    payload_bytes: u64,
    buffer: Vec<u8>,
}

impl ObjectWriter<'_> {
    fn write_object(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        tilo_files::native::atomic::write(&self.dir.join(name), bytes)
    }

    /// Read the next `want` bytes into the reused buffer.
    fn fill(&mut self, bytes: &mut dyn Read, want: u64) -> io::Result<()> {
        self.buffer.clear();
        bytes.take(want).read_to_end(&mut self.buffer)?;
        if self.buffer.len() as u64 != want {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "payload blob is shorter than its archive entry",
            ));
        }
        Ok(())
    }
}

impl BlobSink for ObjectWriter<'_> {
    fn accept(&mut self, hash: &BlobHash, len: u64, bytes: &mut dyn Read) -> io::Result<()> {
        self.payload_bytes = self.payload_bytes.saturating_add(len);
        if len <= self.chunk_bytes {
            self.fill(bytes, len)?;
            // The archive reader checks the hash only after the sink; never
            // write an object under a name its bytes do not have.
            if BlobHash::of(&self.buffer) != *hash {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("payload blob {hash} does not match its name"),
                ));
            }
            let buffer = std::mem::take(&mut self.buffer);
            let written = self.write_object(&blob_object_name(hash), &buffer);
            self.buffer = buffer;
            written?;
            self.written.insert(*hash);
            self.whole_blobs = self.whole_blobs.saturating_add(1);
            return Ok(());
        }
        let mut whole = Hasher::new();
        let mut chunks = Vec::new();
        let mut left = len;
        while left > 0 {
            let want = left.min(self.chunk_bytes);
            self.fill(bytes, want)?;
            whole.update(&self.buffer);
            let chunk = BlobHash::of(&self.buffer);
            if self.written.insert(chunk) {
                let buffer = std::mem::take(&mut self.buffer);
                let written = self.write_object(&blob_object_name(&chunk), &buffer);
                self.buffer = buffer;
                written?;
                self.chunks = self.chunks.saturating_add(1);
            }
            chunks.push(ChunkRef {
                hash: chunk,
                len: want,
            });
            left = left.saturating_sub(want);
        }
        if BlobHash(*whole.finalize().as_bytes()) != *hash {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("payload blob {hash} does not match its name"),
            ));
        }
        let list = ChunkList::new(*hash, len, self.chunk_bytes, chunks);
        self.write_object(&chunk_list_name(hash), &list.encode())?;
        self.chunked_blobs = self.chunked_blobs.saturating_add(1);
        Ok(())
    }
}

/// A directory of payload objects, as [`split`] writes it, read as a
/// [`BlobSource`]: a whole `<blob>.blob`, or a `<blob>.chunks` list whose
/// chunks are joined and verified one at a time.
#[derive(Clone, Debug)]
pub struct ObjectDir {
    dir: PathBuf,
}

impl ObjectDir {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The length of blob `hash`, after reading every byte of it and
    /// checking it against the name, or `None` when the directory has no
    /// object for it.
    ///
    /// # Errors
    ///
    /// Returns an error when an object cannot be read or does not verify.
    pub fn verified_len(&self, hash: &BlobHash) -> io::Result<Option<u64>> {
        let Some(blob) = self.open_blob(hash)? else {
            return Ok(None);
        };
        let mut hashing = HashingReader::new(blob.reader.take(blob.len.saturating_add(1)));
        io::copy(&mut hashing, &mut io::sink())?;
        let (actual, len) = hashing.finish();
        if actual != *hash || len != blob.len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("payload object {hash} does not match its name"),
            ));
        }
        Ok(Some(len))
    }

    fn open_file(&self, name: &str) -> io::Result<Option<std::fs::File>> {
        match std::fs::File::open(self.dir.join(name)) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// `hash`'s object: the `.blob` name [`split`] writes, or the bare name
    /// `registry-server` writes for a reviewed payload object.
    fn open_named(&self, hash: &BlobHash) -> io::Result<Option<std::fs::File>> {
        if let Some(file) = self.open_file(&blob_object_name(hash))? {
            return Ok(Some(file));
        }
        self.open_file(&hash.to_hex())
    }
}

impl BlobSource for ObjectDir {
    fn open_blob(&self, hash: &BlobHash) -> io::Result<Option<OpenBlob<'_>>> {
        if let Some(file) = self.open_named(hash)? {
            let len = file.metadata()?.len();
            return Ok(Some(OpenBlob {
                len,
                reader: Box::new(io::BufReader::new(file)),
            }));
        }
        let Some(file) = self.open_file(&chunk_list_name(hash))? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        file.take(MAX_CHUNK_LIST_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)?;
        let list = ChunkList::decode(&bytes, hash)?;
        let len = list.len;
        let blob = *hash;
        let reader = ChunkedReader::new(list, move |chunk: &ChunkRef| {
            let file = self.open_named(&chunk.hash)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("chunk {} of {blob} is missing", chunk.hash),
                )
            })?;
            let mut bytes = Vec::new();
            file.take(chunk.len).read_to_end(&mut bytes)?;
            Ok(bytes)
        });
        Ok(Some(OpenBlob {
            len,
            reader: Box::new(reader),
        }))
    }
}

mod hex_hash {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::blobs::BlobHash;

    pub fn serialize<S: Serializer>(hash: &BlobHash, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hash.to_hex())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<BlobHash, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.len() != 64 || text.bytes().any(|byte| byte.is_ascii_uppercase()) {
            return Err(serde::de::Error::custom(
                "expected 64 lower-case hex digits",
            ));
        }
        BlobHash::from_hex(&text)
            .ok_or_else(|| serde::de::Error::custom("expected 64 lower-case hex digits"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{Manifest, SigningIdentity, pack};

    /// Deterministic bytes that do not repeat at chunk boundaries.
    fn noise(len: usize, seed: u8) -> Vec<u8> {
        let mut state = u32::from(seed).wrapping_mul(2_654_435_761).wrapping_add(1);
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state.to_le_bytes()[0]
            })
            .collect()
    }

    fn full_tapp(files: &[(&str, Vec<u8>)]) -> (Vec<u8>, PackageMeta) {
        let identity = SigningIdentity::generate();
        let dir = tempfile::tempdir().unwrap();
        for (path, bytes) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let manifest = Manifest::from_toml_str(&format!(
            r#"
[package]
id = "so.tilo.chunks"
version = "1.0.0"
kind = "app"

[publisher]
identity = "ed25519:{}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/app"
type = "gui"
"#,
            hex::encode(identity.public_key_bytes())
        ))
        .unwrap();
        let packed = pack(dir.path(), &manifest, &identity).unwrap();
        let bytes = crate::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
        (bytes, packed.meta)
    }

    fn objects(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().into_string().unwrap(),
                    std::fs::read(entry.path()).unwrap(),
                )
            })
            .collect()
    }

    fn reader_for(
        list: ChunkList,
        objects: &BTreeMap<String, Vec<u8>>,
    ) -> ChunkedReader<impl FnMut(&ChunkRef) -> io::Result<Vec<u8>> + '_> {
        ChunkedReader::new(list, move |chunk: &ChunkRef| {
            objects
                .get(&blob_object_name(&chunk.hash))
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no chunk"))
        })
    }

    #[test]
    fn split_writes_whole_and_chunked_objects_that_reassemble() {
        let big = noise(10_000, 1);
        // Two equal slices in one blob: the chunk is stored once.
        let mut repeated = vec![7u8; 3 * 1024];
        repeated.extend_from_slice(&noise(100, 2));
        let small = b"#!/bin/true\n".to_vec();
        let (bytes, meta) = full_tapp(&[
            ("bin/app", small.clone()),
            ("lib/big.so", big.clone()),
            ("share/repeated", repeated.clone()),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let split = split_with_chunk_bytes(bytes.as_slice(), dir.path(), 1024).unwrap();
        assert_eq!(split.meta, meta);
        assert_eq!(split.whole_blobs, 1);
        assert_eq!(split.chunked_blobs, 2);
        // 10 distinct chunks of `big`, then one zero-like run plus a tail.
        assert_eq!(split.chunks, 10 + 2);
        let objects = objects(dir.path());
        assert!(objects.contains_key(&blob_object_name(&BlobHash::of(&small))));
        for blob in [&big, &repeated] {
            let hash = BlobHash::of(blob);
            assert!(!objects.contains_key(&blob_object_name(&hash)));
            let list = ChunkList::decode(&objects[&chunk_list_name(&hash)], &hash).unwrap();
            assert_eq!(list.len, blob.len() as u64);
            assert_eq!(list.chunk_bytes, 1024);
            let mut out = Vec::new();
            reader_for(list, &objects).read_to_end(&mut out).unwrap();
            assert_eq!(&out, blob);
        }
        let dir_source = ObjectDir::new(dir.path());
        for blob in [&big, &repeated, &small] {
            let hash = BlobHash::of(blob);
            assert_eq!(
                dir_source.verified_len(&hash).unwrap(),
                Some(blob.len() as u64)
            );
        }
        assert_eq!(
            dir_source.verified_len(&BlobHash::of(b"absent")).unwrap(),
            None
        );
        let unpacked = tempfile::tempdir().unwrap();
        crate::unpack(&split.meta, &dir_source, &unpacked.path().join("tree")).unwrap();
        assert_eq!(
            std::fs::read(unpacked.path().join("tree/lib/big.so")).unwrap(),
            big
        );
        let list_text =
            String::from_utf8(objects[&chunk_list_name(&BlobHash::of(&big))].clone()).unwrap();
        assert!(list_text.starts_with(&format!(
            "{{\"format\":2,\"blob\":\"{}\",\"len\":10000,\"chunk_bytes\":1024,\"chunks\":[{{\"hash\":\"",
            BlobHash::of(&big)
        )));
    }

    #[test]
    fn the_real_chunk_size_splits_a_blob_just_over_it() {
        let blob = noise(usize::try_from(CHUNK_BYTES).unwrap() + 3, 9);
        let (bytes, _meta) = full_tapp(&[("bin/app", b"x".to_vec()), ("lib/big", blob.clone())]);
        let dir = tempfile::tempdir().unwrap();
        let split = split(bytes.as_slice(), dir.path()).unwrap();
        assert_eq!(
            (split.whole_blobs, split.chunked_blobs, split.chunks),
            (1, 1, 2)
        );
        let hash = BlobHash::of(&blob);
        let objects = objects(dir.path());
        let list = ChunkList::decode(&objects[&chunk_list_name(&hash)], &hash).unwrap();
        assert_eq!(list.chunk_bytes, CHUNK_BYTES);
        assert_eq!(
            list.chunks
                .iter()
                .map(|chunk| chunk.len)
                .collect::<Vec<_>>(),
            [CHUNK_BYTES, 3]
        );
        let mut out = Vec::new();
        reader_for(list, &objects).read_to_end(&mut out).unwrap();
        assert_eq!(BlobHash::of(&out), hash);
    }

    #[test]
    fn reassembly_fails_closed_on_tampered_reordered_or_truncated_chunks() {
        let blob = noise(4096 + 100, 3);
        let hash = BlobHash::of(&blob);
        let (bytes, _meta) = full_tapp(&[("bin/app", b"x".to_vec()), ("lib/big", blob.clone())]);
        let dir = tempfile::tempdir().unwrap();
        split_with_chunk_bytes(bytes.as_slice(), dir.path(), 1024).unwrap();
        let objects = objects(dir.path());
        let list = ChunkList::decode(&objects[&chunk_list_name(&hash)], &hash).unwrap();

        // phase: a_tampered_chunk_is_refused_before_its_bytes_are_released
        {
            let mut tampered = objects.clone();
            let name = blob_object_name(&list.chunks[2].hash);
            tampered.get_mut(&name).unwrap()[5] ^= 1;
            let mut reader = reader_for(list.clone(), &tampered);
            let mut out = Vec::new();
            let error = reader.read_to_end(&mut out).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(out.len(), 2 * 1024, "only verified chunks were released");
        }
        // phase: a_reordered_list_never_completes_the_blob
        {
            let mut swapped = list.clone();
            swapped.chunks.swap(0, 1);
            swapped.check(&hash).unwrap();
            let mut reader = reader_for(swapped, &objects);
            let mut out = Vec::new();
            let error = reader.read_to_end(&mut out).unwrap_err();
            assert!(error.to_string().contains("do not join"), "{error}");
            assert!(out.len() < blob.len(), "the final chunk is held back");
        }
        // phase: a_list_for_another_blob_or_with_bad_lengths_is_refused
        {
            let other = BlobHash::of(b"other");
            assert!(ChunkList::decode(&objects[&chunk_list_name(&hash)], &other).is_err());
            let mut short = list.clone();
            short.chunks.pop();
            assert!(short.check(&hash).is_err());
            let mut uneven = list.clone();
            uneven.chunks[0].len = 1000;
            uneven.chunks[4].len = 124;
            assert!(uneven.check(&hash).is_err());
            let zero = ChunkList::new(list.blob, list.len, 0, list.chunks.clone());
            assert!(zero.check(&hash).is_err());
            assert!(ChunkList::decode(b"{\"format\":1}", &hash).is_err());
            let upper = std::str::from_utf8(&objects[&chunk_list_name(&hash)])
                .unwrap()
                .replacen(&hash.to_hex(), &hash.to_hex().to_ascii_uppercase(), 1);
            assert!(ChunkList::decode(upper.as_bytes(), &hash).is_err());
        }
        // phase: a_truncated_or_overlong_chunk_is_refused
        {
            let mut truncated = objects.clone();
            truncated
                .get_mut(&blob_object_name(&list.chunks[4].hash))
                .unwrap()
                .pop();
            let mut out = Vec::new();
            assert!(
                reader_for(list.clone(), &truncated)
                    .read_to_end(&mut out)
                    .is_err()
            );
            let mut assembly = Reassembly::new(list.clone());
            assert!(matches!(
                assembly.update(&[0; 1025]),
                Err(ChunkError::ChunkTooLong { .. })
            ));
        }
        // phase: verified_chunks_stream_into_a_reassembly_piecewise
        {
            let mut assembly = Reassembly::new(list.clone());
            for (index, chunk) in list.chunks.iter().enumerate() {
                let bytes = &objects[&blob_object_name(&chunk.hash)];
                let (head, tail) = bytes.split_at(bytes.len() / 2);
                assembly.update(head).unwrap();
                assembly.update(tail).unwrap();
                assert_eq!(
                    assembly.finish_chunk().unwrap(),
                    index + 1 == list.chunks.len()
                );
            }
            assert!(assembly.is_complete());
        }
    }

    #[test]
    fn object_dir_reads_the_bare_names_a_registry_serves_too() {
        let dir = tempfile::tempdir().unwrap();
        let small = b"a reviewed payload object".to_vec();
        let small_hash = BlobHash::of(&small);
        std::fs::write(dir.path().join(small_hash.to_hex()), &small).unwrap();

        let chunked = noise(2048, 3);
        let chunked_hash = BlobHash::of(&chunked);
        let (first, second) = chunked.split_at(1024);
        let first_hash = BlobHash::of(first);
        let second_hash = BlobHash::of(second);
        std::fs::write(dir.path().join(first_hash.to_hex()), first).unwrap();
        std::fs::write(dir.path().join(second_hash.to_hex()), second).unwrap();
        let list = ChunkList::new(
            chunked_hash,
            chunked.len() as u64,
            1024,
            vec![
                ChunkRef {
                    hash: first_hash,
                    len: 1024,
                },
                ChunkRef {
                    hash: second_hash,
                    len: 1024,
                },
            ],
        );
        std::fs::write(
            dir.path().join(chunk_list_name(&chunked_hash)),
            list.encode(),
        )
        .unwrap();

        let source = ObjectDir::new(dir.path());
        assert_eq!(
            source.verified_len(&small_hash).unwrap(),
            Some(small.len() as u64)
        );
        assert_eq!(
            source.verified_len(&chunked_hash).unwrap(),
            Some(chunked.len() as u64)
        );
        let mut out = Vec::new();
        source
            .open_blob(&chunked_hash)
            .unwrap()
            .unwrap()
            .reader
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, chunked);
    }

    #[test]
    fn split_refuses_a_thin_package() {
        let (bytes, meta) = full_tapp(&[("bin/app", b"x".to_vec())]);
        drop(bytes);
        let thin = crate::tapp::to_bytes(&meta, None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            split(thin.as_slice(), dir.path()),
            Err(SplitError::NotFull(_))
        ));
    }
}
