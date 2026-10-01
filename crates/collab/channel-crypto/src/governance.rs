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

/// What a broker checks every document op of a channel against: whether
/// plaintext is closed off, and the epoch of the latest membership change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChannelGate {
    pub encryption_required: bool,
    pub head_epoch: Option<u64>,
}

impl ChannelGate {
    /// Refuse plaintext once encryption is required, ciphertext bound to
    /// another channel, and content sealed under a content key older than
    /// the latest membership change: a removed member still holds those
    /// keys, so it must not inject content under them, and a lagging member
    /// must catch up first (otherwise the removed member could read it).
    pub fn accepts(
        &self,
        encryption: Option<&EncryptionMetadata>,
        channel: collab_types::ChannelId,
    ) -> bool {
        if let Some(metadata) = encryption
            && metadata.retention().is_some()
            && self.head_epoch.is_some_and(|head| metadata.epoch < head)
        {
            return false;
        }
        !self.encryption_required || encryption.is_some_and(|metadata| metadata.channel == channel)
    }

    /// [`Self::accepts`] for history a workspace host replays in its own
    /// order: content sealed under an older epoch was current when the host
    /// admitted it (the host admits only the room's members), so only
    /// plaintext on an encrypted channel and ciphertext bound to another
    /// channel are refused.
    pub fn accepts_history(
        &self,
        encryption: Option<&EncryptionMetadata>,
        channel: collab_types::ChannelId,
    ) -> bool {
        !self.encryption_required || encryption.is_some_and(|metadata| metadata.channel == channel)
    }
}

/// Outcome of [`GovernanceJournal::append_mirrored`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MirrorAppend {
    /// The frame extended the chain; its digest.
    Appended([u8; 32]),
    /// The frame was already in the journal.
    Duplicate,
    /// The frame's predecessor is missing; it was not stored.
    OutOfOrder,
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
    /// `None` for a journal kept only in memory.
    path: Option<PathBuf>,
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
                path: Some(path),
                ..Self::in_memory()
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
        Ok(Self {
            path: Some(path),
            state,
        })
    }

    /// An empty journal that is never written to disk, for a device with
    /// no private filesystem (a browser page); it starts over each session.
    pub fn in_memory() -> Self {
        Self {
            path: None,
            state: JournalState {
                version: JOURNAL_VERSION,
                frames: Vec::new(),
                deliveries: Vec::new(),
                encryption_required: false,
            },
        }
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

    /// The MLS epoch after the latest recorded membership change.
    pub fn head_epoch(&self) -> Option<u64> {
        self.state.frames.last().map(|frame| frame.epoch)
    }

    /// Enforce the durable downgrade gate for a document route, and refuse
    /// new content sealed under a content key older than the latest
    /// membership change: a removed member still holds those keys, so it
    /// must not be able to inject content under them, and a lagging member
    /// must catch up before its content is accepted (otherwise the removed
    /// member could read it).
    pub fn accepts_document_encryption(
        &self,
        encryption: Option<&EncryptionMetadata>,
        channel: collab_types::ChannelId,
    ) -> bool {
        self.gate().accepts(encryption, channel)
    }

    /// The journal's document ingress gate, as a value a broker can hold
    /// without the journal.
    pub fn gate(&self) -> ChannelGate {
        ChannelGate {
            encryption_required: self.state.encryption_required,
            head_epoch: self.head_epoch(),
        }
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
            action @ (MlsGovernanceAction::Add { .. }
            | MlsGovernanceAction::AddWithKeyHistory { .. }) => {
                let Some((recipient, _, existing_member_commit, _)) = action.admission() else {
                    return Err(Error::Governance("malformed admission".into()));
                };
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
            MlsGovernanceAction::EnableEncryption { .. } | MlsGovernanceAction::Genesis => {
                next.encryption_required = true;
            }
        }
        next.frames.push(frame);
        self.persist(&next)?;
        self.state = next;
        Ok(digest)
    }

    /// Append a frame received from the channel's steward to this device's
    /// mirror of the journal. The mirror holds the same hash chain as the
    /// steward's journal but no delivery records: its only use is to apply
    /// frames once each and in order, and to gate document ingress.
    ///
    /// A frame already present is [`MirrorAppend::Duplicate`]; one whose
    /// predecessor has not arrived yet is [`MirrorAppend::OutOfOrder`] and
    /// is not stored. Neither is an error, so redelivery is harmless.
    ///
    /// # Errors
    ///
    /// Returns an error for a frame of another channel, one not signed by
    /// `steward`, a bad signature, or a failed atomic write.
    pub fn append_mirrored(
        &mut self,
        channel: collab_types::ChannelId,
        steward: Ed25519PublicKey,
        frame: MlsGovernanceFrameV1,
    ) -> Result<MirrorAppend> {
        if frame.channel != channel {
            return Err(Error::Governance(
                "governance frame names another channel".into(),
            ));
        }
        if frame.signed_by != steward {
            return Err(Error::Governance(
                "governance frame is not signed by the channel steward".into(),
            ));
        }
        frame
            .verify_signature()
            .map_err(|error| Error::Governance(error.to_string()))?;
        let digest = frame
            .digest()
            .map_err(|error| Error::Serialization(error.to_string()))?;
        if self.contains(digest) {
            return Ok(MirrorAppend::Duplicate);
        }
        if frame.previous_frame_digest != self.head_digest() {
            return Ok(MirrorAppend::OutOfOrder);
        }
        let mut next = self.state.clone();
        if matches!(
            frame.action,
            MlsGovernanceAction::EnableEncryption { .. } | MlsGovernanceAction::Genesis
        ) {
            next.encryption_required = true;
        }
        next.frames.push(frame);
        self.persist(&next)?;
        self.state = next;
        Ok(MirrorAppend::Appended(digest))
    }

    /// Whether a frame with this digest is in the journal.
    pub fn contains(&self, digest: [u8; 32]) -> bool {
        self.state
            .frames
            .iter()
            .any(|frame| frame.digest().ok() == Some(digest))
    }

    /// Every frame, oldest first.
    pub fn frames(&self) -> &[MlsGovernanceFrameV1] {
        &self.state.frames
    }

    /// Frames after the one with digest `after` (all frames for the all-zero
    /// digest), oldest first; `None` when `after` is not in the journal.
    pub fn frames_after(&self, after: [u8; 32]) -> Option<&[MlsGovernanceFrameV1]> {
        if after == [0; 32] {
            return Some(&self.state.frames);
        }
        let position = self
            .state
            .frames
            .iter()
            .position(|frame| frame.digest().ok() == Some(after))?;
        self.state.frames.get(position.saturating_add(1)..)
    }

    /// Recipients with at least one unacknowledged delivery.
    pub fn pending_recipients(&self) -> std::collections::BTreeSet<Ed25519PublicKey> {
        self.state
            .deliveries
            .iter()
            .filter(|delivery| !delivery.acknowledged)
            .map(|delivery| delivery.recipient)
            .collect()
    }

    /// Apply a cumulative acknowledgement: `recipient` has processed every
    /// frame up to and including `through`. Returns how many deliveries it
    /// completed; an acknowledgement that completes nothing new writes
    /// nothing.
    ///
    /// # Errors
    ///
    /// Returns an error when `through` is not in the journal or the atomic
    /// write fails.
    pub fn acknowledge_through(
        &mut self,
        recipient: Ed25519PublicKey,
        through: [u8; 32],
    ) -> Result<usize> {
        let position = self
            .state
            .frames
            .iter()
            .position(|frame| frame.digest().ok() == Some(through))
            .ok_or_else(|| Error::Governance("acknowledged frame is unknown".into()))?;
        let covered: std::collections::BTreeSet<[u8; 32]> = self
            .state
            .frames
            .iter()
            .take(position.saturating_add(1))
            .filter_map(|frame| frame.digest().ok())
            .collect();
        let mut next = self.state.clone();
        let mut completed: usize = 0;
        for delivery in &mut next.deliveries {
            if delivery.recipient == recipient
                && !delivery.acknowledged
                && covered.contains(&delivery.frame_digest)
            {
                delivery.acknowledged = true;
                completed = completed.saturating_add(1);
            }
        }
        if completed > 0 {
            self.persist(&next)?;
            self.state = next;
        }
        Ok(completed)
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
        let Some(path) = &self.path else {
            return Ok(());
        };
        let bytes = postcard::to_allocvec(state)
            .map_err(|error| Error::Serialization(error.to_string()))?;
        tilo_files::native::atomic::write_creating_parent_with_mode(path, &bytes, 0o600)
            .map_err(Error::Io)
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

#[cfg(test)]
mod gate_tests {
    use super::ChannelGate;
    use collab_types::{ChannelId, ContentRetention, EncryptionMetadata};

    /// Live content sealed under a key older than the latest membership
    /// change is refused (a removed member still holds it); the same content
    /// replayed by the host from its history was current when admitted and
    /// is accepted. Plaintext and other channels are refused either way.
    #[test]
    fn host_history_under_an_older_epoch_is_accepted_live_content_is_not() {
        let channel = ChannelId::new();
        let gate = ChannelGate {
            encryption_required: true,
            head_epoch: Some(3),
        };
        let old = EncryptionMetadata::content_key(1, channel, ContentRetention::Delta);
        assert!(!gate.accepts(Some(&old), channel));
        assert!(gate.accepts_history(Some(&old), channel));
        assert!(!gate.accepts_history(None, channel));
        let other = EncryptionMetadata::content_key(3, ChannelId::new(), ContentRetention::Delta);
        assert!(!gate.accepts_history(Some(&other), channel));
    }
}
