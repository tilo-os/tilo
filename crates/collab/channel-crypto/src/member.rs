//! One device's membership of one channel it does not steward.
//!
//! Every channel has a single steward device that issues its governance
//! frames (see [`crate::steward`]). Every other device that takes part in
//! the channel keeps a [`ChannelMember`]:
//!
//! 1. It starts [`Access::Waiting`], holding a member key, and hands the
//!    steward signed access requests ([`ChannelMember::access_request`])
//!    carrying fresh key packages for that key.
//! 2. It applies the steward's frames ([`ChannelMember::receive`]) once each
//!    and in chain order, whatever order the transport delivers them in: a
//!    Welcome addressed to it joins the group with the full key history,
//!    commits advance the group, and a removal ends access.
//! 3. It acknowledges cumulatively ([`ChannelMember::acknowledgement`]), so
//!    the steward can stop redelivering.
//!
//! The member keeps a mirror of the steward's journal. The mirror carries
//! the channel's plaintext gate (set by the genesis frame) and the latest
//! membership epoch, which is what the device's broker checks every
//! document op against.
//!
//! State is written before the frame is appended to the mirror, so a crash
//! between the two replays the frame on the next delivery; applying a frame
//! is idempotent by epoch.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use collab_types::{
    ChannelId, Ed25519PublicKey, MlsAccessRequestV1, MlsGovernanceAckV1, MlsGovernanceAction,
    MlsGovernanceFrameV1,
};
use ed25519_dalek::SigningKey;

use crate::governance::MirrorAppend;
use crate::persistence::MemberRecord;
use crate::{
    ChannelContentKeys, ChannelGroup, Error, GovernanceJournal, MemberKey, Result, VaultKey,
};

/// Frames held back while their predecessor is missing, per channel.
const MAX_BUFFERED_FRAMES: usize = 1024;

/// What the device can do with the channel's content right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Asked for access and not admitted yet: no content key is held, so
    /// nothing can be sealed or opened.
    Waiting,
    /// Admitted; new content seals under this epoch.
    Ready { epoch: u64 },
    /// Removed by the steward. Access resumes only after a new request is
    /// admitted.
    Removed,
}

/// What applying one delivered frame changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberEvent {
    /// The frame was already applied.
    Duplicate,
    /// The frame arrived before its predecessor and is held back.
    Buffered,
    /// The frame was recorded without changing this device's access.
    Recorded,
    /// This device joined the channel at `epoch`.
    Joined { epoch: u64 },
    /// The channel moved to `epoch`; content keys changed.
    Advanced { epoch: u64 },
    /// This device was removed.
    Removed,
    /// The frame could not be applied to this device's MLS state (it was
    /// lost or diverged). The device is waiting again and must send a new
    /// access request, which the steward answers by re-admitting it.
    Desynchronised,
}

/// One device's view of one channel stewarded by another device.
pub struct ChannelMember {
    channel: ChannelId,
    device: Ed25519PublicKey,
    steward: Ed25519PublicKey,
    journal: GovernanceJournal,
    record: MemberRecord,
    /// `None` for a member kept only in memory.
    record_path: Option<PathBuf>,
    buffered: BTreeMap<[u8; 32], MlsGovernanceFrameV1>,
}

impl ChannelMember {
    /// Open this device's state for `channel` under `dir`, or start waiting
    /// with a fresh member key when there is none.
    ///
    /// # Errors
    ///
    /// Returns an error for a corrupt or undecryptable state file or
    /// journal, or when a fresh state cannot be written.
    pub fn open(
        dir: &Path,
        channel: ChannelId,
        device: Ed25519PublicKey,
        steward: Ed25519PublicKey,
        vault: &VaultKey,
    ) -> Result<Self> {
        let record_path = dir.join(format!("{}.member", channel.0));
        let journal = GovernanceJournal::open(dir.join(format!("{}.mirror", channel.0)))?;
        let record = if record_path.exists() {
            MemberRecord::load_sealed(&record_path, vault)?
        } else {
            let record = MemberRecord::Waiting(Box::new(MemberKey::for_device(&device)?));
            record.save_sealed(&record_path, vault)?;
            record
        };
        if let MemberRecord::Joined(group) = &record
            && group.channel_id() != channel
        {
            return Err(Error::InvalidState(
                "member state names another channel".into(),
            ));
        }
        Ok(Self {
            channel,
            device,
            steward,
            journal,
            record,
            record_path: Some(record_path),
            buffered: BTreeMap::new(),
        })
    }

    /// A waiting member with a fresh member key whose state and journal
    /// mirror stay in memory, for a device with no private filesystem (a
    /// browser page). A new session starts waiting again and asks anew;
    /// the vault passed to its other methods is not used.
    ///
    /// # Errors
    ///
    /// Returns an error when the member key cannot be minted.
    pub fn in_memory(
        channel: ChannelId,
        device: Ed25519PublicKey,
        steward: Ed25519PublicKey,
    ) -> Result<Self> {
        Ok(Self {
            channel,
            device,
            steward,
            journal: GovernanceJournal::in_memory(),
            record: MemberRecord::Waiting(Box::new(MemberKey::for_device(&device)?)),
            record_path: None,
            buffered: BTreeMap::new(),
        })
    }

    /// This member's state sealed under `vault` together with its mirror
    /// of the steward's journal, for a device that keeps state in a store
    /// rather than files ([`Self::restore_in_memory`] reads it back).
    ///
    /// # Errors
    ///
    /// Returns an error when the state cannot be encoded or sealed.
    pub fn export_sealed(&self, vault: &VaultKey) -> Result<Vec<u8>> {
        let exported = ExportedMember {
            record: self.record.seal(vault)?,
            frames: self.journal.frames().to_vec(),
        };
        postcard::to_allocvec(&exported).map_err(|error| Error::Serialization(error.to_string()))
    }

    /// An in-memory member restored from [`Self::export_sealed`]: its MLS
    /// state and journal mirror as they were, so it neither asks again nor
    /// loses the keys it held.
    ///
    /// # Errors
    ///
    /// Returns an error when `bytes` do not open under `vault`, name another
    /// channel, or hold a journal that does not verify.
    pub fn restore_in_memory(
        channel: ChannelId,
        device: Ed25519PublicKey,
        steward: Ed25519PublicKey,
        bytes: &[u8],
        vault: &VaultKey,
    ) -> Result<Self> {
        let exported: ExportedMember =
            postcard::from_bytes(bytes).map_err(|error| Error::Serialization(error.to_string()))?;
        let record = MemberRecord::open_sealed(&exported.record, vault)?;
        if let MemberRecord::Joined(group) = &record
            && group.channel_id() != channel
        {
            return Err(Error::InvalidState(
                "member state names another channel".into(),
            ));
        }
        let mut journal = GovernanceJournal::in_memory();
        for frame in exported.frames {
            if !matches!(
                journal.append_mirrored(channel, steward, frame)?,
                MirrorAppend::Appended(_)
            ) {
                return Err(Error::InvalidState(
                    "exported journal is out of order".into(),
                ));
            }
        }
        Ok(Self {
            channel,
            device,
            steward,
            journal,
            record,
            record_path: None,
            buffered: BTreeMap::new(),
        })
    }

    /// The channel.
    pub fn channel_id(&self) -> ChannelId {
        self.channel
    }

    /// The device key this member acts for.
    pub fn device(&self) -> Ed25519PublicKey {
        self.device
    }

    /// The steward whose frames this member accepts.
    pub fn steward(&self) -> Ed25519PublicKey {
        self.steward
    }

    /// This device's mirror of the steward's journal.
    pub fn journal(&self) -> &GovernanceJournal {
        &self.journal
    }

    /// What the device can do with the channel's content.
    pub fn access(&self) -> Access {
        match &self.record {
            MemberRecord::Waiting(_) => Access::Waiting,
            MemberRecord::Joined(group) => Access::Ready {
                epoch: group.epoch(),
            },
            MemberRecord::Removed => Access::Removed,
        }
    }

    /// Content keys for the device's application endpoints, once admitted.
    pub fn content_keys(&self) -> Option<ChannelContentKeys> {
        match &self.record {
            MemberRecord::Joined(group) => group.content_keys().ok(),
            MemberRecord::Waiting(_) | MemberRecord::Removed => None,
        }
    }

    /// A signed access request carrying a fresh key package, or `None` when
    /// the device is already admitted. A removed device starts waiting again
    /// with a new member key: whether it is admitted is the steward's call.
    ///
    /// # Errors
    ///
    /// Returns an error when minting the key package, persisting it, or
    /// signing fails.
    pub fn access_request(
        &mut self,
        device_key: &SigningKey,
        issued_at: u64,
        vault: &VaultKey,
    ) -> Result<Option<MlsAccessRequestV1>> {
        if device_key.verifying_key().to_bytes() != *self.device.as_bytes() {
            return Err(Error::InvalidState(
                "access request signer is not this member's device".into(),
            ));
        }
        if matches!(self.record, MemberRecord::Removed) {
            self.store(
                MemberRecord::Waiting(Box::new(MemberKey::for_device(&self.device)?)),
                vault,
            )?;
        }
        let MemberRecord::Waiting(member) = &self.record else {
            return Ok(None);
        };
        let package = crate::encode_key_package(&member.fresh_key_package()?)?;
        // The package's HPKE secret now lives in the member key's storage;
        // it must be durable before the package leaves the device.
        self.save(vault)?;
        MlsAccessRequestV1::signed(self.channel, package, issued_at, device_key)
            .map(Some)
            .map_err(|error| Error::Serialization(error.to_string()))
    }

    /// The cumulative acknowledgement for everything applied so far, or
    /// `None` before the first frame.
    ///
    /// # Errors
    ///
    /// Returns an error when signing fails.
    pub fn acknowledgement(&self, device_key: &SigningKey) -> Result<Option<MlsGovernanceAckV1>> {
        if self.journal.frames().is_empty() {
            return Ok(None);
        }
        MlsGovernanceAckV1::signed(self.channel, self.journal.head_digest(), device_key)
            .map(Some)
            .map_err(|error| Error::Serialization(error.to_string()))
    }

    /// Apply one frame delivered by any transport, then every held-back frame
    /// it unblocks. Returns one event per frame applied (or one
    /// `Duplicate`/`Buffered` event).
    ///
    /// # Errors
    ///
    /// Returns an error for a frame of another channel or steward, a bad
    /// signature, or a failed write. A frame this device's MLS state cannot
    /// apply is not an error: it yields [`MemberEvent::Desynchronised`].
    pub fn receive(
        &mut self,
        frame: MlsGovernanceFrameV1,
        vault: &VaultKey,
    ) -> Result<Vec<MemberEvent>> {
        if frame.channel != self.channel {
            return Err(Error::Governance("frame names another channel".into()));
        }
        if frame.signed_by != self.steward {
            return Err(Error::Governance(
                "frame is not from this channel's steward".into(),
            ));
        }
        frame
            .verify_signature()
            .map_err(|error| Error::Governance(error.to_string()))?;
        let digest = frame
            .digest()
            .map_err(|error| Error::Serialization(error.to_string()))?;
        if self.journal.contains(digest) {
            return Ok(vec![MemberEvent::Duplicate]);
        }
        if frame.previous_frame_digest != self.journal.head_digest() {
            if self.buffered.len() < MAX_BUFFERED_FRAMES {
                self.buffered.insert(frame.previous_frame_digest, frame);
            }
            return Ok(vec![MemberEvent::Buffered]);
        }
        let mut events = vec![self.apply(frame, vault)?];
        while let Some(next) = self.buffered.remove(&self.journal.head_digest()) {
            events.push(self.apply(next, vault)?);
        }
        Ok(events)
    }

    fn apply(&mut self, frame: MlsGovernanceFrameV1, vault: &VaultKey) -> Result<MemberEvent> {
        let event = self.transition(&frame, vault)?;
        match self
            .journal
            .append_mirrored(self.channel, self.steward, frame)?
        {
            MirrorAppend::Appended(_) | MirrorAppend::Duplicate => Ok(event),
            MirrorAppend::OutOfOrder => Err(Error::InvalidState(
                "mirror head moved while applying a frame".into(),
            )),
        }
    }

    /// Apply `frame` to this device's MLS state and persist the result.
    fn transition(
        &mut self,
        frame: &MlsGovernanceFrameV1,
        vault: &VaultKey,
    ) -> Result<MemberEvent> {
        let me = self.device;
        match (&frame.action, &mut self.record) {
            (MlsGovernanceAction::Remove { removed, .. }, MemberRecord::Joined(_))
                if *removed == me =>
            {
                self.store(MemberRecord::Removed, vault)?;
                Ok(MemberEvent::Removed)
            }
            (action, MemberRecord::Waiting(pending)) => {
                let Some((recipient, welcome, _, key_history)) = action.admission() else {
                    return Ok(MemberEvent::Recorded);
                };
                if *recipient != me {
                    return Ok(MemberEvent::Recorded);
                }
                let attempt = pending.duplicate()?;
                let joined = match key_history {
                    Some(history) => ChannelGroup::process_welcome_with_history(
                        self.channel,
                        welcome,
                        history,
                        attempt,
                    ),
                    None => ChannelGroup::process_welcome(self.channel, welcome, attempt),
                };
                match joined {
                    Ok(group) => {
                        let epoch = group.epoch();
                        self.store(MemberRecord::Joined(Box::new(group)), vault)?;
                        Ok(MemberEvent::Joined { epoch })
                    }
                    // A Welcome for a key package this device no longer
                    // holds: stay waiting; the next request re-admits it.
                    Err(_) => Ok(MemberEvent::Desynchronised),
                }
            }
            (action, MemberRecord::Joined(group)) => {
                let commit = match action {
                    MlsGovernanceAction::Remove { commit, .. } => Some(commit.as_slice()),
                    other => other.admission().and_then(|(_, _, commit, _)| commit),
                };
                let Some(commit) = commit else {
                    return Ok(MemberEvent::Recorded);
                };
                if frame.epoch <= group.epoch() {
                    // Already merged: this device's own admission, or a frame
                    // replayed after a crash between state and mirror writes.
                    return Ok(MemberEvent::Recorded);
                }
                if frame.epoch != group.epoch().saturating_add(1)
                    || group.process_commit(commit).is_err()
                {
                    self.store(
                        MemberRecord::Waiting(Box::new(MemberKey::for_device(&me)?)),
                        vault,
                    )?;
                    return Ok(MemberEvent::Desynchronised);
                }
                let epoch = group.epoch();
                self.save(vault)?;
                Ok(MemberEvent::Advanced { epoch })
            }
            (_, MemberRecord::Removed) => Ok(MemberEvent::Recorded),
        }
    }

    fn store(&mut self, record: MemberRecord, vault: &VaultKey) -> Result<()> {
        if let Some(path) = &self.record_path {
            record.save_sealed(path, vault)?;
        }
        self.record = record;
        Ok(())
    }

    fn save(&self, vault: &VaultKey) -> Result<()> {
        match &self.record_path {
            Some(path) => self.record.save_sealed(path, vault),
            None => Ok(()),
        }
    }

    /// Mint a fresh key package and return the access request for it,
    /// unsigned (see [`MlsAccessRequestV1::unsigned`]), or `None` when the
    /// device is already admitted. For a device key held outside this
    /// process; [`Self::access_request`] otherwise.
    ///
    /// # Errors
    ///
    /// Returns an error when minting or persisting the key package fails.
    pub fn unsigned_access_request(
        &mut self,
        issued_at: u64,
        vault: &VaultKey,
    ) -> Result<Option<MlsAccessRequestV1>> {
        if matches!(self.record, MemberRecord::Removed) {
            self.store(
                MemberRecord::Waiting(Box::new(MemberKey::for_device(&self.device)?)),
                vault,
            )?;
        }
        let MemberRecord::Waiting(member) = &self.record else {
            return Ok(None);
        };
        let package = crate::encode_key_package(&member.fresh_key_package()?)?;
        self.save(vault)?;
        Ok(Some(MlsAccessRequestV1::unsigned(
            self.channel,
            self.device,
            package,
            issued_at,
        )))
    }
}

/// What [`ChannelMember::export_sealed`] writes.
#[derive(serde::Serialize, serde::Deserialize)]
struct ExportedMember {
    record: Vec<u8>,
    frames: Vec<MlsGovernanceFrameV1>,
}

impl std::fmt::Debug for ChannelMember {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelMember")
            .field("channel", &self.channel)
            .field("access", &self.access())
            .field("frames", &self.journal.frames().len())
            .field("buffered", &self.buffered.len())
            .finish_non_exhaustive()
    }
}
