//! Versioned, signed clear metadata around opaque MLS governance bytes.
//!
//! Any device of a workspace that commits writes the MLS commit that brings
//! the workspace's group in line with the membership, and signs it in a
//! [`MlsGovernanceFrame`]. Hosts route the frame and target its opaque
//! Welcome and key history; they never receive group keys or decrypt
//! application data.

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::{
    CanonicalError, Ed25519PublicKey, Signature, VerifyError, WorkspaceId, canonical_bytes,
    sign_event, verify_event,
};

/// Governance wire format. Unknown versions are refused.
pub const MLS_GOVERNANCE_VERSION: u16 = 2;
/// Prevent a signed frame from becoming an unbounded allocation vector.
pub const MAX_MLS_CONTROL_BYTES: usize = 16 * 1024 * 1024;
/// Devices one commit may add or remove.
pub const MAX_COMMIT_DEVICES: usize = 4096;

/// What a frame does to its workspace's group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum MlsGovernanceAction {
    /// The first frame of a workspace's group: its signer created the group
    /// alone, and no plaintext document op is accepted for the workspace from
    /// then on. Two devices creating the same group at once keep the genesis
    /// with the lower digest.
    Genesis,
    /// One MLS commit moving the group to the membership: `removed` leave,
    /// `added` join from `joining`. A device in both was admitted again with
    /// a fresh key package after it lost its state.
    Commit {
        removed: Vec<Ed25519PublicKey>,
        added: Vec<Ed25519PublicKey>,
        commit: Vec<u8>,
        /// What `added` join from; `None` exactly when `added` is empty.
        joining: Option<Joining>,
        /// Content keys of a branch of the group that lost (a genesis or
        /// a commit whose sibling had the lower digest) that the committer
        /// held, sealed for the post-commit epoch: every member keeps them,
        /// so what was sealed under that branch stays readable. A commit may
        /// carry only this, changing no one.
        history: Option<Vec<u8>>,
    },
}

/// What the devices one commit adds join from: one Welcome and their
/// content-key history, sealed for the post-commit epoch, so a joiner can
/// decrypt every earlier op without any other member online.
///
/// Only those devices need it, and it grows with the group (the Welcome
/// carries the group's ratchet tree), so it travels to them alone: every
/// other member gets the frame with the material [`Joining::Withheld`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinMaterial {
    pub welcome: Vec<u8>,
    pub key_history: Vec<u8>,
}

impl JoinMaterial {
    /// SHA-256 of the material: what the frame signs and links over in its
    /// place, so withholding it changes neither.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the material cannot be encoded.
    pub fn digest(&self) -> Result<[u8; 32], CanonicalError> {
        Ok(Sha256::digest(canonical_bytes(self)?).into())
    }
}

/// A commit's [`JoinMaterial`], or only its digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Joining {
    /// The material, for the committer and the devices it adds.
    Material(JoinMaterial),
    /// The material's digest, for every member already in the group:
    /// they apply the commit and never need the Welcome.
    Withheld([u8; 32]),
}

impl Joining {
    /// The material's digest, whether it is carried or withheld.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the material cannot be encoded.
    pub fn digest(&self) -> Result<[u8; 32], CanonicalError> {
        match self {
            Self::Material(material) => material.digest(),
            Self::Withheld(digest) => Ok(*digest),
        }
    }

    /// The material, unless it is withheld.
    pub const fn material(&self) -> Option<&JoinMaterial> {
        match self {
            Self::Material(material) => Some(material),
            Self::Withheld(_) => None,
        }
    }
}

impl MlsGovernanceAction {
    /// The action as signed and linked over: join material by its digest.
    fn view(&self) -> Result<ActionView<'_>, CanonicalError> {
        Ok(match self {
            Self::Genesis => ActionView::Genesis,
            Self::Commit {
                removed,
                added,
                commit,
                joining,
                history,
            } => ActionView::Commit {
                removed,
                added,
                commit,
                joining: joining.as_ref().map(Joining::digest).transpose()?,
                history: history.as_deref(),
            },
        })
    }
}

#[derive(Serialize)]
enum ActionView<'a> {
    Genesis,
    Commit {
        removed: &'a [Ed25519PublicKey],
        added: &'a [Ed25519PublicKey],
        commit: &'a [u8],
        joining: Option<[u8; 32]>,
        history: Option<&'a [u8]>,
    },
}

/// A signed, hash-linked governance frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MlsGovernanceFrame {
    pub version: u16,
    pub workspace: WorkspaceId,
    /// Digest of the frame this one follows, or all-zero for a genesis.
    pub previous_frame_digest: [u8; 32],
    /// MLS epoch after the commit; 0 for a genesis.
    pub epoch: u64,
    pub action: MlsGovernanceAction,
    pub signed_by: Ed25519PublicKey,
    pub signature: Signature,
}

#[derive(Serialize)]
struct SigningView<'a> {
    domain: &'static [u8],
    version: u16,
    workspace: WorkspaceId,
    previous_frame_digest: [u8; 32],
    epoch: u64,
    action: ActionView<'a>,
    signed_by: Ed25519PublicKey,
}

#[derive(Serialize)]
struct DigestView<'a> {
    signed: SigningView<'a>,
    signature: Signature,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MlsGovernanceError {
    #[error("unsupported MLS governance version {0}")]
    Version(u16),
    #[error("governance signature is invalid: {0}")]
    Signature(String),
    #[error("governance frame is malformed")]
    Malformed,
    #[error("required Welcome or commit is missing or oversized")]
    InvalidControlPayload,
}

impl MlsGovernanceFrame {
    /// Create and sign a frame.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the signing view cannot be encoded.
    pub fn signed(
        workspace: WorkspaceId,
        previous_frame_digest: [u8; 32],
        epoch: u64,
        action: MlsGovernanceAction,
        signer: &SigningKey,
    ) -> Result<Self, CanonicalError> {
        let mut frame = Self::unsigned(
            workspace,
            previous_frame_digest,
            epoch,
            action,
            Ed25519PublicKey::from_bytes(signer.verifying_key().to_bytes()),
        );
        frame.signature = sign_event(&frame.signing_view()?, signer)?;
        Ok(frame)
    }

    /// A frame whose signature is still to be made by `signed_by`,
    /// see [`MlsAccessRequestV1::unsigned`]: sign [`Self::signing_bytes`] and
    /// store it in `signature`.
    pub fn unsigned(
        workspace: WorkspaceId,
        previous_frame_digest: [u8; 32],
        epoch: u64,
        action: MlsGovernanceAction,
        signed_by: Ed25519PublicKey,
    ) -> Self {
        Self {
            version: MLS_GOVERNANCE_VERSION,
            workspace,
            previous_frame_digest,
            epoch,
            action,
            signed_by,
            signature: Signature::from_bytes([0; 64]),
        }
    }

    /// The bytes `signed_by` signs.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the signing view cannot be encoded.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(&self.signing_view()?)
    }

    fn signing_view(&self) -> Result<SigningView<'_>, CanonicalError> {
        Ok(SigningView {
            domain: b"tilo.mls-governance.v2",
            version: self.version,
            workspace: self.workspace,
            previous_frame_digest: self.previous_frame_digest,
            epoch: self.epoch,
            action: self.action.view()?,
            signed_by: self.signed_by,
        })
    }

    /// SHA-256 link value covering the signature as well as all metadata,
    /// the same whether join material is carried or withheld. Of two frames
    /// following the same one, every device keeps the lower digest.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the frame cannot be encoded.
    pub fn digest(&self) -> Result<[u8; 32], CanonicalError> {
        let view = DigestView {
            signed: self.signing_view()?,
            signature: self.signature,
        };
        Ok(Sha256::digest(canonical_bytes(&view)?).into())
    }

    /// The devices this frame adds.
    pub fn added(&self) -> &[Ed25519PublicKey] {
        match &self.action {
            MlsGovernanceAction::Commit { added, .. } => added,
            MlsGovernanceAction::Genesis => &[],
        }
    }

    /// The devices this frame removes.
    pub fn removed(&self) -> &[Ed25519PublicKey] {
        match &self.action {
            MlsGovernanceAction::Commit { removed, .. } => removed,
            MlsGovernanceAction::Genesis => &[],
        }
    }

    /// The join material this copy carries, if any.
    pub fn join_material(&self) -> Option<&JoinMaterial> {
        match &self.action {
            MlsGovernanceAction::Commit {
                joining: Some(joining),
                ..
            } => joining.material(),
            _ => None,
        }
    }

    /// The frame with its join material withheld: what every member but the
    /// devices it adds gets. `None` for a frame that carries none.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the material cannot be encoded.
    pub fn withheld(&self) -> Result<Option<Self>, CanonicalError> {
        let MlsGovernanceAction::Commit {
            removed,
            added,
            commit,
            joining: Some(Joining::Material(material)),
            history,
        } = &self.action
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            action: MlsGovernanceAction::Commit {
                removed: removed.clone(),
                added: added.clone(),
                commit: commit.clone(),
                joining: Some(Joining::Withheld(material.digest()?)),
                history: history.clone(),
            },
            ..self.clone()
        }))
    }

    /// What a committer publishes for this frame: the frame, and for a
    /// commit adding devices also its copy with the material withheld. A
    /// host delivers the whole one to the devices it adds alone and the other
    /// to everyone.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the material cannot be encoded.
    pub fn into_deliveries(self) -> Result<Vec<Self>, CanonicalError> {
        let withheld = self.withheld()?;
        Ok(std::iter::once(self).chain(withheld).collect())
    }

    /// Verify the frame's detached signature.
    ///
    /// # Errors
    ///
    /// Returns [`VerifyError`] when the signature does not verify.
    pub fn verify_signature(&self) -> Result<(), VerifyError> {
        verify_event(&self.signing_view()?, &self.signature, &self.signed_by)
    }

    /// Check the version, the signature and the shape of the action: a
    /// genesis opens the chain at epoch 0, and a commit names distinct
    /// devices within bounds (or hands over history), carries bounded MLS
    /// bytes, and joining material exactly when it adds anyone. Whether the changes match the
    /// membership is for the receiver's projection to decide.
    ///
    /// # Errors
    ///
    /// Returns [`MlsGovernanceError`] for the first check that fails.
    pub fn check(&self) -> Result<(), MlsGovernanceError> {
        if self.version != MLS_GOVERNANCE_VERSION {
            return Err(MlsGovernanceError::Version(self.version));
        }
        self.verify_signature()
            .map_err(|error| MlsGovernanceError::Signature(error.to_string()))?;
        let bad = |bytes: &[u8]| bytes.is_empty() || bytes.len() > MAX_MLS_CONTROL_BYTES;
        match &self.action {
            MlsGovernanceAction::Genesis => {
                if self.previous_frame_digest != [0; 32] || self.epoch != 0 {
                    return Err(MlsGovernanceError::Malformed);
                }
            }
            MlsGovernanceAction::Commit {
                removed,
                added,
                commit,
                joining,
                history,
            } => {
                let distinct = |devices: &[Ed25519PublicKey]| {
                    devices.len() <= MAX_COMMIT_DEVICES
                        && devices
                            .iter()
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                            == devices.len()
                };
                if self.epoch == 0
                    || (removed.is_empty() && added.is_empty() && history.is_none())
                    || !distinct(removed)
                    || !distinct(added)
                    || removed.contains(&self.signed_by)
                    || added.contains(&self.signed_by)
                    || added.is_empty() != joining.is_none()
                {
                    return Err(MlsGovernanceError::Malformed);
                }
                let material_bad = joining
                    .as_ref()
                    .and_then(Joining::material)
                    .is_some_and(|material| bad(&material.welcome) || bad(&material.key_history));
                if bad(commit) || material_bad || history.as_deref().is_some_and(bad) {
                    return Err(MlsGovernanceError::InvalidControlPayload);
                }
            }
        }
        Ok(())
    }
}

/// Version of [`MlsAccessRequestV1`].
pub const MLS_ACCESS_VERSION: u16 = 1;

/// One device asking a workspace's key holders for membership: a fresh MLS
/// key package for that workspace's group, signed by the device's certified key.
///
/// Any key holder admits the device once the workspace's access policy (workspace
/// membership through the person's certified device set, or a live service
/// grant) names this device key. The signature binds the key
/// package to the device, so a relay that stores or forwards the request
/// cannot substitute its own package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MlsAccessRequestV1 {
    pub version: u16,
    pub workspace: WorkspaceId,
    /// Certified device key of the requester; also its MLS credential name.
    pub device: Ed25519PublicKey,
    /// TLS-serialised MLS `KeyPackage` minted for this group only.
    pub key_package: Vec<u8>,
    /// When the device minted the request (seconds since the Unix epoch),
    /// so a key holder keeps only the newest request per device.
    pub issued_at: u64,
    pub signature: Signature,
}

#[derive(Serialize)]
struct AccessRequestView<'a> {
    domain: &'static [u8],
    version: u16,
    workspace: WorkspaceId,
    device: Ed25519PublicKey,
    key_package: &'a [u8],
    issued_at: u64,
}

impl MlsAccessRequestV1 {
    /// Create and sign a request with the device key.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the signing view cannot be encoded.
    pub fn signed(
        workspace: WorkspaceId,
        key_package: Vec<u8>,
        issued_at: u64,
        device_key: &SigningKey,
    ) -> Result<Self, CanonicalError> {
        let mut request = Self {
            version: MLS_ACCESS_VERSION,
            workspace,
            device: Ed25519PublicKey::from_bytes(device_key.verifying_key().to_bytes()),
            key_package,
            issued_at,
            signature: Signature::from_bytes([0; 64]),
        };
        request.signature = sign_event(&request.signing_view(), device_key)?;
        Ok(request)
    }

    /// A request for `device` whose signature is still to be made by a
    /// key this process cannot hold (a browser's non-extractable `WebCrypto`
    /// key): sign [`Self::signing_bytes`] and store it in `signature`.
    pub fn unsigned(
        workspace: WorkspaceId,
        device: Ed25519PublicKey,
        key_package: Vec<u8>,
        issued_at: u64,
    ) -> Self {
        Self {
            version: MLS_ACCESS_VERSION,
            workspace,
            device,
            key_package,
            issued_at,
            signature: Signature::from_bytes([0; 64]),
        }
    }

    /// The bytes the device key signs.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the signing view cannot be encoded.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(&self.signing_view())
    }

    fn signing_view(&self) -> AccessRequestView<'_> {
        AccessRequestView {
            domain: b"tilo.mls-access-request.v1",
            version: self.version,
            workspace: self.workspace,
            device: self.device,
            key_package: &self.key_package,
            issued_at: self.issued_at,
        }
    }

    /// Verify the version, payload bound and device signature.
    pub fn verify(&self) -> Result<(), MlsGovernanceError> {
        if self.version != MLS_ACCESS_VERSION {
            return Err(MlsGovernanceError::Version(self.version));
        }
        if self.key_package.is_empty() || self.key_package.len() > MAX_MLS_CONTROL_BYTES {
            return Err(MlsGovernanceError::InvalidControlPayload);
        }
        verify_event(&self.signing_view(), &self.signature, &self.device)
            .map_err(|error| MlsGovernanceError::Signature(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate_keypair;

    tilo_test_env::scenario! {
        /// The signed frames of key governance: what each proves, what a signature covers and which frames are refused.
        fn governance_frames_are_bound_to_what_they_sign_and_refused_when_malformed;

        phase access_request_is_bound_to_its_device_workspace_and_package;
        {
            let (device, device_pk) = generate_keypair();
            let workspace = WorkspaceId::new();
            let request = MlsAccessRequestV1::signed(workspace, vec![1, 2, 3], 7, &device).unwrap();
            assert_eq!(request.device, device_pk);
            request.verify().unwrap();

            let mut swapped = request.clone();
            swapped.key_package = vec![9];
            assert!(swapped.verify().is_err());
            let mut moved = request.clone();
            moved.workspace = WorkspaceId::new();
            assert!(moved.verify().is_err());
            let (_, other) = generate_keypair();
            let mut impersonated = request;
            impersonated.device = other;
            assert!(impersonated.verify().is_err());
            let empty = MlsAccessRequestV1::signed(workspace, Vec::new(), 7, &device).unwrap();
            assert_eq!(
                empty.verify(),
                Err(MlsGovernanceError::InvalidControlPayload)
            );
        }

        phase a_frame_signed_elsewhere_matches_one_signed_here;
        {
            let (signer, signer_pk) = generate_keypair();
            let workspace = WorkspaceId::new();
            let expected =
                MlsGovernanceFrame::signed(workspace, [0; 32], 0, MlsGovernanceAction::Genesis, &signer)
                    .unwrap();
            let mut unsigned = MlsGovernanceFrame::unsigned(
                workspace,
                [0; 32],
                0,
                MlsGovernanceAction::Genesis,
                signer_pk,
            );
            assert!(unsigned.verify_signature().is_err());
            let message = unsigned.signing_bytes().unwrap();
            unsigned.signature =
                Signature::from_bytes(ed25519_dalek::Signer::sign(&signer, &message).to_bytes());
            unsigned.verify_signature().unwrap();
            assert_eq!(unsigned, expected);
        }

        phase withholding_join_material_keeps_digest_and_signature;
        {
            let (signer, _) = generate_keypair();
            let (_, joiner) = generate_keypair();
            let frame = MlsGovernanceFrame::signed(
                WorkspaceId::new(),
                [3; 32],
                2,
                MlsGovernanceAction::Commit {
                    removed: Vec::new(),
                    added: vec![joiner],
                    commit: vec![1],
                    joining: Some(Joining::Material(JoinMaterial {
                        welcome: vec![2],
                        key_history: vec![3],
                    })),
                    history: None,
                },
                &signer,
            )
            .unwrap();
            frame.check().unwrap();
            let withheld = frame.withheld().unwrap().unwrap();
            assert!(withheld.join_material().is_none());
            withheld.check().unwrap();
            assert_eq!(withheld.digest().unwrap(), frame.digest().unwrap());
        }

        phase malformed_frames_are_refused;
        {
            let (signer, signer_pk) = generate_keypair();
            let (_, other) = generate_keypair();
            let workspace = WorkspaceId::new();
            let frame = |previous, epoch, action| {
                MlsGovernanceFrame::signed(workspace, previous, epoch, action, &signer).unwrap()
            };
            let commit = |removed: Vec<Ed25519PublicKey>, added: Vec<Ed25519PublicKey>| {
                let joining = (!added.is_empty()).then_some(Joining::Withheld([1; 32]));
                MlsGovernanceAction::Commit {
                    removed,
                    added,
                    commit: vec![1],
                    joining,
                    history: None,
                }
            };
            frame([0; 32], 0, MlsGovernanceAction::Genesis)
                .check()
                .unwrap();
            frame([1; 32], 1, commit(vec![other], Vec::new()))
                .check()
                .unwrap();
            // A commit that only hands the members another branch's keys.
            frame(
                [1; 32],
                1,
                MlsGovernanceAction::Commit {
                    removed: Vec::new(),
                    added: Vec::new(),
                    commit: vec![1],
                    joining: None,
                    history: Some(vec![4]),
                },
            )
            .check()
            .unwrap();
            for malformed in [
                frame([3; 32], 0, MlsGovernanceAction::Genesis),
                frame([0; 32], 1, MlsGovernanceAction::Genesis),
                frame([1; 32], 1, commit(Vec::new(), Vec::new())),
                frame([1; 32], 1, commit(vec![other, other], Vec::new())),
                frame([1; 32], 1, commit(vec![signer_pk], Vec::new())),
                frame([1; 32], 0, commit(vec![other], Vec::new())),
                frame(
                    [1; 32],
                    1,
                    MlsGovernanceAction::Commit {
                        removed: Vec::new(),
                        added: vec![other],
                        commit: vec![1],
                        joining: None,
                        history: None,
                    },
                ),
            ] {
                assert_eq!(malformed.check(), Err(MlsGovernanceError::Malformed));
            }
        }
    }
}
