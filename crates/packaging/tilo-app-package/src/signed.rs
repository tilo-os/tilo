//! The "signed root" blob — the structure whose hash is the
//! [`crate::PackageId`].
//!
//! Layout:
//!
//! ```text
//! MAGIC  "TLOPKG\0\0"   (8 bytes)
//! u32 LE format_version = 1
//! [u8; 32] manifest_hash       (BLAKE3 of the manifest TOML bytes)
//! [u8; 32] payload_root_hash   (BLAKE3 of the encoded Collection)
//! u8       has_signature       (0 / 1)
//! [u8; 32] publisher_identity  (Ed25519 group public key bytes)
//! [u8; 64] signature           (zeroed if has_signature == 0)
//! ```
//!
//! Total fixed size: 8 + 4 + 32 + 32 + 1 + 32 + 64 = 173 bytes.
//!
//! The signed *message* bytes (what the publisher's Ed25519 key
//! signs over) cover the first 109 bytes — i.e. everything up to
//! and including `publisher_identity`. The signature is then
//! tacked on; the `has_signature` byte distinguishes an unsigned
//! tree-built root from a fully signed one.
//!
//! **Pack convention**: the publisher signs the bytes with
//! `has_signature = 1` and a placeholder zero signature. That
//! way [`crate::verify::verify`], which always decodes the
//! signed-root with the real `has_signature` byte set to 1,
//! reconstructs the exact same prefix the publisher signed.
//!
//! The hash of the full 173-byte blob (signed or unsigned) is the
//! `PackageId`. Note this means an unsigned root and the same
//! root with a real signature attached have **different**
//! `PackageId`s — which is the right thing: a consumer must
//! know whether they're verifying a signed or unsigned bundle,
//! and an attacker swapping in a forged signature would produce
//! a different `PackageId`.

use thiserror::Error;

use crate::blobs::BlobHash;

const MAGIC: &[u8; 8] = b"TLOPKG\0\0";
/// The signed root is part of the package format and carries its version.
const FORMAT_VERSION: u32 = crate::tapp::TAPP_FORMAT_VERSION;

pub const SIGNED_ROOT_LEN: usize = 8 + 4 + 32 + 32 + 1 + 32 + 64;
pub const SIGNING_PREFIX_LEN: usize = 8 + 4 + 32 + 32 + 1 + 32; // 109 bytes

/// The on-disk signed-root blob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedRoot {
    pub manifest_hash: BlobHash,
    pub payload_root_hash: BlobHash,
    pub publisher_identity: [u8; 32],
    /// `None` until `tilo-app-package sign` runs.
    pub signature: Option<[u8; 64]>,
}

impl SignedRoot {
    /// Build an unsigned root. Pack uses this to compute the
    /// preimage that sign attaches a signature to.
    pub fn unsigned(
        manifest_hash: BlobHash,
        payload_root_hash: BlobHash,
        publisher_identity: [u8; 32],
    ) -> Self {
        Self {
            manifest_hash,
            payload_root_hash,
            publisher_identity,
            signature: None,
        }
    }

    /// Bytes covered by the publisher's signature. Stable layout
    /// — see module docs.
    pub fn signing_bytes(&self) -> [u8; SIGNING_PREFIX_LEN] {
        let mut out = [0u8; SIGNING_PREFIX_LEN];
        out[0..8].copy_from_slice(MAGIC);
        out[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        out[12..44].copy_from_slice(&self.manifest_hash.0);
        out[44..76].copy_from_slice(&self.payload_root_hash.0);
        out[76] = u8::from(self.signature.is_some());
        out[77..109].copy_from_slice(&self.publisher_identity);
        out
    }

    /// Full encoded blob.
    pub fn encode(&self) -> [u8; SIGNED_ROOT_LEN] {
        let mut out = [0u8; SIGNED_ROOT_LEN];
        out[..SIGNING_PREFIX_LEN].copy_from_slice(&self.signing_bytes());
        if let Some(sig) = self.signature {
            out[SIGNING_PREFIX_LEN..].copy_from_slice(&sig);
        }
        out
    }

    /// Decode strict — magic, format version, `has_signature` bit
    /// all checked.
    ///
    /// # Errors
    ///
    /// Returns [`SignedRootError::WrongLength`] if `bytes` is not exactly
    /// [`SIGNED_ROOT_LEN`] bytes, [`SignedRootError::BadMagic`] if the
    /// header doesn't match, [`SignedRootError::UnsupportedFormat`] if the
    /// format version isn't `FORMAT_VERSION`,
    /// [`SignedRootError::BadHasSignatureFlag`] if the `has_signature` byte
    /// is neither 0 nor 1, and
    /// [`SignedRootError::NonZeroSignatureWhenUnsigned`] if it is 0 but the
    /// signature tail is not all-zero.
    ///
    /// # Panics
    ///
    /// Never in practice: every slice below has a fixed, compile-time-known
    /// offset within `SIGNED_ROOT_LEN`, and the length check above rejects
    /// any `bytes` that wouldn't fit them.
    #[allow(
        clippy::indexing_slicing,
        reason = "bytes.len() == SIGNED_ROOT_LEN is checked immediately \
                  above, so every fixed offset below (compile-time \
                  constants: 8, 12, 44, 76, 77, 109 = SIGNING_PREFIX_LEN, \
                  173 = SIGNED_ROOT_LEN) is provably in range"
    )]
    pub fn decode(bytes: &[u8]) -> Result<Self, SignedRootError> {
        if bytes.len() != SIGNED_ROOT_LEN {
            return Err(SignedRootError::WrongLength {
                expected: SIGNED_ROOT_LEN,
                got: bytes.len(),
            });
        }
        if &bytes[0..8] != MAGIC {
            return Err(SignedRootError::BadMagic);
        }
        let format = u32::from_le_bytes(bytes[8..12].try_into().unwrap_or([0; 4]));
        if format != FORMAT_VERSION {
            return Err(SignedRootError::UnsupportedFormat(format));
        }
        let mut manifest_hash = [0u8; 32];
        manifest_hash.copy_from_slice(&bytes[12..44]);
        let mut payload_root_hash = [0u8; 32];
        payload_root_hash.copy_from_slice(&bytes[44..76]);
        let has_sig = bytes[76];
        let mut publisher_identity = [0u8; 32];
        publisher_identity.copy_from_slice(&bytes[77..109]);
        let signature = match has_sig {
            0 => {
                // Sanity: signature tail must be zero.
                if bytes[SIGNING_PREFIX_LEN..].iter().any(|b| *b != 0) {
                    return Err(SignedRootError::NonZeroSignatureWhenUnsigned);
                }
                None
            }
            1 => {
                let mut sig = [0u8; 64];
                sig.copy_from_slice(&bytes[SIGNING_PREFIX_LEN..]);
                Some(sig)
            }
            _ => return Err(SignedRootError::BadHasSignatureFlag(has_sig)),
        };
        Ok(Self {
            manifest_hash: BlobHash(manifest_hash),
            payload_root_hash: BlobHash(payload_root_hash),
            publisher_identity,
            signature,
        })
    }

    /// The `PackageId` of this signed-root — BLAKE3 over the
    /// full encoded blob.
    pub fn package_id(&self) -> crate::id::PackageId {
        crate::id::PackageId::from_bytes(*blake3::hash(&self.encode()).as_bytes())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SignedRootError {
    #[error("signed-root length: expected {expected}, got {got}")]
    WrongLength { expected: usize, got: usize },
    #[error("missing or wrong magic header")]
    BadMagic,
    #[error("unsupported signed-root format version {0}")]
    UnsupportedFormat(u32),
    #[error("has_signature flag is {0} (expected 0 or 1)")]
    BadHasSignatureFlag(u8),
    #[error("has_signature=0 but signature tail is non-zero")]
    NonZeroSignatureWhenUnsigned,
}
