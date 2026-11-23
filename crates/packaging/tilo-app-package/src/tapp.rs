//! The `.tapp` archive: how a package travels as one file.

use std::collections::BTreeMap;
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::blobs::{BlobHash, BlobSink, BlobSource, DiscardBlobs, HashingReader};
use crate::id::PackageId;
use crate::package::{MetaError, PackageMeta};
use crate::signed::SIGNED_ROOT_LEN;

/// The store package format this build writes: the archive layout, the
/// manifest it signs, the signed root and the chunk list. A device reads and
/// installs packages of every version in [`READABLE_FORMATS`]
/// (docs/architecture/compatibility.md); `interface.toml` holds the manifest
/// and header schema, and the repository policy fails a change to it that
/// keeps the number. The binary layouts (signed root, chunk list) are not in
/// that schema: a change to them bumps this by hand.
pub const TAPP_FORMAT_VERSION: u32 = registry_types::PACKAGE_FORMAT_VERSION;

/// Every package format a device reads and installs, oldest first; the last
/// is [`TAPP_FORMAT_VERSION`]. A package is named by the hash of its signed
/// root, which includes the format, and environments name their images by
/// that id: an image written at an earlier format cannot be rewritten at
/// this one without becoming another image, so it is read as it is. Apps
/// are read the same way: publishing at a new format moves the writer, not
/// the packages already published, and a reader keeps installing every
/// format it can verify.
///
/// Format 2 changed only the registry catalogue; format 3 only drops the
/// retired `[sandbox.fs]` manifest block. The archive, the signed root and
/// the chunk list are byte for byte format 1's, so one reader reads them
/// all. A format that changes one of those layouts needs its reader here
/// before anything ships at it.
pub const READABLE_FORMATS: [u32; 3] = [1, 2, TAPP_FORMAT_VERSION];

/// What a package at `format`, which this build does not read, means for
/// the person holding it, and what to do.
#[must_use]
pub fn unreadable_format(format: u32) -> String {
    let newest = TAPP_FORMAT_VERSION;
    if format > newest {
        format!(
            "package format {format} is newer than this Tilo reads (up to {newest}): \
             update Tilo (`tilo update`) to use this package"
        )
    } else {
        format!(
            "package format {format} is older than any Tilo reads: install the package \
             again from its publisher, who must rebuild it at format {newest}"
        )
    }
}

const HEADER: &str = "tapp.toml";
const SIGNED_ROOT: &str = "signed-root";
const MANIFEST: &str = "manifest.toml";
const COLLECTION: &str = "collection.qcl";
const BLOB_PREFIX: &str = "blobs/";

const MAX_HEADER_BYTES: u64 = 4 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_COLLECTION_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct Header {
    package_id: String,
    format: u32,
}

#[derive(Clone, Debug)]
pub struct Tapp {
    pub meta: PackageMeta,
    pub carried: BTreeMap<BlobHash, u64>,
}

impl Tapp {
    pub fn missing(&self) -> Vec<BlobHash> {
        self.meta
            .payload_hashes()
            .into_iter()
            .filter(|hash| !self.carried.contains_key(hash))
            .collect()
    }

    pub fn is_complete(&self) -> bool {
        self.missing().is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TappError {
    #[error("reading archive: {0}")]
    Io(#[from] io::Error),
    #[error(".tapp has no {0}")]
    MissingEntry(&'static str),
    #[error("tapp.toml: {0}")]
    BadHeader(String),
    #[error("{}", unreadable_format(*.0))]
    UnsupportedFormat(u32),
    #[error("the archive header says package format {header} but its signed root says {root}")]
    HeaderFormatMismatch { header: u32, root: u32 },
    #[error("archive entry {name} exceeds {limit} bytes")]
    EntryTooLarge { name: String, limit: u64 },
    #[error("archive entry {0} appears twice")]
    DuplicateEntry(String),
    #[error("package metadata must precede payload blobs; {0} came first")]
    MetadataAfterBlobs(String),
    #[error("payload blob name {0:?} is not a BLAKE3 hash")]
    BadBlobName(String),
    #[error("payload blob {0} does not match its name")]
    BlobHashMismatch(BlobHash),
    #[error("header names {header} but the signed root is {root}")]
    HeaderIdMismatch { header: PackageId, root: PackageId },
    #[error("payload blob {0} is not available to write")]
    MissingBlob(BlobHash),
    #[error("application tapp is {bytes} bytes; limit is {limit} bytes")]
    PackageTooLarge { bytes: u64, limit: u64 },
    #[error("written archive lacks {0} payload blob(s)")]
    IncompleteArchive(usize),
    #[error("written archive id {actual} does not match {expected}")]
    WrittenIdMismatch {
        expected: PackageId,
        actual: PackageId,
    },
    #[error("written archive does not verify: {0}")]
    VerificationFailed(String),
    #[error(transparent)]
    Meta(#[from] MetaError),
}

#[derive(Default)]
struct Pending {
    header: Option<Header>,
    signed_root: Option<Vec<u8>>,
    manifest: Option<Vec<u8>>,
    collection: Option<Vec<u8>>,
}

impl Pending {
    fn finish(self) -> Result<PackageMeta, TappError> {
        let header = self.header.ok_or(TappError::MissingEntry(HEADER))?;
        if !READABLE_FORMATS.contains(&header.format) {
            return Err(TappError::UnsupportedFormat(header.format));
        }
        let header_id: PackageId = header
            .package_id
            .parse()
            .map_err(|error| TappError::BadHeader(format!("package_id: {error}")))?;
        let signed_root = self
            .signed_root
            .ok_or(TappError::MissingEntry(SIGNED_ROOT))?;
        let manifest = self.manifest.ok_or(TappError::MissingEntry(MANIFEST))?;
        let collection = self.collection.ok_or(TappError::MissingEntry(COLLECTION))?;
        let root = crate::SignedRoot::decode(&signed_root).map_err(MetaError::from)?;
        if root.package_id() != header_id {
            return Err(TappError::HeaderIdMismatch {
                header: header_id,
                root: root.package_id(),
            });
        }
        if root.format != header.format {
            return Err(TappError::HeaderFormatMismatch {
                header: header.format,
                root: root.format,
            });
        }
        Ok(PackageMeta::from_root(root, manifest, collection)?)
    }
}

fn read_small(
    entry: &mut dyn Read,
    name: &str,
    len: u64,
    limit: u64,
) -> Result<Vec<u8>, TappError> {
    if len > limit {
        return Err(TappError::EntryTooLarge {
            name: name.to_string(),
            limit,
        });
    }
    let mut buf = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    entry.take(limit).read_to_end(&mut buf)?;
    Ok(buf)
}

fn set_once(slot: &mut Option<Vec<u8>>, name: &str, bytes: Vec<u8>) -> Result<(), TappError> {
    if slot.replace(bytes).is_some() {
        return Err(TappError::DuplicateEntry(name.to_string()));
    }
    Ok(())
}

/// Decode an archive, handing each carried payload blob to `sink`.
pub fn read<R: Read>(reader: R, sink: &mut dyn BlobSink) -> Result<Tapp, TappError> {
    let mut archive = tar::Archive::new(reader);
    let mut pending = Some(Pending::default());
    let mut meta: Option<PackageMeta> = None;
    let mut carried = BTreeMap::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let name = entry.path()?.to_string_lossy().into_owned();
        let len = entry.size();
        if let Some(hex) = name.strip_prefix(BLOB_PREFIX) {
            let hash =
                BlobHash::from_hex(hex).ok_or_else(|| TappError::BadBlobName(name.clone()))?;
            if meta.is_none() {
                let Some(ready) = pending.take() else {
                    return Err(TappError::MetadataAfterBlobs(name));
                };
                // A metadata entry still missing at the first blob is out of
                // order; anything else (another package format, a broken
                // root) is that error, named as itself.
                let finished = ready.finish().map_err(|error| match error {
                    TappError::MissingEntry(_) => TappError::MetadataAfterBlobs(name.clone()),
                    other => other,
                })?;
                sink.metadata(&finished)?;
                meta = Some(finished);
            }
            let referenced = meta
                .as_ref()
                .is_some_and(|meta| meta.collection.entries.values().any(|h| *h == hash));
            if !referenced || carried.contains_key(&hash) {
                continue;
            }
            let mut hashing = HashingReader::new(&mut entry);
            sink.accept(&hash, len, &mut hashing)?;
            io::copy(&mut hashing, &mut io::sink())?;
            let (actual, actual_len) = hashing.finish();
            if actual != hash || actual_len != len {
                return Err(TappError::BlobHashMismatch(hash));
            }
            carried.insert(hash, len);
            continue;
        }
        let Some(slots) = pending.as_mut() else {
            if matches!(name.as_str(), HEADER | SIGNED_ROOT | MANIFEST | COLLECTION) {
                return Err(TappError::DuplicateEntry(name));
            }
            continue;
        };
        match name.as_str() {
            HEADER => {
                let bytes = read_small(&mut entry, &name, len, MAX_HEADER_BYTES)?;
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| TappError::BadHeader("not UTF-8".into()))?;
                let header: Header = toml::from_str(text)
                    .map_err(|error| TappError::BadHeader(error.to_string()))?;
                if slots.header.replace(header).is_some() {
                    return Err(TappError::DuplicateEntry(name));
                }
            }
            SIGNED_ROOT => {
                let bytes = read_small(&mut entry, &name, len, SIGNED_ROOT_LEN as u64)?;
                set_once(&mut slots.signed_root, &name, bytes)?;
            }
            MANIFEST => {
                let bytes = read_small(&mut entry, &name, len, MAX_MANIFEST_BYTES)?;
                set_once(&mut slots.manifest, &name, bytes)?;
            }
            COLLECTION => {
                let bytes = read_small(&mut entry, &name, len, MAX_COLLECTION_BYTES)?;
                set_once(&mut slots.collection, &name, bytes)?;
            }
            _ => {}
        }
    }
    let meta = if let Some(meta) = meta {
        meta
    } else {
        let meta = pending
            .take()
            .ok_or(TappError::MissingEntry(SIGNED_ROOT))?
            .finish()?;
        sink.metadata(&meta)?;
        meta
    };
    Ok(Tapp { meta, carried })
}

fn append(
    tar: &mut tar::Builder<impl Write>,
    name: &str,
    len: u64,
    bytes: impl Read,
) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(len);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, name, bytes)
}

/// Write `meta` as an archive to `out` and return `out`.
///
/// With `payload = None` the archive is thin. With a source it is full:
/// every distinct payload blob is streamed from `payload` and checked
/// against its hash as it is written.
///
/// # Errors
///
/// Returns [`TappError::MissingBlob`] if a full archive's source lacks a
/// blob, [`TappError::BlobHashMismatch`] if the source's bytes do not match,
/// or [`TappError::Io`] if writing fails.
pub fn write<W: Write>(
    out: W,
    meta: &PackageMeta,
    payload: Option<&dyn BlobSource>,
) -> Result<W, TappError> {
    let mut tar = tar::Builder::new(out);
    let header = toml::to_string(&Header {
        package_id: meta.id.to_string(),
        // An image read at an earlier format is shared at it: its root, and
        // so its id, are that format's.
        format: meta.root.format,
    })
    .map_err(|error| TappError::BadHeader(error.to_string()))?;
    append(&mut tar, HEADER, header.len() as u64, header.as_bytes())?;
    let root = meta.signed_root_bytes();
    append(&mut tar, SIGNED_ROOT, root.len() as u64, root.as_slice())?;
    append(
        &mut tar,
        MANIFEST,
        meta.manifest_bytes.len() as u64,
        meta.manifest_bytes.as_slice(),
    )?;
    append(
        &mut tar,
        COLLECTION,
        meta.collection_bytes.len() as u64,
        meta.collection_bytes.as_slice(),
    )?;
    if let Some(source) = payload {
        for hash in meta.payload_hashes() {
            let blob = source
                .open_blob(&hash)?
                .ok_or(TappError::MissingBlob(hash))?;
            let mut hashing = HashingReader::new(blob.reader.take(blob.len));
            append(
                &mut tar,
                &format!("{BLOB_PREFIX}{hash}"),
                blob.len,
                &mut hashing,
            )?;
            let (actual, actual_len) = hashing.finish();
            if actual != hash || actual_len != blob.len {
                return Err(TappError::BlobHashMismatch(hash));
            }
        }
    }
    Ok(tar.into_inner()?)
}

/// Write `meta` as an archive into a new byte vector.
///
/// # Errors
///
/// See [`write()`].
pub fn to_bytes(
    meta: &PackageMeta,
    payload: Option<&dyn BlobSource>,
) -> Result<Vec<u8>, TappError> {
    write(Vec::new(), meta, payload)
}

/// What [`write_file`] does when the output path already exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExistingOutput {
    /// Atomically replace an existing file once the new archive passes every check.
    Replace,
    /// Refuse to overwrite an existing file (fails with [`TappError::Io`] of
    /// [`io::ErrorKind::AlreadyExists`]); the existing file is untouched.
    Refuse,
}

/// Which size limit [`write_file`] enforces on an application archive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SizeLimit {
    /// Enforce [`crate::app_size_limit`] of the manifest.
    #[default]
    Manifest,
    /// Enforce [`crate::local_build_size_limit`] of the manifest, for unpublished
    /// developer builds installed only into a local development store.
    LocalBuild,
}

/// How [`write_file`] writes and checks a `.tapp` archive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteOptions {
    pub existing: ExistingOutput,
    pub size_limit: SizeLimit,
}

impl WriteOptions {
    /// Replace any existing file at the output path, enforcing [`crate::app_size_limit`].
    #[must_use]
    pub const fn replace() -> Self {
        Self {
            existing: ExistingOutput::Replace,
            size_limit: SizeLimit::Manifest,
        }
    }

    /// Refuse to overwrite an existing file at the output path, enforcing [`crate::app_size_limit`].
    #[must_use]
    pub const fn create_new() -> Self {
        Self {
            existing: ExistingOutput::Refuse,
            size_limit: SizeLimit::Manifest,
        }
    }

    /// Use [`crate::local_build_size_limit`] instead of [`crate::app_size_limit`].
    #[must_use]
    pub const fn local_build(mut self) -> Self {
        self.size_limit = SizeLimit::LocalBuild;
        self
    }
}

/// What [`write_file`] wrote and verified on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Written {
    pub id: PackageId,
    pub bytes: u64,
}

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TempGuard {
    path: Option<PathBuf>,
}

impl TempGuard {
    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn parent_or_current(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn temp_path(path: &Path, sequence: u64) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(|| "file".to_owned(), |n| n.to_string_lossy().into_owned());
    let tmp_name = format!(".{name}.{}.{sequence}.tmp", std::process::id());
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(tmp_name),
        _ => PathBuf::from(tmp_name),
    }
}

fn create_temp_file(path: &Path) -> io::Result<(PathBuf, std::fs::File)> {
    for _ in 0..32 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let tmp = temp_path(path, sequence);
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(file) => return Ok((tmp, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique temporary archive file",
    ))
}

/// Write `meta` (full when `payload` is given, thin otherwise) to `path`
/// atomically, enforce the application size ceiling, read the written archive
/// back to verify every carried blob and the publisher signature, and publish
/// it only once every check passes.
///
/// A failure at any step removes the temporary file and leaves `path`
/// (absent or existing) untouched.
pub fn write_file(
    path: &Path,
    meta: &PackageMeta,
    payload: Option<&dyn BlobSource>,
    options: WriteOptions,
) -> Result<Written, TappError> {
    if options.existing == ExistingOutput::Refuse {
        match std::fs::symlink_metadata(path) {
            Ok(_) => {
                return Err(TappError::Io(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} already exists", path.display()),
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(TappError::Io(error)),
        }
    }

    let (tmp_path, file) = create_temp_file(path)?;
    let mut guard = TempGuard {
        path: Some(tmp_path.clone()),
    };

    let bytes = {
        let mut file = file;
        let mut archive = write(io::BufWriter::new(&mut file), meta, payload)?;
        archive.flush()?;
        drop(archive);

        let bytes = file.metadata()?.len();
        let limit = match options.size_limit {
            SizeLimit::Manifest => crate::app_size_limit(&meta.manifest),
            SizeLimit::LocalBuild => crate::local_build_size_limit(&meta.manifest),
        };
        if let Some(limit) = limit
            && bytes > limit
        {
            return Err(TappError::PackageTooLarge { bytes, limit });
        }

        file.seek(io::SeekFrom::Start(0))?;
        let decoded = read(io::BufReader::new(&mut file), &mut DiscardBlobs)?;
        if payload.is_some() && !decoded.missing().is_empty() {
            return Err(TappError::IncompleteArchive(decoded.missing().len()));
        }
        if decoded.meta.id != meta.id {
            return Err(TappError::WrittenIdMismatch {
                expected: meta.id,
                actual: decoded.meta.id,
            });
        }
        match crate::verify(&decoded.meta, &[meta.publisher()]) {
            crate::Verification::Trusted { .. } => {}
            other => return Err(TappError::VerificationFailed(format!("{other:?}"))),
        }

        file.sync_all()?;
        bytes
    };

    match options.existing {
        ExistingOutput::Replace => {
            std::fs::rename(&tmp_path, path)?;
            guard.disarm();
        }
        ExistingOutput::Refuse => {
            std::fs::hard_link(&tmp_path, path)?;
            drop(guard);
        }
    }
    tilo_files::native::atomic::sync_directory(parent_or_current(path))?;

    Ok(Written { id: meta.id, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blobs::DiscardBlobs;
    use crate::{Manifest, SigningIdentity, pack};

    /// `packed`'s package as it would have been written at `format`, signed
    /// by a fresh publisher.
    fn at_format(packed: &crate::Packed, format: u32) -> (SigningIdentity, PackageMeta) {
        let identity = SigningIdentity::generate();
        let mut root = crate::SignedRoot {
            format,
            publisher_identity: identity.public_key_bytes(),
            signature: Some([0; 64]),
            ..packed.meta.root.clone()
        };
        root.signature = Some(identity.sign_signed_root(&root).unwrap());
        let meta = PackageMeta::from_root(
            root,
            packed.meta.manifest_bytes.clone(),
            packed.meta.collection_bytes.clone(),
        )
        .unwrap();
        (identity, meta)
    }

    fn packed() -> (tempfile::TempDir, crate::Packed) {
        let identity = SigningIdentity::generate();
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("payload");
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::write(payload.join("bin/app"), vec![0xAB; 50_000]).unwrap();
        std::fs::write(payload.join("bin/copy"), vec![0xAB; 50_000]).unwrap();
        std::fs::write(payload.join("README"), b"hello").unwrap();
        let manifest = Manifest::from_toml_str(&format!(
            r#"
[package]
id = "so.tilo.codec"
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
        let packed = pack(&payload, &manifest, &identity).unwrap();
        (dir, packed)
    }

    /// `bytes` with its header naming `format`: the number the archive writes after
    /// `format = `.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "offsets into an archive this test built"
    )]
    fn header_names(bytes: &mut Vec<u8>, format: u32) {
        let marker = b"format = ";
        let at = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("the archive's header names its format");
        let value = at + marker.len();
        let end = value
            + bytes[value..]
                .iter()
                .position(|byte| *byte == b'\n')
                .expect("the header's line ends");
        bytes.splice(value..end, format.to_string().into_bytes());
    }

    tilo_test_env::scenario! {
        /// An archive is read at the format it names, when that is one a device
        /// reads, and otherwise refused saying what to do.
        fn tapp_archives_are_read_or_refused_by_their_format;

        phase a_newer_format_fails_closed_and_says_to_update;
        {
            let (_dir, packed) = packed();
            let newer = crate::TAPP_FORMAT_VERSION + 1;
            let mut bytes = to_bytes(&packed.meta, None).unwrap();
            header_names(&mut bytes, newer);
            let error = read(bytes.as_slice(), &mut DiscardBlobs).unwrap_err();
            assert!(
                matches!(error, TappError::UnsupportedFormat(format) if format == newer),
                "{error}"
            );
            assert!(error.to_string().contains("update Tilo"), "{error}");
        }

        phase a_full_archive_of_another_format_says_so;
        // A package of another format carrying its blobs, in the right
        // order, was reported as "metadata must precede payload blobs".
        {
            let (_dir, packed) = packed();
            let mut bytes = to_bytes(&packed.meta, Some(&packed)).unwrap();
            header_names(&mut bytes, 1);
            let error = read(bytes.as_slice(), &mut DiscardBlobs).unwrap_err();
            assert!(
                matches!(
                    error,
                    TappError::HeaderFormatMismatch { header: 1, root }
                        if root == crate::TAPP_FORMAT_VERSION
                ),
                "{error}"
            );
        }

        phase an_app_at_an_earlier_format_is_read;
        // The store published every app at format 1; later formats changed only
        // the catalogue and the manifest, so such an app verifies and installs as
        // it is.
        {
            let (_dir, packed) = packed();
            let (identity, meta) = at_format(&packed, 1);
            assert_eq!(meta.root.publisher_identity, identity.public_key_bytes());
            let bytes = to_bytes(&meta, Some(&packed)).unwrap();
            let read = read(bytes.as_slice(), &mut DiscardBlobs).unwrap();
            assert_eq!(read.meta.id, meta.id);
            assert_eq!(read.meta.root.format, 1);
            assert!(!read.meta.manifest.package.kind.is_image());
            assert!(matches!(
                crate::verify(&read.meta, &[identity.public_key_bytes()]),
                crate::Verification::Trusted { .. }
            ));
        }
    }

    tilo_test_env::scenario! {
        fn tapp_archives_carry_verified_blobs_and_fail_closed;

        phase a_full_archive_carries_each_distinct_blob_once;
        {
            let (_dir, packed) = packed();
            let bytes = to_bytes(&packed.meta, Some(&packed)).unwrap();
            let mut blobs = BTreeMap::new();
            let tapp = read(bytes.as_slice(), &mut blobs).unwrap();
            assert_eq!(tapp.meta, packed.meta);
            assert!(tapp.is_complete());
            assert_eq!(blobs.len(), 2, "two files share one blob");
            assert_eq!(tapp.carried.values().sum::<u64>(), 50_005);
        }

        phase a_thin_archive_is_metadata_only;
        {
            let (_dir, packed) = packed();
            let thin = to_bytes(&packed.meta, None).unwrap();
            let full = to_bytes(&packed.meta, Some(&packed)).unwrap();
            assert!(
                thin.len() < 8 * 1024,
                "thin archive is {} bytes",
                thin.len()
            );
            assert!(full.len() > 50_000);
            let tapp = read(thin.as_slice(), &mut DiscardBlobs).unwrap();
            assert_eq!(tapp.meta, packed.meta);
            assert_eq!(tapp.missing().len(), 2);
        }

        phase a_sink_can_refuse_on_the_metadata_before_any_blob;
        {
            struct Refuse {
                blobs: usize,
            }
            impl BlobSink for Refuse {
                fn metadata(&mut self, _meta: &PackageMeta) -> io::Result<()> {
                    Err(io::Error::new(io::ErrorKind::PermissionDenied, "untrusted"))
                }
                fn accept(&mut self, _: &BlobHash, _: u64, _: &mut dyn Read) -> io::Result<()> {
                    self.blobs += 1;
                    Ok(())
                }
            }
            let (_dir, packed) = packed();
            let bytes = to_bytes(&packed.meta, Some(&packed)).unwrap();
            let mut sink = Refuse { blobs: 0 };
            assert!(read(bytes.as_slice(), &mut sink).is_err());
            assert_eq!(sink.blobs, 0, "no payload reaches a sink that refused");
        }

        phase a_blob_that_does_not_match_its_name_is_refused;
        {
            let (_dir, packed) = packed();
            let mut bytes = to_bytes(&packed.meta, Some(&packed)).unwrap();
            // The README's five bytes are unique in the archive; flip one.
            let at = bytes
                .windows(5)
                .rposition(|window| window == b"hello")
                .unwrap();
            bytes[at] = b'j';
            let error = read(bytes.as_slice(), &mut DiscardBlobs).unwrap_err();
            assert!(matches!(error, TappError::BlobHashMismatch(_)), "{error}");
        }

        phase a_blob_before_the_metadata_is_out_of_order;
        {
            let mut tar = tar::Builder::new(Vec::new());
            append(
                &mut tar,
                &format!("{BLOB_PREFIX}{}", BlobHash::of(b"x")),
                1,
                &b"x"[..],
            )
            .unwrap();
            let bytes = tar.into_inner().unwrap();
            let error = read(bytes.as_slice(), &mut DiscardBlobs).unwrap_err();
            assert!(matches!(error, TappError::MetadataAfterBlobs(_)), "{error}");
        }

        phase empty_input_is_refused_without_a_panic;
        {
            assert!(read(&[][..], &mut DiscardBlobs).is_err());
            assert!(crate::SignedRoot::decode(&[]).is_err());
        }
    }

    fn deterministic_app(payload_len: usize) -> (tempfile::TempDir, crate::Packed) {
        let identity = SigningIdentity::from_seed(&[0x11; 32]);
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("payload");
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::write(payload.join("bin/app"), vec![0x42; payload_len]).unwrap();
        let manifest = Manifest::from_toml_str(&format!(
            r#"
[package]
id = "so.tilo.fixture"
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
        let packed = pack(&payload, &manifest, &identity).unwrap();
        (dir, packed)
    }

    fn entries_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    tilo_test_env::scenario! {
        fn write_file_enforces_limits_verifies_and_keeps_exact_bytes;

        phase valid_full_and_thin_archives_match_to_bytes_byte_for_byte;
        {
            let (_src, packed) = deterministic_app(1024);
            let out_dir = tempfile::tempdir().unwrap();

            let full_path = out_dir.path().join("fixture.tapp");
            let expected_full = to_bytes(&packed.meta, Some(&packed)).unwrap();
            let written = write_file(
                &full_path,
                &packed.meta,
                Some(&packed),
                WriteOptions::create_new(),
            )
            .unwrap();
            assert_eq!(written.id, packed.meta.id);
            assert_eq!(written.bytes, expected_full.len() as u64);
            let on_disk = std::fs::read(&full_path).unwrap();
            assert_eq!(on_disk, expected_full, "full archive bytes must not change");
            assert_eq!(entries_in(out_dir.path()), ["fixture.tapp"]);

            let thin_path = out_dir.path().join("fixture-thin.tapp");
            let expected_thin = to_bytes(&packed.meta, None).unwrap();
            let written_thin =
                write_file(&thin_path, &packed.meta, None, WriteOptions::replace()).unwrap();
            assert_eq!(written_thin.id, packed.meta.id);
            assert_eq!(written_thin.bytes, expected_thin.len() as u64);
            assert_eq!(
                std::fs::read(&thin_path).unwrap(),
                expected_thin,
                "thin archive bytes must not change"
            );
        }

        phase existing_output_rule_is_honored_on_replace_and_refuse;
        {
            let (_src, packed) = deterministic_app(256);
            let out_dir = tempfile::tempdir().unwrap();
            let target = out_dir.path().join("app.tapp");
            std::fs::write(&target, b"existing release").unwrap();

            let refused = write_file(
                &target,
                &packed.meta,
                Some(&packed),
                WriteOptions::create_new(),
            )
            .unwrap_err();
            assert!(
                matches!(
                    refused,
                    TappError::Io(ref error) if error.kind() == io::ErrorKind::AlreadyExists
                ),
                "expected AlreadyExists, got {refused}"
            );
            assert_eq!(std::fs::read(&target).unwrap(), b"existing release");
            assert_eq!(entries_in(out_dir.path()), ["app.tapp"]);

            let written = write_file(
                &target,
                &packed.meta,
                Some(&packed),
                WriteOptions::replace(),
            )
            .unwrap();
            assert_eq!(
                std::fs::read(&target).unwrap(),
                to_bytes(&packed.meta, Some(&packed)).unwrap()
            );
            assert_eq!(written.id, packed.meta.id);
            assert_eq!(entries_in(out_dir.path()), ["app.tapp"]);
        }

        phase oversized_app_is_refused_and_leaves_nothing_behind;
        {
            let oversized = usize::try_from(crate::MAX_APP_TAPP_BYTES).unwrap();
            let (_src, packed) = deterministic_app(oversized);
            let out_dir = tempfile::tempdir().unwrap();

            // Absent destination: nothing created, no temporary files left.
            let fresh = out_dir.path().join("fresh.tapp");
            let error = write_file(
                &fresh,
                &packed.meta,
                Some(&packed),
                WriteOptions::create_new(),
            )
            .unwrap_err();
            assert!(
                matches!(
                    error,
                    TappError::PackageTooLarge { limit, .. } if limit == crate::MAX_APP_TAPP_BYTES
                ),
                "{error}"
            );
            assert!(entries_in(out_dir.path()).is_empty());

            // Existing destination with Replace: prior file preserved, no temp files left.
            let prior = out_dir.path().join("prior.tapp");
            std::fs::write(&prior, b"previous").unwrap();
            let error = write_file(
                &prior,
                &packed.meta,
                Some(&packed),
                WriteOptions::replace(),
            )
            .unwrap_err();
            assert!(matches!(error, TappError::PackageTooLarge { .. }), "{error}");
            assert_eq!(std::fs::read(&prior).unwrap(), b"previous");
            assert_eq!(entries_in(out_dir.path()), ["prior.tapp"]);

            // LocalBuild honors local_build_size_limit (64 MiB).
            let local = out_dir.path().join("local.tapp");
            let written = write_file(
                &local,
                &packed.meta,
                Some(&packed),
                WriteOptions::replace().local_build(),
            )
            .unwrap();
            assert!(written.bytes > crate::MAX_APP_TAPP_BYTES);
            assert!(local.is_file());

            // Non-app bundles (such as runtimes) are not subject to MAX_APP_TAPP_BYTES.
            let identity = SigningIdentity::from_seed(&[0x12; 32]);
            let runtime_src = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(runtime_src.path().join("bin")).unwrap();
            std::fs::write(
                runtime_src.path().join("bin/runtime"),
                vec![0x33; oversized],
            )
            .unwrap();
            let runtime_manifest = Manifest::from_toml_str(&format!(
                r#"
[package]
id = "so.tilo.fixture.runtime"
version = "1.0.0"
kind = "runtime"
provides = "tilo-runtime/2026.07"

[publisher]
identity = "ed25519:{}"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/runtime"
type = "cli"
"#,
                hex::encode(identity.public_key_bytes())
            ))
            .unwrap();
            let packed_runtime = pack(runtime_src.path(), &runtime_manifest, &identity).unwrap();
            let runtime_tapp = out_dir.path().join("runtime.tapp");
            let written_runtime = write_file(
                &runtime_tapp,
                &packed_runtime.meta,
                Some(&packed_runtime),
                WriteOptions::replace(),
            )
            .unwrap();
            assert!(written_runtime.bytes > crate::MAX_APP_TAPP_BYTES);
            assert!(runtime_tapp.is_file());
        }

        phase unverified_or_incomplete_archive_is_refused_and_leaves_nothing_behind;
        {
            let (_src, packed) = deterministic_app(128);
            let out_dir = tempfile::tempdir().unwrap();

            // Unsigned root fails signature verification and leaves no file.
            let unsigned_root = crate::SignedRoot {
                signature: None,
                ..packed.meta.root
            };
            let unsigned_meta = PackageMeta::from_root(
                unsigned_root,
                packed.meta.manifest_bytes.clone(),
                packed.meta.collection_bytes.clone(),
            )
            .unwrap();
            let target = out_dir.path().join("unsigned.tapp");
            let error = write_file(
                &target,
                &unsigned_meta,
                Some(&packed),
                WriteOptions::replace(),
            )
            .unwrap_err();
            assert!(matches!(error, TappError::VerificationFailed(_)), "{error}");
            assert!(entries_in(out_dir.path()).is_empty());

            // Missing payload blob in source fails and leaves no file.
            let empty_source: BTreeMap<BlobHash, Vec<u8>> = BTreeMap::new();
            let missing_target = out_dir.path().join("missing-blob.tapp");
            let error = write_file(
                &missing_target,
                &packed.meta,
                Some(&empty_source),
                WriteOptions::replace(),
            )
            .unwrap_err();
            assert!(matches!(error, TappError::MissingBlob(_)), "{error}");
            assert!(entries_in(out_dir.path()).is_empty());
        }
    }
}
