//! Verifying a downloaded tapp against the version selected for it.
//!
//! The archive layout, signed root and collection encodings are those of
//! `tilo-app-package` (`tapp.rs`, `signed.rs`, `blobs.rs`); this is an
//! independent reader of the same bytes with no file system, cross-checked
//! against that crate in the tests.

use std::collections::BTreeMap;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::{Selection, VerifyError};

const HEADER: &str = "tapp.toml";
const SIGNED_ROOT: &str = "signed-root";
const MANIFEST: &str = "manifest.toml";
const COLLECTION: &str = "collection.qcl";
const BLOB_PREFIX: &str = "blobs/";

const MAX_HEADER_BYTES: usize = 4 * 1024;
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_COLLECTION_BYTES: usize = 16 * 1024 * 1024;

const TAPP_FORMAT: u32 = 1;
const ROOT_MAGIC: &[u8; 8] = b"TLOPKG\0\0";
const ROOT_FORMAT: [u8; 4] = 1u32.to_le_bytes();
const SIGNED_ROOT_LEN: usize = 173;
const SIGNING_PREFIX_LEN: usize = 109;
const COLLECTION_MAGIC: &[u8; 8] = b"TLOCOL\0\0";

const BLOCK: usize = 512;

/// What the verified package is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageKind {
    /// An app side module.
    App,
    /// A toolkit core.
    Runtime,
}

/// One payload file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadFile {
    pub path: String,
    pub blake3_hex: String,
}

/// A tapp whose signature and every hash link checked out against its
/// [`Selection`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedPackage {
    pub package_id: String,
    pub version: String,
    pub kind: PackageKind,
    pub package_root_hex: String,
    /// The runtime ABI id the module links against (app) or provides
    /// (runtime).
    pub runtime: String,
    /// The WebAssembly module the loader instantiates.
    pub module: PayloadFile,
    /// Files under `assets/`, named by their path below it.
    pub assets: Vec<PayloadFile>,
    /// Every payload file, in path order.
    pub files: Vec<PayloadFile>,
}

/// Verify a thin or full tapp against the version selected for it. Blobs
/// a full archive carries are not read; fetch and [`verify_blob`] them.
///
/// # Errors
///
/// Returns the first failed check as a [`VerifyError`].
pub fn verify_package(bytes: &[u8], selection: &Selection) -> Result<VerifiedPackage, VerifyError> {
    let parts = Parts::read(bytes)?;
    let root_hex = blake3_hex(parts.signed_root);
    if root_hex != selection.package_root_hex {
        return Err(VerifyError::RootMismatch {
            expected: selection.package_root_hex.clone(),
            found: root_hex,
        });
    }
    check_header(parts.header, &root_hex)?;
    let root = SignedRoot::decode(parts.signed_root)?;
    let publisher_hex = hex::encode(root.publisher);
    if !publisher_hex.eq_ignore_ascii_case(&selection.publisher_pubkey_hex) {
        return Err(VerifyError::PublisherMismatch {
            expected: selection.publisher_pubkey_hex.clone(),
            found: publisher_hex,
        });
    }
    root.verify_signature()?;
    if blake3::hash(parts.manifest).as_bytes() != root.manifest_hash {
        return Err(VerifyError::HashLink(MANIFEST));
    }
    if blake3::hash(parts.collection).as_bytes() != root.collection_hash {
        return Err(VerifyError::HashLink(COLLECTION));
    }
    let collection = decode_collection(parts.collection)?;
    let manifest = std::str::from_utf8(parts.manifest)
        .map_err(|_| VerifyError::Manifest("not UTF-8".into()))
        .and_then(|text| {
            toml::from_str::<Manifest>(text)
                .map_err(|error| VerifyError::Manifest(error.to_string()))
        })?;
    let kind = check_manifest(&manifest, selection)?;
    let files: Vec<PayloadFile> = collection
        .iter()
        .map(|(path, hash)| PayloadFile {
            path: path.clone(),
            blake3_hex: hex::encode(hash),
        })
        .collect();
    let module_path = manifest
        .target
        .as_ref()
        .map(|target| target.module.as_str())
        .unwrap_or_default();
    let module = files
        .iter()
        .find(|file| file.path == module_path)
        .cloned()
        .ok_or(VerifyError::ManifestMismatch(
            "the module is not in the payload",
        ))?;
    let assets = files
        .iter()
        .filter_map(|file| {
            file.path.strip_prefix("assets/").map(|name| PayloadFile {
                path: name.to_string(),
                blake3_hex: file.blake3_hex.clone(),
            })
        })
        .collect();
    Ok(VerifiedPackage {
        package_id: manifest.package.id,
        version: manifest.package.version,
        kind,
        package_root_hex: root_hex,
        runtime: selection.target.runtime.clone().unwrap_or_default(),
        module,
        assets,
        files,
    })
}

/// Whether `bytes` hash to `blake3_hex`.
///
/// # Errors
///
/// Returns [`VerifyError::BlobMismatch`] otherwise.
pub fn verify_blob(bytes: &[u8], expected_hex: &str) -> Result<(), VerifyError> {
    if blake3_hex(bytes).eq_ignore_ascii_case(expected_hex) {
        Ok(())
    } else {
        Err(VerifyError::BlobMismatch(expected_hex.to_string()))
    }
}

/// Lower-case hex BLAKE3 of `bytes`.
pub fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

struct Parts<'a> {
    header: &'a [u8],
    signed_root: &'a [u8],
    manifest: &'a [u8],
    collection: &'a [u8],
}

impl<'a> Parts<'a> {
    fn read(bytes: &'a [u8]) -> Result<Self, VerifyError> {
        let mut slots: [Option<&'a [u8]>; 4] = [None; 4];
        for entry in TarEntries::new(bytes) {
            let (name, data) = entry?;
            if name.starts_with(BLOB_PREFIX) {
                continue;
            }
            let (slot, limit, label) = match name.as_str() {
                HEADER => (0, MAX_HEADER_BYTES, HEADER),
                SIGNED_ROOT => (1, SIGNED_ROOT_LEN, SIGNED_ROOT),
                MANIFEST => (2, MAX_MANIFEST_BYTES, MANIFEST),
                COLLECTION => (3, MAX_COLLECTION_BYTES, COLLECTION),
                _ => continue,
            };
            if data.len() > limit {
                return Err(VerifyError::EntryTooLarge {
                    name: label.to_string(),
                    limit,
                });
            }
            let Some(cell) = slots.get_mut(slot) else {
                continue;
            };
            if cell.replace(data).is_some() {
                return Err(VerifyError::DuplicateEntry(label.to_string()));
            }
        }
        let [header, signed_root, manifest, collection] = slots;
        Ok(Self {
            header: header.ok_or(VerifyError::MissingEntry(HEADER))?,
            signed_root: signed_root.ok_or(VerifyError::MissingEntry(SIGNED_ROOT))?,
            manifest: manifest.ok_or(VerifyError::MissingEntry(MANIFEST))?,
            collection: collection.ok_or(VerifyError::MissingEntry(COLLECTION))?,
        })
    }
}

#[derive(Deserialize)]
struct Header {
    package_id: String,
    format: u32,
}

fn check_header(bytes: &[u8], root_hex: &str) -> Result<(), VerifyError> {
    let header: Header = std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| toml::from_str(text).ok())
        .ok_or_else(|| VerifyError::Archive("unreadable tapp.toml".into()))?;
    if header.format != TAPP_FORMAT {
        return Err(VerifyError::Archive(format!(
            "unsupported tapp format {}",
            header.format
        )));
    }
    if header.package_id != format!("tapp:{root_hex}") {
        return Err(VerifyError::Archive(
            "tapp.toml names another package than its signed root".into(),
        ));
    }
    Ok(())
}

struct SignedRoot<'a> {
    prefix: &'a [u8],
    manifest_hash: &'a [u8],
    collection_hash: &'a [u8],
    publisher: [u8; 32],
    signature: [u8; 64],
}

impl<'a> SignedRoot<'a> {
    /// Layout: magic (8), format u32 LE (4), manifest hash (32), collection
    /// hash (32), `has_signature` (1), publisher key (32), signature (64).
    /// The signature covers the first 109 bytes; a registry version must be
    /// signed.
    fn decode(bytes: &'a [u8]) -> Result<Self, VerifyError> {
        if bytes.len() != SIGNED_ROOT_LEN {
            return Err(VerifyError::SignedRoot("wrong length"));
        }
        let field = |from: usize, to: usize| bytes.get(from..to).unwrap_or_default();
        if field(0, 8) != ROOT_MAGIC || field(8, 12) != ROOT_FORMAT {
            return Err(VerifyError::SignedRoot("unknown magic or format"));
        }
        if field(76, 77) != [1] {
            return Err(VerifyError::SignedRoot("the package is not signed"));
        }
        Ok(Self {
            prefix: field(0, SIGNING_PREFIX_LEN),
            manifest_hash: field(12, 44),
            collection_hash: field(44, 76),
            publisher: field(77, SIGNING_PREFIX_LEN)
                .try_into()
                .map_err(|_| VerifyError::SignedRoot("wrong length"))?,
            signature: field(SIGNING_PREFIX_LEN, SIGNED_ROOT_LEN)
                .try_into()
                .map_err(|_| VerifyError::SignedRoot("wrong length"))?,
        })
    }

    fn verify_signature(&self) -> Result<(), VerifyError> {
        let key = VerifyingKey::from_bytes(&self.publisher)
            .map_err(|_| VerifyError::SignedRoot("the publisher key is not an Ed25519 point"))?;
        key.verify(self.prefix, &Signature::from_bytes(&self.signature))
            .map_err(|_| VerifyError::Signature)
    }
}

/// `TLOCOL\0\0`, u32 LE count, then per entry in path order: u32 LE path
/// length, UTF-8 path, 32-byte BLAKE3.
fn decode_collection(bytes: &[u8]) -> Result<BTreeMap<String, [u8; 32]>, VerifyError> {
    if bytes.get(..8) != Some(COLLECTION_MAGIC.as_slice()) {
        return Err(VerifyError::Collection("bad magic"));
    }
    let mut cursor = Cursor { bytes, at: 8 };
    let count = cursor.u32()?;
    let mut entries = BTreeMap::new();
    for _ in 0..count {
        let len = cursor.u32()?;
        let path = std::str::from_utf8(cursor.take(len)?)
            .map_err(|_| VerifyError::Collection("path is not UTF-8"))?
            .to_string();
        if !is_safe_path(&path) {
            return Err(VerifyError::Collection("unsafe path"));
        }
        let hash: [u8; 32] = cursor
            .take(32)?
            .try_into()
            .map_err(|_| VerifyError::Collection("truncated"))?;
        if entries.insert(path, hash).is_some() {
            return Err(VerifyError::Collection("duplicate path"));
        }
    }
    if cursor.at != bytes.len() {
        return Err(VerifyError::Collection("trailing bytes"));
    }
    Ok(entries)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], VerifyError> {
        let slice = self
            .at
            .checked_add(len)
            .and_then(|end| self.bytes.get(self.at..end))
            .ok_or(VerifyError::Collection("truncated"))?;
        self.at = self.at.saturating_add(len);
        Ok(slice)
    }

    fn u32(&mut self) -> Result<usize, VerifyError> {
        let array: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| VerifyError::Collection("truncated"))?;
        usize::try_from(u32::from_le_bytes(array)).map_err(|_| VerifyError::Collection("truncated"))
    }
}

fn is_safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

#[derive(Deserialize)]
struct Manifest {
    package: ManifestPackage,
    target: Option<ManifestTarget>,
}

#[derive(Deserialize)]
struct ManifestPackage {
    id: String,
    version: String,
    kind: String,
    runtime: Option<String>,
}

#[derive(Deserialize)]
struct ManifestTarget {
    platform: String,
    architecture: String,
    isa: String,
    runtime: String,
    module: String,
}

fn check_manifest(manifest: &Manifest, selection: &Selection) -> Result<PackageKind, VerifyError> {
    let package = &manifest.package;
    if package.id != selection.package_id {
        return Err(VerifyError::ManifestMismatch("package id"));
    }
    if package.version != selection.version {
        return Err(VerifyError::ManifestMismatch("version"));
    }
    let target = manifest
        .target
        .as_ref()
        .ok_or(VerifyError::ManifestMismatch(
            "the manifest has no [target]",
        ))?;
    let signed = &selection.target;
    if target.platform != signed.platform
        || target.architecture != signed.architecture
        || target.isa != signed.isa
        || Some(&target.runtime) != signed.runtime.as_ref()
    {
        return Err(VerifyError::ManifestMismatch("[target]"));
    }
    let runtime = registry_types::parse_web_runtime(&target.runtime)
        .map_err(|_| VerifyError::ManifestMismatch("[target].runtime"))?;
    match package.kind.as_str() {
        "app" if package.runtime.as_deref() == Some(runtime.release().as_str()) => {
            Ok(PackageKind::App)
        }
        "app" => Err(VerifyError::ManifestMismatch("package.runtime")),
        "runtime" => Ok(PackageKind::Runtime),
        _ => Err(VerifyError::ManifestMismatch("package kind")),
    }
}

/// Regular-file entries of a ustar/GNU tar archive, with GNU long names.
struct TarEntries<'a> {
    bytes: &'a [u8],
    at: usize,
    long_name: Option<String>,
    done: bool,
}

impl<'a> TarEntries<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            at: 0,
            long_name: None,
            done: false,
        }
    }

    fn next_entry(&mut self) -> Result<Option<(String, &'a [u8])>, VerifyError> {
        loop {
            if self.at == self.bytes.len() {
                return Ok(None);
            }
            let header_end = self.at.checked_add(BLOCK).ok_or_else(truncated)?;
            let header = self.bytes.get(self.at..header_end).ok_or_else(truncated)?;
            if header.iter().all(|byte| *byte == 0) {
                return Ok(None);
            }
            check_header_checksum(header)?;
            let size = parse_octal(header.get(124..136).unwrap_or_default())?;
            let data_end = header_end.checked_add(size).ok_or_else(truncated)?;
            let data = self.bytes.get(header_end..data_end).ok_or_else(truncated)?;
            let padded = size.checked_next_multiple_of(BLOCK).ok_or_else(truncated)?;
            self.at = header_end.checked_add(padded).ok_or_else(truncated)?;
            match header.get(156).copied().unwrap_or_default() {
                b'L' => {
                    self.long_name = Some(c_string(data));
                }
                b'0' | 0 => {
                    let name = match self.long_name.take() {
                        Some(name) => name,
                        None => entry_name(header),
                    };
                    return Ok(Some((name, data)));
                }
                _ => {
                    self.long_name = None;
                }
            }
        }
    }
}

impl<'a> Iterator for TarEntries<'a> {
    type Item = Result<(String, &'a [u8]), VerifyError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let next = self.next_entry().transpose();
        if !matches!(next, Some(Ok(_))) {
            self.done = true;
        }
        next
    }
}

fn truncated() -> VerifyError {
    VerifyError::Archive("truncated or malformed tar".into())
}

fn entry_name(header: &[u8]) -> String {
    let name = c_string(header.get(..100).unwrap_or_default());
    let prefix = if header.get(257..262) == Some(b"ustar".as_slice()) {
        c_string(header.get(345..500).unwrap_or_default())
    } else {
        String::new()
    };
    if prefix.is_empty() {
        name
    } else {
        format!("{prefix}/{name}")
    }
}

fn c_string(field: &[u8]) -> String {
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    String::from_utf8_lossy(field.get(..end).unwrap_or_default()).into_owned()
}

fn parse_octal(field: &[u8]) -> Result<usize, VerifyError> {
    let mut value = 0usize;
    for byte in field
        .iter()
        .copied()
        .skip_while(|byte| *byte == b' ')
        .take_while(|byte| *byte != 0 && *byte != b' ')
    {
        if !(b'0'..=b'7').contains(&byte) {
            return Err(truncated());
        }
        value = value
            .checked_mul(8)
            .and_then(|value| value.checked_add(usize::from(byte.wrapping_sub(b'0'))))
            .ok_or_else(truncated)?;
    }
    Ok(value)
}

fn check_header_checksum(header: &[u8]) -> Result<(), VerifyError> {
    let expected = parse_octal(header.get(148..156).unwrap_or_default())?;
    let actual = header
        .iter()
        .enumerate()
        .map(|(at, byte)| {
            if (148..156).contains(&at) {
                usize::from(b' ')
            } else {
                usize::from(*byte)
            }
        })
        .fold(0usize, usize::wrapping_add);
    if actual == expected {
        Ok(())
    } else {
        Err(VerifyError::Archive("tar header checksum mismatch".into()))
    }
}
