//! Signed desired state for optional image extensions.
//!
//! Tilo's root is a signed, immutable bootc/composefs deployment. Optional
//! kernel modules, GPU stacks, and local-model runtimes must therefore not be
//! installed by an arbitrary package manager into that root. This crate is the
//! deliberately small policy seam between a catalogue/downloader and the image
//! updater:
//!
//! * a manifest is signed by an explicitly trusted release key;
//! * the artifact and the base image are content-addressed pins;
//! * a manifest targets one image digest (and, optionally, an architecture or
//!   kernel ABI), so an extension cannot silently survive an incompatible base
//!   update;
//! * state changes use a durable, atomic two-phase transaction; and
//! * this crate never downloads, mounts, loads a module, invokes a package
//!   manager, or changes a boot deployment.
//!
//! A privileged image builder may consume a committed [`AcceptedExtension`]
//! set on its next deployment. It must still verify the artifact independently.
//! A crash with a pending transaction is inert and fails closed: the previous
//! accepted set stays active until the transaction is explicitly completed or
//! aborted.

#![allow(
    clippy::missing_errors_doc,
    reason = "the compact public API is documented as one transaction protocol above; every fallible method returns one of the explicit ExtensionError variants"
)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Wire/schema version for manifests and desired state.
pub const SCHEMA_VERSION: u16 = 1;
// Frozen signing bytes retain their historical prefix across product renames.
const SIGNING_DOMAIN: &str = "tilo.system-extension.manifest.v1";
const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;

/// Hardware-neutral extension family. The kind is UX/policy metadata; it is
/// not a permission and never changes what the manifest may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionKind {
    KernelModule,
    GpuDriverStack,
    LocalModelRuntime,
    Other,
}

/// A content-addressed artifact. `source` is only a retrieval hint; its hash
/// and declared byte length are the identity the updater must enforce.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedArtifact {
    pub source: String,
    /// Lowercase hexadecimal SHA-256 with no algorithm prefix.
    pub sha256: String,
    pub size_bytes: u64,
}

/// Exact compatibility constraints for the immutable base deployment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageConstraint {
    /// OCI image manifest digest, always `sha256:<64 lowercase hex digits>`.
    pub base_image_digest: String,
    /// Optional exact machine architecture (`x86_64`, `aarch64`, ...).
    pub architecture: Option<String>,
    /// Optional exact kernel ABI, supplied by the image build pipeline.
    pub kernel_abi: Option<String>,
}

/// The immutable image identity the updater supplies for preflight checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostImage {
    pub digest: String,
    pub architecture: String,
    pub kernel_abi: String,
}

/// One signed request to make an optional extension part of a future system
/// deployment. It is a description only, never an installation script.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionManifest {
    pub schema_version: u16,
    /// Stable, lower-case extension id (`nvidia-open`, `llama-runtime`).
    pub id: String,
    pub kind: ExtensionKind,
    pub version: String,
    pub artifact: PinnedArtifact,
    pub target: ImageConstraint,
    /// A UI hint for the image updater. It must not be interpreted as a shell
    /// command or override normal boot-health policy.
    pub requires_reboot: bool,
}

impl ExtensionManifest {
    /// Reject malformed or ambiguous manifest data before it is signed or
    /// trusted. Unknown schema versions are never guessed at.
    pub fn validate(&self) -> Result<(), ExtensionError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ExtensionError::UnsupportedSchema(self.schema_version));
        }
        validate_identifier("extension id", &self.id)?;
        validate_nonempty("version", &self.version, 128)?;
        validate_nonempty("artifact source", &self.artifact.source, 2048)?;
        if self.artifact.size_bytes == 0 {
            return Err(ExtensionError::ZeroArtifactSize);
        }
        validate_sha256("artifact SHA-256", &self.artifact.sha256)?;
        validate_oci_digest(&self.target.base_image_digest)?;
        if let Some(architecture) = &self.target.architecture {
            validate_identifier("architecture", architecture)?;
        }
        if let Some(kernel_abi) = &self.target.kernel_abi {
            validate_nonempty("kernel ABI", kernel_abi, 256)?;
        }
        Ok(())
    }

    /// Check this declaration against the base image selected by the updater.
    /// The comparison is exact; a tag, a prefix, or a similar kernel release
    /// is not a compatibility proof.
    pub fn preflight(&self, host: &HostImage) -> Result<(), ExtensionError> {
        self.validate()?;
        validate_oci_digest(&host.digest)?;
        validate_identifier("host architecture", &host.architecture)?;
        validate_nonempty("host kernel ABI", &host.kernel_abi, 256)?;
        if self.target.base_image_digest != host.digest {
            return Err(ExtensionError::BaseImageMismatch);
        }
        if self
            .target
            .architecture
            .as_ref()
            .is_some_and(|architecture| architecture != &host.architecture)
        {
            return Err(ExtensionError::ArchitectureMismatch);
        }
        if self
            .target
            .kernel_abi
            .as_ref()
            .is_some_and(|kernel_abi| kernel_abi != &host.kernel_abi)
        {
            return Err(ExtensionError::KernelAbiMismatch);
        }
        Ok(())
    }

    /// Verify bytes downloaded by a separate transport. This has no I/O so a
    /// caller can stream into its own safe cache and decide where it belongs.
    pub fn verify_artifact(&self, bytes: &[u8]) -> Result<ArtifactReceipt, ExtensionError> {
        let actual_size =
            u64::try_from(bytes.len()).map_err(|_| ExtensionError::ArtifactTooLarge)?;
        if actual_size != self.artifact.size_bytes {
            return Err(ExtensionError::ArtifactSizeMismatch {
                expected: self.artifact.size_bytes,
                actual: actual_size,
            });
        }
        let actual_sha256 = hex::encode(Sha256::digest(bytes));
        if actual_sha256 != self.artifact.sha256 {
            return Err(ExtensionError::ArtifactDigestMismatch);
        }
        Ok(ArtifactReceipt {
            sha256: actual_sha256,
            size_bytes: actual_size,
        })
    }
}

/// A verified local artifact observation. It is stored only after the bytes
/// match the signed pin; it is not a path, avoiding a stale mutable path being
/// mistaken for the verified object after a reboot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactReceipt {
    pub sha256: String,
    pub size_bytes: u64,
}

/// A manifest plus the release-key identity and Ed25519 signature over its
/// canonical postcard representation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedExtensionManifest {
    pub manifest: ExtensionManifest,
    pub key_id: String,
    pub signature: Vec<u8>,
}

#[derive(Serialize)]
struct SigningPayload<'a> {
    domain: &'a str,
    manifest: &'a ExtensionManifest,
}

impl SignedExtensionManifest {
    /// Create a release envelope. Production release tooling owns the private
    /// key; devices need only [`Self::verify`].
    pub fn sign(
        manifest: ExtensionManifest,
        key_id: impl Into<String>,
        signing_key: &SigningKey,
    ) -> Result<Self, ExtensionError> {
        manifest.validate()?;
        let key_id = key_id.into();
        validate_identifier("key id", &key_id)?;
        let bytes = canonical_manifest_bytes(&manifest)?;
        let signature = signing_key.sign(&bytes).to_bytes().to_vec();
        Ok(Self {
            manifest,
            key_id,
            signature,
        })
    }

    /// Verify both manifest shape and its signature against a pinned keyring.
    pub fn verify(&self, trusted_keys: &TrustedKeyring) -> Result<(), ExtensionError> {
        self.manifest.validate()?;
        validate_identifier("key id", &self.key_id)?;
        let key = trusted_keys
            .keys
            .get(&self.key_id)
            .ok_or_else(|| ExtensionError::UnknownSigningKey(self.key_id.clone()))?;
        let signature = Signature::from_slice(&self.signature)
            .map_err(|_| ExtensionError::MalformedSignature)?;
        key.verify(&canonical_manifest_bytes(&self.manifest)?, &signature)
            .map_err(|_| ExtensionError::InvalidSignature)
    }
}

/// The release keys an image trusts. The keyring is expected to be owned by
/// the signed base image; this type deliberately has no network/key-discovery
/// mechanism.
#[derive(Clone, Debug, Default)]
pub struct TrustedKeyring {
    keys: BTreeMap<String, VerifyingKey>,
}

impl TrustedKeyring {
    pub fn insert(
        &mut self,
        key_id: impl Into<String>,
        key: VerifyingKey,
    ) -> Result<(), ExtensionError> {
        let key_id = key_id.into();
        validate_identifier("key id", &key_id)?;
        if self.keys.insert(key_id.clone(), key).is_some() {
            return Err(ExtensionError::DuplicateSigningKey(key_id));
        }
        Ok(())
    }
}

/// One extension accepted into desired state. It remains a declaration; an
/// image builder is responsible for constructing a new immutable deployment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedExtension {
    pub signed: SignedExtensionManifest,
    pub artifact: ArtifactReceipt,
    pub committed_revision: u64,
}

/// An interrupted transaction is intentionally visible and inactive. It can
/// only be completed after the exact signed artifact is verified again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingExtensionTransaction {
    pub id: String,
    pub base_revision: u64,
    pub signed: SignedExtensionManifest,
    pub artifact: Option<ArtifactReceipt>,
}

/// Durable desired state. `active` means accepted for a future image build,
/// not installed in the running root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionState {
    pub schema_version: u16,
    pub revision: u64,
    pub active: BTreeMap<String, AcceptedExtension>,
    pub pending: Option<PendingExtensionTransaction>,
}

impl Default for ExtensionState {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: 0,
            active: BTreeMap::new(),
            pending: None,
        }
    }
}

impl ExtensionState {
    pub fn validate(&self) -> Result<(), ExtensionError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ExtensionError::UnsupportedSchema(self.schema_version));
        }
        for (id, accepted) in &self.active {
            validate_identifier("active extension id", id)?;
            if accepted.signed.manifest.id != *id {
                return Err(ExtensionError::ActiveIdMismatch(id.clone()));
            }
            validate_receipt(&accepted.signed.manifest, &accepted.artifact)?;
            if accepted.committed_revision == 0 || accepted.committed_revision > self.revision {
                return Err(ExtensionError::InvalidCommittedRevision);
            }
        }
        if let Some(pending) = &self.pending {
            validate_identifier("transaction id", &pending.id)?;
            if pending.base_revision != self.revision {
                return Err(ExtensionError::TransactionRevisionMismatch);
            }
            pending.signed.manifest.validate()?;
            if let Some(receipt) = &pending.artifact {
                validate_receipt(&pending.signed.manifest, receipt)?;
            }
        }
        Ok(())
    }

    /// Begin a two-phase desired-state transaction. This mutates no active
    /// state, and a caller should persist it with [`ExtensionStateStore`]
    /// before fetching any artifact.
    pub fn begin(
        &mut self,
        transaction_id: impl Into<String>,
        signed: SignedExtensionManifest,
        trusted_keys: &TrustedKeyring,
        host: &HostImage,
    ) -> Result<(), ExtensionError> {
        self.validate()?;
        if self.pending.is_some() {
            return Err(ExtensionError::TransactionAlreadyPending);
        }
        let transaction_id = transaction_id.into();
        validate_identifier("transaction id", &transaction_id)?;
        signed.verify(trusted_keys)?;
        signed.manifest.preflight(host)?;
        self.pending = Some(PendingExtensionTransaction {
            id: transaction_id,
            base_revision: self.revision,
            signed,
            artifact: None,
        });
        Ok(())
    }

    /// Attach a receipt only after verifying the bytes against the signed pin.
    pub fn verify_pending_artifact(
        &mut self,
        transaction_id: &str,
        bytes: &[u8],
    ) -> Result<(), ExtensionError> {
        let pending = self
            .pending
            .as_mut()
            .ok_or(ExtensionError::NoPendingTransaction)?;
        if pending.id != transaction_id {
            return Err(ExtensionError::WrongTransaction);
        }
        pending.artifact = Some(pending.signed.manifest.verify_artifact(bytes)?);
        Ok(())
    }

    /// Atomically promote the verified pending declaration into desired state.
    /// It is impossible to commit an unverified artifact or to commit after the
    /// base revision changed under the transaction.
    pub fn commit(&mut self, transaction_id: &str) -> Result<AcceptedExtension, ExtensionError> {
        self.validate()?;
        // Compute fallible state before taking `pending`, so even the
        // theoretical revision-overflow failure leaves the transaction intact
        // for an explicit operator decision instead of silently dropping it.
        let next_revision = self
            .revision
            .checked_add(1)
            .ok_or(ExtensionError::RevisionOverflow)?;
        let pending = self
            .pending
            .take()
            .ok_or(ExtensionError::NoPendingTransaction)?;
        if pending.id != transaction_id {
            self.pending = Some(pending);
            return Err(ExtensionError::WrongTransaction);
        }
        if pending.base_revision != self.revision {
            self.pending = Some(pending);
            return Err(ExtensionError::TransactionRevisionMismatch);
        }
        let Some(artifact) = pending.artifact else {
            self.pending = Some(pending);
            return Err(ExtensionError::ArtifactNotVerified);
        };
        self.revision = next_revision;
        let accepted = AcceptedExtension {
            signed: pending.signed,
            artifact,
            committed_revision: self.revision,
        };
        self.active
            .insert(accepted.signed.manifest.id.clone(), accepted.clone());
        Ok(accepted)
    }

    /// Discard only the named pending transaction. Existing accepted state is
    /// untouched, which makes cancellation and crash recovery safe.
    pub fn abort(&mut self, transaction_id: &str) -> Result<(), ExtensionError> {
        let pending = self
            .pending
            .as_ref()
            .ok_or(ExtensionError::NoPendingTransaction)?;
        if pending.id != transaction_id {
            return Err(ExtensionError::WrongTransaction);
        }
        self.pending = None;
        Ok(())
    }

    /// Verify all accepted declarations before handing them to an image
    /// builder. A malformed/tampered state file gives no active extensions.
    pub fn accepted_for(
        &self,
        trusted_keys: &TrustedKeyring,
        host: &HostImage,
    ) -> Result<Vec<&AcceptedExtension>, ExtensionError> {
        self.validate()?;
        let mut accepted = Vec::with_capacity(self.active.len());
        for extension in self.active.values() {
            extension.signed.verify(trusted_keys)?;
            extension.signed.manifest.preflight(host)?;
            validate_receipt(&extension.signed.manifest, &extension.artifact)?;
            accepted.push(extension);
        }
        Ok(accepted)
    }
}

/// Atomic JSON persistence for desired state. It intentionally exposes no
/// "install" operation: callers can only stage/verify/commit declarations.
#[derive(Clone, Debug)]
pub struct ExtensionStateStore {
    path: PathBuf,
}

impl ExtensionStateStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<ExtensionState, ExtensionStoreError> {
        if !self.path.exists() {
            return Ok(ExtensionState::default());
        }
        let metadata = fs::symlink_metadata(&self.path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ExtensionStoreError::UnsafeStatePath(self.path.clone()));
        }
        let bytes = tilo_files::native::bounded::read_or(
            &self.path,
            MAX_STATE_BYTES,
            ExtensionStoreError::StateTooLarge,
        )?
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
        let state: ExtensionState = serde_json::from_slice(&bytes)?;
        state.validate()?;
        Ok(state)
    }

    pub fn save(&self, state: &ExtensionState) -> Result<(), ExtensionStoreError> {
        state.validate()?;
        let mut bytes = serde_json::to_vec_pretty(state)?;
        bytes.push(b'\n');
        // Desired state authorises a privileged image-build boundary.  Keep it
        // owner-only before the atomic rename so a session cannot replace an
        // accepted declaration between review and activation.
        tilo_files::native::atomic::write_creating_parent_with_mode(&self.path, &bytes, 0o600)?;
        Ok(())
    }

    pub fn begin(
        &self,
        transaction_id: impl Into<String>,
        signed: SignedExtensionManifest,
        trusted_keys: &TrustedKeyring,
        host: &HostImage,
    ) -> Result<(), ExtensionStoreError> {
        let mut state = self.load()?;
        state.begin(transaction_id, signed, trusted_keys, host)?;
        self.save(&state)
    }

    pub fn verify_pending_artifact(
        &self,
        transaction_id: &str,
        bytes: &[u8],
    ) -> Result<(), ExtensionStoreError> {
        let mut state = self.load()?;
        state.verify_pending_artifact(transaction_id, bytes)?;
        self.save(&state)
    }

    pub fn commit(&self, transaction_id: &str) -> Result<AcceptedExtension, ExtensionStoreError> {
        let mut state = self.load()?;
        let accepted = state.commit(transaction_id)?;
        self.save(&state)?;
        Ok(accepted)
    }

    pub fn abort(&self, transaction_id: &str) -> Result<(), ExtensionStoreError> {
        let mut state = self.load()?;
        state.abort(transaction_id)?;
        self.save(&state)
    }
}

fn canonical_manifest_bytes(manifest: &ExtensionManifest) -> Result<Vec<u8>, ExtensionError> {
    postcard::to_allocvec(&SigningPayload {
        domain: SIGNING_DOMAIN,
        manifest,
    })
    .map_err(|_| ExtensionError::CanonicalEncoding)
}

fn validate_receipt(
    manifest: &ExtensionManifest,
    receipt: &ArtifactReceipt,
) -> Result<(), ExtensionError> {
    validate_sha256("artifact receipt SHA-256", &receipt.sha256)?;
    if receipt.sha256 != manifest.artifact.sha256
        || receipt.size_bytes != manifest.artifact.size_bytes
    {
        return Err(ExtensionError::ReceiptDoesNotMatchManifest);
    }
    Ok(())
}

fn validate_identifier(name: &str, value: &str) -> Result<(), ExtensionError> {
    validate_nonempty(name, value, 128)?;
    if !value.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
    }) || value.starts_with(['.', '-', '_'])
        || value.ends_with(['.', '-', '_'])
    {
        return Err(ExtensionError::InvalidIdentifier(name.to_string()));
    }
    Ok(())
}

fn validate_nonempty(name: &str, value: &str, max_len: usize) -> Result<(), ExtensionError> {
    if value.trim().is_empty() || value.len() > max_len || value.contains(['\n', '\r', '\0']) {
        return Err(ExtensionError::InvalidField(name.to_string()));
    }
    Ok(())
}

fn validate_sha256(name: &str, value: &str) -> Result<(), ExtensionError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(ExtensionError::InvalidDigest(name.to_string()));
    }
    Ok(())
}

fn validate_oci_digest(value: &str) -> Result<(), ExtensionError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(ExtensionError::InvalidDigest("base image digest".into()));
    };
    validate_sha256("base image digest", hex)
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExtensionError {
    #[error("unsupported extension schema version {0}")]
    UnsupportedSchema(u16),
    #[error("invalid {0}")]
    InvalidField(String),
    #[error("invalid identifier for {0}")]
    InvalidIdentifier(String),
    #[error("invalid {0}")]
    InvalidDigest(String),
    #[error("artifact size must be non-zero")]
    ZeroArtifactSize,
    #[error("artifact is too large to represent")]
    ArtifactTooLarge,
    #[error("artifact size does not match signed pin (expected {expected}, got {actual})")]
    ArtifactSizeMismatch { expected: u64, actual: u64 },
    #[error("artifact SHA-256 does not match signed pin")]
    ArtifactDigestMismatch,
    #[error("canonical manifest encoding failed")]
    CanonicalEncoding,
    #[error("unknown extension signing key {0}")]
    UnknownSigningKey(String),
    #[error("duplicate extension signing key {0}")]
    DuplicateSigningKey(String),
    #[error("extension signature is malformed")]
    MalformedSignature,
    #[error("extension signature is invalid")]
    InvalidSignature,
    #[error("extension targets a different immutable base image")]
    BaseImageMismatch,
    #[error("extension architecture does not match the selected image")]
    ArchitectureMismatch,
    #[error("extension kernel ABI does not match the selected image")]
    KernelAbiMismatch,
    #[error("an extension transaction is already pending")]
    TransactionAlreadyPending,
    #[error("there is no pending extension transaction")]
    NoPendingTransaction,
    #[error("the requested transaction does not match the pending transaction")]
    WrongTransaction,
    #[error("the pending transaction was created against a different state revision")]
    TransactionRevisionMismatch,
    #[error("artifact must be verified before commit")]
    ArtifactNotVerified,
    #[error("extension state revision overflowed")]
    RevisionOverflow,
    #[error("active state key does not match manifest id {0}")]
    ActiveIdMismatch(String),
    #[error("accepted extension has an invalid committed revision")]
    InvalidCommittedRevision,
    #[error("artifact receipt does not match the signed manifest")]
    ReceiptDoesNotMatchManifest,
}

#[derive(Debug, Error)]
pub enum ExtensionStoreError {
    #[error(transparent)]
    State(#[from] ExtensionError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("extension state is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("extension state path is not a regular file: {0}")]
    UnsafeStatePath(PathBuf),
    #[error("extension state exceeds 4 MiB")]
    StateTooLarge,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    fn host() -> HostImage {
        HostImage {
            digest: format!("sha256:{}", digest('a')),
            architecture: "x86_64".into(),
            kernel_abi: "6.17.0-tilo.1".into(),
        }
    }

    fn manifest(bytes: &[u8]) -> ExtensionManifest {
        ExtensionManifest {
            schema_version: SCHEMA_VERSION,
            id: "local-model-runtime".into(),
            kind: ExtensionKind::LocalModelRuntime,
            version: "1.2.3".into(),
            artifact: PinnedArtifact {
                source: "oci://registry.example.test/models/runtime".into(),
                sha256: hex::encode(Sha256::digest(bytes)),
                size_bytes: u64::try_from(bytes.len()).unwrap(),
            },
            target: ImageConstraint {
                base_image_digest: host().digest,
                architecture: Some("x86_64".into()),
                kernel_abi: Some("6.17.0-tilo.1".into()),
            },
            requires_reboot: false,
        }
    }

    fn signer_and_keyring() -> (SigningKey, TrustedKeyring) {
        let signer = SigningKey::from_bytes(&[7; 32]);
        let mut keyring = TrustedKeyring::default();
        keyring
            .insert("release-2026", signer.verifying_key())
            .unwrap();
        (signer, keyring)
    }

    #[test]
    fn extensions_are_accepted_only_after_exact_signed_verification() {
        {
            let bytes = b"runtime artifact";
            let (release_key, keyring) = signer_and_keyring();
            let mut envelope =
                SignedExtensionManifest::sign(manifest(bytes), "release-2026", &release_key)
                    .unwrap();
            envelope.verify(&keyring).unwrap();
            envelope.manifest.target.kernel_abi = Some("other".into());
            assert_eq!(
                envelope.verify(&keyring),
                Err(ExtensionError::InvalidSignature)
            );
        }
        {
            let manifest = manifest(b"runtime artifact");
            manifest.preflight(&host()).unwrap();
            let mut wrong = host();
            wrong.kernel_abi = "6.17.1-tilo.1".into();
            assert_eq!(
                manifest.preflight(&wrong),
                Err(ExtensionError::KernelAbiMismatch)
            );
            wrong = host();
            wrong.digest = format!("sha256:{}", digest('b'));
            assert_eq!(
                manifest.preflight(&wrong),
                Err(ExtensionError::BaseImageMismatch)
            );
        }
        {
            let bytes = b"runtime artifact";
            let (release_key, keyring) = signer_and_keyring();
            let envelope =
                SignedExtensionManifest::sign(manifest(bytes), "release-2026", &release_key)
                    .unwrap();
            let mut state = ExtensionState::default();
            state
                .begin("install-runtime", envelope, &keyring, &host())
                .unwrap();
            assert!(state.active.is_empty());
            assert_eq!(
                state.commit("install-runtime"),
                Err(ExtensionError::ArtifactNotVerified)
            );
            assert!(state.active.is_empty());
            assert!(
                state.pending.is_some(),
                "failed commit keeps the transaction inert for recovery"
            );

            assert!(
                state
                    .verify_pending_artifact("install-runtime", b"tampered")
                    .is_err()
            );
            assert!(state.active.is_empty());
            state
                .verify_pending_artifact("install-runtime", bytes)
                .unwrap();
            let accepted = state.commit("install-runtime").unwrap();
            assert_eq!(
                accepted.artifact.size_bytes,
                u64::try_from(bytes.len()).unwrap()
            );
            assert_eq!(state.active.len(), 1);
            assert!(state.pending.is_none());
        }
        {
            let bytes = b"runtime artifact";
            let (release_key, keyring) = signer_and_keyring();
            let envelope =
                SignedExtensionManifest::sign(manifest(bytes), "release-2026", &release_key)
                    .unwrap();
            let mut state = ExtensionState::default();
            state
                .begin("install-runtime", envelope, &keyring, &host())
                .unwrap();
            state
                .verify_pending_artifact("install-runtime", bytes)
                .unwrap();
            state.commit("install-runtime").unwrap();
            assert_eq!(state.accepted_for(&keyring, &host()).unwrap().len(), 1);
            let empty_keys = TrustedKeyring::default();
            assert!(matches!(
                state.accepted_for(&empty_keys, &host()),
                Err(ExtensionError::UnknownSigningKey(_))
            ));
        }
    }

    #[test]
    fn the_state_store_persists_only_valid_transitions_and_never_defaults() {
        {
            use std::os::unix::fs::symlink;

            let temp = tempfile::TempDir::new().unwrap();
            let path = temp.path().join("extensions.json");
            fs::write(&path, b"not json").unwrap();
            assert!(matches!(
                ExtensionStateStore::new(&path).load(),
                Err(ExtensionStoreError::Json(_))
            ));

            let target = temp.path().join("target.json");
            fs::write(&target, b"{}").unwrap();
            fs::remove_file(&path).unwrap();
            symlink(&target, &path).unwrap();
            assert!(matches!(
                ExtensionStateStore::new(&path).load(),
                Err(ExtensionStoreError::UnsafeStatePath(_))
            ));
        }
        {
            let bytes = b"runtime artifact";
            let (release_key, keyring) = signer_and_keyring();
            let envelope =
                SignedExtensionManifest::sign(manifest(bytes), "release-2026", &release_key)
                    .unwrap();
            let temp = tempfile::TempDir::new().unwrap();
            let store = ExtensionStateStore::new(temp.path().join("state/extensions.json"));
            store
                .begin("install-runtime", envelope, &keyring, &host())
                .unwrap();
            assert!(store.load().unwrap().active.is_empty());
            store
                .verify_pending_artifact("install-runtime", bytes)
                .unwrap();
            store.commit("install-runtime").unwrap();
            let state = store.load().unwrap();
            assert_eq!(state.revision, 1);
            assert!(state.pending.is_none());
            assert!(state.active.contains_key("local-model-runtime"));
        }
        {
            let bytes = b"runtime artifact";
            let (release_key, keyring) = signer_and_keyring();
            let envelope =
                SignedExtensionManifest::sign(manifest(bytes), "release-2026", &release_key)
                    .unwrap();
            let mut state = ExtensionState {
                revision: u64::MAX,
                ..ExtensionState::default()
            };
            state
                .begin("install-runtime", envelope, &keyring, &host())
                .unwrap();
            state
                .verify_pending_artifact("install-runtime", bytes)
                .unwrap();
            assert_eq!(
                state.commit("install-runtime"),
                Err(ExtensionError::RevisionOverflow)
            );
            assert!(state.active.is_empty());
            assert!(state.pending.is_some());
        }
    }

    #[cfg(unix)]
    #[test]
    fn persisted_state_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::TempDir::new().unwrap();
        let store = ExtensionStateStore::new(temp.path().join("state.json"));
        store.save(&ExtensionState::default()).unwrap();
        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "desired extension state must be owner-only");
    }
}
