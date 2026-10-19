//! The `.tapp` archive: how a package travels as one file.
//!
//! A `.tapp` is a tar archive:
//!
//! ```text
//! tapp.toml          # header: package_id = "tapp:<hex>", format = 2
//! signed-root        # the 173-byte signed root (its hash is the package id)
//! manifest.toml      # the manifest the root commits to
//! collection.qcl     # the payload collection the root commits to
//! blobs/<hex>        # payload blobs, named by BLAKE3 hash (full form only)
//! ```
//!
//! It comes in two forms that verify identically:
//!
//! - **thin** — metadata only, typically a few KiB. The payload bytes are
//!   content like any other and are fetched from whoever holds them (the
//!   device's store, a peer, a registry), each checked against the
//!   collection.
//! - **full** — metadata plus every distinct payload blob, for offline
//!   transfer and for sources that cannot serve blobs.
//!
//! Both directions stream. [`write()`] reads payload bytes from a
//! [`BlobSource`] as it emits them, and [`read`] hands each carried blob to
//! a [`BlobSink`] as it arrives and checks the blob's hash after the sink
//! has consumed it. Metadata entries precede blobs, so the reader knows the
//! collection before the first payload byte and skips anything the package
//! does not reference.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use crate::blobs::{BlobHash, BlobSink, BlobSource, HashingReader};
use crate::id::PackageId;
use crate::package::{MetaError, PackageMeta};
use crate::signed::SIGNED_ROOT_LEN;

/// The store package format's one version: the archive layout, the
/// manifest it signs, the signed root and the chunk list. A device reads
/// only packages of exactly this version (docs/architecture/compatibility.md);
/// `interface.toml` holds the manifest and header schema, and the repository
/// policy fails a change to it that keeps the number. The binary layouts
/// (signed root, chunk list) are not in that schema: a change to them bumps
/// this by hand.
pub const TAPP_FORMAT_VERSION: u32 = registry_types::PACKAGE_FORMAT_VERSION;

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

/// A decoded archive: the package's metadata and which payload blobs the
/// archive carried.
#[derive(Clone, Debug)]
pub struct Tapp {
    pub meta: PackageMeta,
    /// Length of each payload blob the archive carried, all verified.
    pub carried: BTreeMap<BlobHash, u64>,
}

impl Tapp {
    /// Payload blobs the package references that the archive did not carry.
    pub fn missing(&self) -> Vec<BlobHash> {
        self.meta
            .payload_hashes()
            .into_iter()
            .filter(|hash| !self.carried.contains_key(hash))
            .collect()
    }

    /// Whether the archive carried every payload blob.
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
    #[error("unsupported tapp format {0}; expected {TAPP_FORMAT_VERSION}")]
    UnsupportedFormat(u32),
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
        if header.format != TAPP_FORMAT_VERSION {
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
///
/// Every carried blob is verified against its name after `sink` consumed
/// it; blobs the collection does not reference, and repeats, are skipped
/// unread by the sink. The archive may be thin: check [`Tapp::missing`].
///
/// # Errors
///
/// Returns a [`TappError`] for a malformed archive, broken metadata links,
/// a blob that does not match its name, or an error from `sink`.
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
                let finished = ready
                    .finish()
                    .map_err(|_| TappError::MetadataAfterBlobs(name.clone()))?;
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
        format: TAPP_FORMAT_VERSION,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blobs::DiscardBlobs;
    use crate::{Manifest, SigningIdentity, pack};

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

    #[test]
    fn tapp_archives_carry_verified_blobs_and_fail_closed() {
        // phase: a_full_archive_carries_each_distinct_blob_once
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
        // phase: a_thin_archive_is_metadata_only
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
        // phase: a_sink_can_refuse_on_the_metadata_before_any_blob
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
        // phase: a_blob_that_does_not_match_its_name_is_refused
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
        // phase: an_unsupported_format_fails_closed
        {
            let (_dir, packed) = packed();
            let mut bytes = to_bytes(&packed.meta, None).unwrap();
            let marker = b"format = 2\n";
            let at = bytes
                .windows(marker.len())
                .position(|window| window == marker)
                .unwrap();
            bytes[at + marker.len() - 2] = b'1';
            let error = read(bytes.as_slice(), &mut DiscardBlobs).unwrap_err();
            assert!(matches!(error, TappError::UnsupportedFormat(1)), "{error}");
        }
    }
}
