//! Durable ciphertext-only governance frame and targeted-delivery journal.

use std::fs;
use std::path::PathBuf;

use collab_types::{
    Ed25519PublicKey, EncryptionMetadata, MlsGovernanceAction, MlsGovernanceContext,
    MlsGovernanceFrameV1,
};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

const JOURNAL_VERSION: u16 = 1;
const MAX_JOURNAL_BYTES: u64 = 256 * 1024 * 1024;

/// Which opaque part of a governance frame a recipient must process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryKind {
    Welcome,
    Commit,
}

/// Durable per-endpoint delivery record. Acknowledgement is idempotent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GovernanceDelivery {
    pub frame_digest: [u8; 32],
    pub recipient: Ed25519PublicKey,
    pub kind: DeliveryKind,
    pub acknowledged: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalState {
    version: u16,
    frames: Vec<MlsGovernanceFrameV1>,
    deliveries: Vec<GovernanceDelivery>,
    encryption_required: bool,
}

/// Append-only repository journal. Each successful mutation atomically and
/// durably replaces one file containing the frame, every target, ack state,
/// and the irreversible plaintext gate.
pub struct GovernanceJournal {
    path: PathBuf,
    state: JournalState,
}

impl GovernanceJournal {
    /// Open and integrity-check a journal. Corruption never produces an empty
    /// replacement journal; only a genuinely absent file starts empty.
    ///
    /// # Errors
    ///
    /// Returns an error for I/O, decoding, version, hash-chain, signature, or
    /// dangling-delivery failures.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if !path.exists() {
            return Ok(Self {
                path,
                state: JournalState {
                    version: JOURNAL_VERSION,
                    frames: Vec::new(),
                    deliveries: Vec::new(),
                    encryption_required: false,
                },
            });
        }
        if fs::metadata(&path)?.len() > MAX_JOURNAL_BYTES {
            return Err(Error::InvalidState(
                "governance journal exceeds 256 MiB".into(),
            ));
        }
        let state: JournalState = postcard::from_bytes(&fs::read(&path)?)
            .map_err(|error| Error::Serialization(error.to_string()))?;
        if state.version != JOURNAL_VERSION {
            return Err(Error::InvalidState(
                "unsupported governance journal version".into(),
            ));
        }
        let mut previous = [0_u8; 32];
        for frame in &state.frames {
            if frame.previous_frame_digest != previous {
                return Err(Error::InvalidState("broken governance hash chain".into()));
            }
            frame
                .verify_signature()
                .map_err(|error| Error::Governance(error.to_string()))?;
            previous = frame
                .digest()
                .map_err(|error| Error::Serialization(error.to_string()))?;
        }
        for delivery in &state.deliveries {
            if !state
                .frames
                .iter()
                .any(|frame| frame.digest().ok() == Some(delivery.frame_digest))
            {
                return Err(Error::InvalidState(
                    "delivery references an unknown frame".into(),
                ));
            }
        }
        Ok(Self { path, state })
    }

    pub fn head_digest(&self) -> [u8; 32] {
        self.state
            .frames
            .last()
            .and_then(|frame| frame.digest().ok())
            .unwrap_or([0; 32])
    }

    pub fn encryption_required(&self) -> bool {
        self.state.encryption_required
    }

    /// Enforce the durable downgrade gate for a document route.
    pub fn accepts_document_encryption(
        &self,
        encryption: Option<&EncryptionMetadata>,
        channel: collab_types::ChannelId,
    ) -> bool {
        !self.state.encryption_required
            || encryption.is_some_and(|metadata| metadata.channel == channel)
    }

    /// Validate against the caller's live authoritative membership snapshot,
    /// then atomically append the frame and every required targeted delivery.
    /// The MLS payload remains opaque in `frame`.
    ///
    /// # Errors
    ///
    /// Returns an error when validation fails or the atomic write fails.
    pub fn enqueue(
        &mut self,
        frame: MlsGovernanceFrameV1,
        context: &MlsGovernanceContext<'_>,
    ) -> Result<[u8; 32]> {
        if context.previous_frame_digest != self.head_digest() {
            return Err(Error::Governance("journal head changed".into()));
        }
        frame
            .validate(context)
            .map_err(|error| Error::Governance(error.to_string()))?;
        let digest = frame
            .digest()
            .map_err(|error| Error::Serialization(error.to_string()))?;
        if self
            .state
            .frames
            .iter()
            .any(|existing| existing.digest().ok() == Some(digest))
        {
            return Err(Error::Governance("replayed governance frame".into()));
        }

        let mut next = self.state.clone();
        match &frame.action {
            MlsGovernanceAction::Add {
                recipient,
                existing_member_commit,
                ..
            } => {
                next.deliveries.push(GovernanceDelivery {
                    frame_digest: digest,
                    recipient: *recipient,
                    kind: DeliveryKind::Welcome,
                    acknowledged: false,
                });
                if existing_member_commit.is_some() {
                    for target in context.current_mls_members {
                        if target != &frame.signed_by {
                            next.deliveries.push(GovernanceDelivery {
                                frame_digest: digest,
                                recipient: *target,
                                kind: DeliveryKind::Commit,
                                acknowledged: false,
                            });
                        }
                    }
                }
            }
            MlsGovernanceAction::Remove { .. } => {
                for target in context.current_mls_members {
                    if target != &frame.signed_by {
                        next.deliveries.push(GovernanceDelivery {
                            frame_digest: digest,
                            recipient: *target,
                            kind: DeliveryKind::Commit,
                            acknowledged: false,
                        });
                    }
                }
            }
            MlsGovernanceAction::EnableEncryption { .. } => {
                next.encryption_required = true;
            }
        }
        next.frames.push(frame);
        self.persist(&next)?;
        self.state = next;
        Ok(digest)
    }

    /// Return unacknowledged records for one authenticated endpoint.
    pub fn pending_for(&self, recipient: Ed25519PublicKey) -> Vec<GovernanceDelivery> {
        self.state
            .deliveries
            .iter()
            .filter(|delivery| delivery.recipient == recipient && !delivery.acknowledged)
            .cloned()
            .collect()
    }

    /// Resolve the immutable frame for a delivery. The caller selects the
    /// Welcome or commit bytes based on [`GovernanceDelivery::kind`].
    pub fn frame(&self, digest: [u8; 32]) -> Option<&MlsGovernanceFrameV1> {
        self.state
            .frames
            .iter()
            .find(|frame| frame.digest().ok() == Some(digest))
    }

    /// Locate a previously committed idempotency key during crash recovery.
    pub fn frame_for_transaction(&self, transaction_id: [u8; 32]) -> Option<&MlsGovernanceFrameV1> {
        self.state
            .frames
            .iter()
            .find(|frame| frame.transaction_id == transaction_id)
    }

    /// Durably mark an exact endpoint delivery complete.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown delivery or failed atomic write.
    pub fn acknowledge(
        &mut self,
        digest: [u8; 32],
        recipient: Ed25519PublicKey,
        kind: DeliveryKind,
    ) -> Result<()> {
        let mut next = self.state.clone();
        let delivery = next
            .deliveries
            .iter_mut()
            .find(|delivery| {
                delivery.frame_digest == digest
                    && delivery.recipient == recipient
                    && delivery.kind == kind
            })
            .ok_or_else(|| Error::Governance("unknown targeted delivery".into()))?;
        delivery.acknowledged = true;
        self.persist(&next)?;
        self.state = next;
        Ok(())
    }

    fn persist(&self, state: &JournalState) -> Result<()> {
        let bytes = postcard::to_allocvec(state)
            .map_err(|error| Error::Serialization(error.to_string()))?;
        atomic_file::write_creating_parent_with_mode(&self.path, &bytes, 0o600).map_err(Error::Io)
    }
}

impl std::fmt::Debug for GovernanceJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GovernanceJournal")
            .field("path", &self.path)
            .field("frames", &self.state.frames.len())
            .field(
                "pending_deliveries",
                &self
                    .state
                    .deliveries
                    .iter()
                    .filter(|d| !d.acknowledged)
                    .count(),
            )
            .field("encryption_required", &self.state.encryption_required)
            .finish()
    }
}
