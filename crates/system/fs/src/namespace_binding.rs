//! Namespace payloads bound to the workspace they belong to.
//!
//! A workspace's namespace is one Automerge document, and nothing inside
//! its bytes says which workspace that is. A device that serves several
//! workspaces under one key (a home peer replicating for more than one
//! room) or a peer that simply lies could otherwise hand a requester a
//! different workspace's valid tree, which Automerge would merge without a
//! complaint.
//!
//! Every namespace payload that crosses the wire therefore travels with a
//! [`NamespaceBindingV1`]: a whole document, the changes beyond a set of
//! heads, and each Automerge sync message. The sending device signs the
//! workspace it serves, the kind of payload and the BLAKE3 digest of its
//! exact bytes. A receiver applies the payload only when
//! [`NamespaceBindingV1::verify`] accepts it for the workspace it asked for,
//! signed by the device the transport authenticated; whether that device's
//! holder may serve (or author) the workspace's namespace at all is the
//! receiver's policy decision, made next to the same call
//! (`sync::accept_namespace_payload`).

use collab_types::{CanonicalError, Ed25519PublicKey, Signature, sign_event, verify_event};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const SIGNATURE_DOMAIN: &str = "tilo.collaborative-fs.namespace-binding.v1";
const DIGEST_DOMAIN: &str = "tilo.collaborative-fs.namespace-payload.v1";

/// What the bound bytes are, so one kind is never taken for another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NamespacePayloadKind {
    /// A whole saved document (a bootstrap or a read-only full pull).
    Document,
    /// The changes beyond the requester's heads (a read-only pull).
    Changes,
    /// One Automerge sync-protocol message between two writers.
    SyncMessage,
}

/// A device's signed statement that `digest` names namespace bytes of
/// `workspace_id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamespaceBindingV1 {
    pub workspace_id: Uuid,
    pub kind: NamespacePayloadKind,
    /// Keyed BLAKE3 of the exact payload bytes.
    pub digest: [u8; 32],
    /// The signing device.
    pub device: Ed25519PublicKey,
    pub signature: Signature,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct SigningView<'a> {
    domain: &'static str,
    workspace_id: &'a Uuid,
    kind: NamespacePayloadKind,
    digest: &'a [u8; 32],
    device: &'a Ed25519PublicKey,
}

/// Why a namespace payload was refused.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum NamespaceBindingError {
    #[error("namespace payload belongs to workspace {actual}, not the requested {expected}")]
    WrongWorkspace { expected: Uuid, actual: Uuid },
    #[error("namespace payload is bound as {actual:?}, not {expected:?}")]
    WrongKind {
        expected: NamespacePayloadKind,
        actual: NamespacePayloadKind,
    },
    #[error("namespace payload was bound by another device than the authenticated peer")]
    WrongDevice,
    #[error("namespace payload bytes do not match their signed digest")]
    DigestMismatch,
    #[error("namespace payload binding signature does not verify")]
    BadSignature,
    #[error("the peer is not authorised to serve this workspace's namespace")]
    Unauthorised,
    #[error("the peer sent namespace bytes without a workspace binding")]
    Unbound,
    #[error("could not encode the namespace binding: {0}")]
    Canonical(#[from] CanonicalError),
}

/// The digest a binding signs.
#[must_use]
pub fn payload_digest(bytes: &[u8]) -> [u8; 32] {
    *blake3::Hasher::new_derive_key(DIGEST_DOMAIN)
        .update(bytes)
        .finalize()
        .as_bytes()
}

impl NamespaceBindingV1 {
    /// Binds `bytes` to `workspace_id` as this device.
    ///
    /// # Errors
    ///
    /// Returns an error only if the signing view cannot be encoded.
    pub fn sign(
        workspace_id: Uuid,
        kind: NamespacePayloadKind,
        bytes: &[u8],
        device_key: &ed25519_dalek::SigningKey,
    ) -> Result<Self, NamespaceBindingError> {
        let device = Ed25519PublicKey(device_key.verifying_key().to_bytes());
        let digest = payload_digest(bytes);
        let signature = sign_event(
            &SigningView {
                domain: SIGNATURE_DOMAIN,
                workspace_id: &workspace_id,
                kind,
                digest: &digest,
                device: &device,
            },
            device_key,
        )?;
        Ok(Self {
            workspace_id,
            kind,
            digest,
            device,
            signature,
        })
    }

    /// Accepts `bytes` as `kind` namespace bytes of `workspace_id`, sent by
    /// `device` (the key the transport authenticated).
    ///
    /// # Errors
    ///
    /// Returns the first check that fails: the workspace, the kind, the
    /// signing device, the digest of the exact bytes, then the signature.
    pub fn verify(
        &self,
        workspace_id: Uuid,
        kind: NamespacePayloadKind,
        device: [u8; 32],
        bytes: &[u8],
    ) -> Result<(), NamespaceBindingError> {
        if self.workspace_id != workspace_id {
            return Err(NamespaceBindingError::WrongWorkspace {
                expected: workspace_id,
                actual: self.workspace_id,
            });
        }
        if self.kind != kind {
            return Err(NamespaceBindingError::WrongKind {
                expected: kind,
                actual: self.kind,
            });
        }
        if self.device.0 != device {
            return Err(NamespaceBindingError::WrongDevice);
        }
        if payload_digest(bytes) != self.digest {
            return Err(NamespaceBindingError::DigestMismatch);
        }
        verify_event(
            &SigningView {
                domain: SIGNATURE_DOMAIN,
                workspace_id: &self.workspace_id,
                kind: self.kind,
                digest: &self.digest,
                device: &self.device,
            },
            &self.signature,
            &self.device,
        )
        .map_err(|_| NamespaceBindingError::BadSignature)
    }
}

/// Namespace bytes with their binding, as one wire value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundNamespacePayloadV1 {
    pub binding: NamespaceBindingV1,
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
}

impl BoundNamespacePayloadV1 {
    /// Binds `bytes` as this device.
    ///
    /// # Errors
    ///
    /// As [`NamespaceBindingV1::sign`].
    pub fn sign(
        workspace_id: Uuid,
        kind: NamespacePayloadKind,
        bytes: Vec<u8>,
        device_key: &ed25519_dalek::SigningKey,
    ) -> Result<Self, NamespaceBindingError> {
        let binding = NamespaceBindingV1::sign(workspace_id, kind, &bytes, device_key)?;
        Ok(Self { binding, bytes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: Uuid = Uuid::from_u128(0xa11ce);
    const OTHER: Uuid = Uuid::from_u128(0xb0b);

    fn device(seed: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
    }

    fn public(key: &ed25519_dalek::SigningKey) -> [u8; 32] {
        key.verifying_key().to_bytes()
    }

    #[test]
    fn a_binding_verifies_for_its_workspace_kind_device_and_bytes() {
        let key = device(1);
        let binding =
            NamespaceBindingV1::sign(WORKSPACE, NamespacePayloadKind::Document, b"tree", &key)
                .unwrap();
        binding
            .verify(
                WORKSPACE,
                NamespacePayloadKind::Document,
                public(&key),
                b"tree",
            )
            .unwrap();
    }

    #[test]
    fn another_workspaces_signed_tree_is_refused() {
        let key = device(1);
        let binding =
            NamespaceBindingV1::sign(OTHER, NamespacePayloadKind::Document, b"tree", &key).unwrap();
        assert_eq!(
            binding.verify(
                WORKSPACE,
                NamespacePayloadKind::Document,
                public(&key),
                b"tree"
            ),
            Err(NamespaceBindingError::WrongWorkspace {
                expected: WORKSPACE,
                actual: OTHER
            })
        );
    }

    #[test]
    fn changed_bytes_another_kind_or_another_signer_are_refused() {
        let key = device(1);
        let binding =
            NamespaceBindingV1::sign(WORKSPACE, NamespacePayloadKind::Changes, b"delta", &key)
                .unwrap();
        assert_eq!(
            binding.verify(
                WORKSPACE,
                NamespacePayloadKind::Changes,
                public(&key),
                b"delta!"
            ),
            Err(NamespaceBindingError::DigestMismatch)
        );
        assert!(matches!(
            binding.verify(
                WORKSPACE,
                NamespacePayloadKind::Document,
                public(&key),
                b"delta"
            ),
            Err(NamespaceBindingError::WrongKind { .. })
        ));
        assert_eq!(
            binding.verify(
                WORKSPACE,
                NamespacePayloadKind::Changes,
                public(&device(2)),
                b"delta"
            ),
            Err(NamespaceBindingError::WrongDevice)
        );
    }

    #[test]
    fn a_rewritten_workspace_or_digest_breaks_the_signature() {
        let key = device(1);
        let mut binding =
            NamespaceBindingV1::sign(OTHER, NamespacePayloadKind::SyncMessage, b"m", &key).unwrap();
        binding.workspace_id = WORKSPACE;
        assert_eq!(
            binding.verify(
                WORKSPACE,
                NamespacePayloadKind::SyncMessage,
                public(&key),
                b"m"
            ),
            Err(NamespaceBindingError::BadSignature)
        );
        let mut binding =
            NamespaceBindingV1::sign(WORKSPACE, NamespacePayloadKind::SyncMessage, b"m", &key)
                .unwrap();
        binding.digest = payload_digest(b"forged");
        assert_eq!(
            binding.verify(
                WORKSPACE,
                NamespacePayloadKind::SyncMessage,
                public(&key),
                b"forged"
            ),
            Err(NamespaceBindingError::BadSignature)
        );
    }
}
