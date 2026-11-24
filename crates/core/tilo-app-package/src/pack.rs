//! `pack` — walk a payload directory, hash each file into the [`Collection`], encode the manifest and
//! build and sign the signed root. The payload bytes stay where they are; [`crate::signed`] has the
//! on-wire layout and what the publisher signs over.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ed25519_dalek::Signer;
use getrandom::{SysRng, rand_core::UnwrapErr};
use walkdir::WalkDir;

use collab_types::Ed25519PublicKey;

use crate::blobs::{BlobHash, BlobSource, Collection, OpenBlob};
use crate::id::PackageId;
use crate::manifest::Manifest;
use crate::package::{MetaError, PackageMeta};
use crate::signed::SignedRoot;
use crate::validate::{ManifestError, validate_manifest_with_payload};

/// A signing identity that can sign the package's signed-root bytes.
#[derive(Clone)]
pub enum SigningIdentity {
    Ed25519(ed25519_dalek::SigningKey),
}

impl SigningIdentity {
    pub fn generate() -> Self {
        let mut rng = UnwrapErr(SysRng);
        Self::Ed25519(ed25519_dalek::SigningKey::generate(&mut rng))
    }

    pub fn from_signing_key(sk: ed25519_dalek::SigningKey) -> Self {
        Self::Ed25519(sk)
    }

    /// The Ed25519 identity for a 32-byte secret-key seed.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self::Ed25519(ed25519_dalek::SigningKey::from_bytes(seed))
    }

    /// Load an Ed25519 signing identity from a publisher key file
    /// ([`registry_types::key_file`]).
    pub fn from_key_file(path: &Path) -> Result<Self, registry_types::key_file::KeyFileError> {
        registry_types::key_file::load(path).map(Self::from_signing_key)
    }

    /// Public-key bytes — used for the manifest's `publisher.identity`
    /// field and the signed-root's `publisher_identity` slot.
    pub fn public_key_bytes(&self) -> [u8; 32] {
        match self {
            Self::Ed25519(sk) => sk.verifying_key().to_bytes(),
        }
    }

    pub fn public_key(&self) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(self.public_key_bytes())
    }

    /// Sign the signing-bytes prefix of a [`SignedRoot`].
    pub fn sign_signed_root(&self, root: &SignedRoot) -> [u8; 64] {
        let preimage = root.signing_bytes();
        self.sign_message(&preimage)
    }

    /// Sign an arbitrary message with the identity's Ed25519 key.
    ///
    /// Used for out-of-band attestations binding to the same publisher
    /// identity as the package — e.g. a registry index entry vouching
    /// "this publisher's build of app X at version V has tapp
    /// content-hash H". Verification is a plain Ed25519 check against
    /// [`Self::public_key_bytes`].
    pub fn sign_message(&self, msg: &[u8]) -> [u8; 64] {
        match self {
            Self::Ed25519(sk) => sk.sign(msg).to_bytes(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("manifest validation: {0}")]
    Manifest(#[from] ManifestError),
    #[error("payload-dir walk failed: {0}")]
    Walk(#[from] walkdir::Error),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("packed metadata is inconsistent: {0}")]
    Meta(#[from] MetaError),
    #[error("manifest re-encode failed: {0}")]
    ManifestEncode(#[from] crate::manifest::ManifestParseError),
    #[error("publisher identity in manifest does not match signing identity")]
    IdentityMismatch,
    #[error("payload path {0:?} is not valid UTF-8 — Tilo packages require UTF-8 path names")]
    NonUtf8Path(PathBuf),
    #[error("payload path {0:?} is reserved for a file the packer writes")]
    ReservedPath(String),
}

/// A signed package built from a payload directory: its metadata, plus
/// where each payload blob's bytes are on disk. Nothing is read into
/// memory; files are hashed as they stream past and read again only when
/// the package is written out ([`crate::tapp::write`]) or installed.
#[derive(Clone, Debug)]
pub struct Packed {
    pub meta: PackageMeta,
    files: BTreeMap<BlobHash, PathBuf>,
    inline: BTreeMap<BlobHash, Vec<u8>>,
}

impl Packed {
    pub fn id(&self) -> PackageId {
        self.meta.id
    }

    pub fn files(&self) -> impl Iterator<Item = (&BlobHash, &Path)> {
        self.files.iter().map(|(hash, path)| (hash, path.as_path()))
    }
}

impl BlobSource for Packed {
    fn open_blob(&self, hash: &BlobHash) -> std::io::Result<Option<OpenBlob<'_>>> {
        if let Some(bytes) = self.inline.get(hash) {
            return Ok(Some(OpenBlob {
                len: bytes.len() as u64,
                reader: Box::new(bytes.as_slice()),
            }));
        }
        let Some(path) = self.files.get(hash) else {
            return Ok(None);
        };
        let file = std::fs::File::open(path)?;
        Ok(Some(OpenBlob {
            len: file.metadata()?.len(),
            reader: Box::new(file),
        }))
    }
}

struct Walked {
    collection: Collection,
    files: BTreeMap<BlobHash, PathBuf>,
    inline: BTreeMap<BlobHash, Vec<u8>>,
    paths: Vec<PathBuf>,
}

/// Walk `payload_dir` and hash every regular file, then add the generated
/// `extra` files, which must not collide with a walked path.
fn walk_payload(payload_dir: &Path, extra: Vec<(String, Vec<u8>)>) -> Result<Walked, PackError> {
    let mut entries = Vec::new();
    let mut files = BTreeMap::new();
    let mut paths = Vec::new();
    for entry in WalkDir::new(payload_dir).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let abs = entry.path();
        let rel = abs
            .strip_prefix(payload_dir)
            .map_err(|_| {
                PackError::Io(std::io::Error::other(format!(
                    "walkdir entry {} is not under payload_dir {}",
                    abs.display(),
                    payload_dir.display()
                )))
            })?
            .to_path_buf();
        let rel_str = rel
            .to_str()
            .ok_or_else(|| PackError::NonUtf8Path(rel.clone()))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        let (hash, _len) = BlobHash::of_reader(&mut std::fs::File::open(abs)?)?;
        files.entry(hash).or_insert_with(|| abs.to_path_buf());
        entries.push((rel_str, hash));
        paths.push(rel);
    }
    let mut inline = BTreeMap::new();
    for (rel, bytes) in extra {
        if entries.iter().any(|(walked, _)| *walked == rel) {
            return Err(PackError::ReservedPath(rel));
        }
        let hash = BlobHash::of(&bytes);
        paths.push(PathBuf::from(&rel));
        entries.push((rel, hash));
        inline.insert(hash, bytes);
    }
    Ok(Walked {
        collection: Collection::from_entries(entries),
        files,
        inline,
        paths,
    })
}

/// Everything but the signature: the payload index and the manifest blob a
/// root is built from.
struct Prepared {
    /// The root to sign, with a placeholder signature.
    root: SignedRoot,
    manifest_bytes: Vec<u8>,
    collection_bytes: Vec<u8>,
    files: BTreeMap<BlobHash, PathBuf>,
    inline: BTreeMap<BlobHash, Vec<u8>>,
}

impl Prepared {
    fn finish(self, signature: [u8; 64]) -> Result<Packed, PackError> {
        let root = SignedRoot {
            signature: Some(signature),
            ..self.root
        };
        let meta = PackageMeta::from_root(root, self.manifest_bytes, self.collection_bytes)?;
        Ok(Packed {
            meta,
            files: self.files,
            inline: self.inline,
        })
    }
}

fn prepare(
    payload_dir: &Path,
    manifest: &Manifest,
    publisher_identity: [u8; 32],
    extra: Vec<(String, Vec<u8>)>,
) -> Result<Prepared, PackError> {
    let declared = manifest
        .publisher
        .identity_bytes()
        .map_err(|e| PackError::Manifest(ManifestError::BadPublisherIdentity(e.to_string())))?;
    if declared != publisher_identity {
        return Err(PackError::IdentityMismatch);
    }
    let walked = walk_payload(payload_dir, extra)?;
    validate_manifest_with_payload(manifest, &walked.paths)?;
    let collection_bytes = walked.collection.encode();
    let mut to_sign_manifest = manifest.clone();
    to_sign_manifest.publisher.signature.clear();
    let manifest_bytes = to_sign_manifest.to_toml_string()?.into_bytes();
    let root = SignedRoot {
        format: crate::TAPP_FORMAT_VERSION,
        manifest_hash: BlobHash::of(&manifest_bytes),
        payload_root_hash: BlobHash::of(&collection_bytes),
        publisher_identity,
        // The signed prefix includes `has_signature = 1`, so the root is
        // signed with a placeholder in place; see `crate::signed`.
        signature: Some([0u8; 64]),
    };
    Ok(Prepared {
        root,
        manifest_bytes,
        collection_bytes,
        files: walked.files,
        inline: walked.inline,
    })
}

/// Pack a payload directory and manifest into a signed package.
///
/// 1. Check the manifest's publisher is `identity`, validate the manifest
///    alone and against the payload paths.
/// 2. Hash every payload file into the [`Collection`].
/// 3. Encode the manifest with an empty signature field.
/// 4. Build the [`SignedRoot`] over both hashes and sign it.
///
/// # Errors
///
/// Returns [`PackError::IdentityMismatch`] if the manifest's declared
/// publisher identity does not match `identity`'s public key,
/// [`PackError::Manifest`] if manifest validation (standalone or against the
/// payload) fails, [`PackError::Walk`] or [`PackError::Io`] if walking or
/// reading `payload_dir` fails, [`PackError::ManifestEncode`] if re-encoding
/// the manifest fails.
pub fn pack(
    payload_dir: &Path,
    manifest: &Manifest,
    identity: &SigningIdentity,
) -> Result<Packed, PackError> {
    pack_with_files(payload_dir, manifest, identity, Vec::new())
}

/// [`pack`] for a development build of `app` made by the device whose key is `identity`: the
/// package carries the app's own manifest, renamed to the id its dev marking derives, marked as
/// that device's build of `app`, and signed by that device. It installs only on that device
/// (`package_store::installer::install_dev`) and runs apart from the installed app.
///
/// It keeps nothing the app's published ids name (`previously`), so a dev build is never taken
/// for the app it builds.
///
/// # Errors
///
/// See [`pack`].
pub fn pack_dev(
    payload_dir: &Path,
    manifest: &Manifest,
    app: &str,
    identity: &SigningIdentity,
) -> Result<Packed, PackError> {
    let device = identity.public_key_bytes();
    let key = format!("ed25519:{}", hex::encode(device));
    let mut manifest = manifest.clone();
    manifest.package.id = crate::manifest::dev_app_id(app, &device);
    manifest.package.previously.clear();
    manifest.package.dev = Some(crate::manifest::DevBuild {
        device: key.clone(),
        of: app.to_owned(),
    });
    manifest.publisher.identity = key;
    manifest.publisher.signature.clear();
    pack_with_files(payload_dir, &manifest, identity, Vec::new())
}

pub(crate) fn pack_with_files(
    payload_dir: &Path,
    manifest: &Manifest,
    identity: &SigningIdentity,
    extra: Vec<(String, Vec<u8>)>,
) -> Result<Packed, PackError> {
    let prepared = prepare(payload_dir, manifest, identity.public_key_bytes(), extra)?;
    let signature = identity.sign_signed_root(&prepared.root);
    prepared.finish(signature)
}

/// [`pack_with_files`] for `publisher`, whose key is not here: the root
/// carries the placeholder signature it is signed with
/// ([`sign_placeholder_root`] attaches the real one).
pub(crate) fn pack_with_files_for(
    payload_dir: &Path,
    manifest: &Manifest,
    publisher: [u8; 32],
    extra: Vec<(String, Vec<u8>)>,
) -> Result<Packed, PackError> {
    prepare(payload_dir, manifest, publisher, extra)?.finish([0u8; 64])
}

/// Sign a package packed for `identity` by someone without its key
/// ([`crate::image::seal_image_for`]): the root's placeholder signature is
/// replaced by `identity`'s, which changes the package id.
///
/// # Errors
///
/// Returns [`PackError::IdentityMismatch`] when the package names another
/// publisher, or what signing and reassembling the package return.
pub fn sign_placeholder_root(
    meta: &PackageMeta,
    identity: &SigningIdentity,
) -> Result<PackageMeta, PackError> {
    if meta.root.publisher_identity != identity.public_key_bytes() {
        return Err(PackError::IdentityMismatch);
    }
    let placeholder = SignedRoot {
        signature: Some([0u8; 64]),
        ..meta.root.clone()
    };
    let signature = identity.sign_signed_root(&placeholder);
    Ok(PackageMeta::from_root(
        SignedRoot {
            signature: Some(signature),
            ..placeholder
        },
        meta.manifest_bytes.clone(),
        meta.collection_bytes.clone(),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_identity_loads_from_key_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("publisher.json");
        let key = ed25519_dalek::SigningKey::from_bytes(&[0x33; 32]);
        std::fs::write(&path, registry_types::key_file::encode(&key)).unwrap();
        let loaded = SigningIdentity::from_key_file(&path).unwrap();
        assert_eq!(loaded.public_key_bytes(), key.verifying_key().to_bytes());
    }

    #[test]
    fn a_development_build_is_marked_and_signed_by_its_device() {
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("payload");
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::write(payload.join("bin/app"), b"program").unwrap();
        let identity = SigningIdentity::from_seed(&[0x42; 32]);
        let key = hex::encode(identity.public_key_bytes());
        let manifest = Manifest::from_toml_str(&format!(
            r#"[package]
id = "org.example.notes"
previously = ["org.example.old-notes"]
version = "0.1.0"
kind = "app"

[publisher]
identity = "ed25519:{key}"
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
"#
        ))
        .unwrap();
        let packed = pack_dev(&payload, &manifest, "org.example.notes", &identity).unwrap();
        let built = &packed.meta.manifest.package;
        assert_eq!(
            built.id,
            crate::manifest::dev_app_id("org.example.notes", &identity.public_key_bytes())
        );
        let dev = built.dev.as_ref().expect("the marking");
        assert_eq!(dev.of, "org.example.notes");
        assert_eq!(dev.device, format!("ed25519:{key}"));
        assert!(built.previously.is_empty(), "a dev build keeps no ids");
        assert_eq!(
            packed.meta.root.publisher_identity,
            identity.public_key_bytes()
        );
        assert_eq!(
            crate::verify(&packed.meta, &[identity.public_key_bytes()]),
            crate::Verification::Trusted {
                manifest: packed.meta.manifest.clone()
            }
        );
    }
}
