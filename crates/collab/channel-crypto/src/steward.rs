//! The one device that issues a channel's governance frames.
//!
//! A channel's MLS group is created by its steward, a device of whoever
//! governs the channel's room. The steward is the only issuer of commits,
//! so there is never a fork to resolve: every other device applies the
//! steward's frames in chain order ([`crate::member`]).
//!
//! The steward does not decide who may read the channel. Its caller computes
//! the device set the access policy allows (the certified devices of every
//! room member, plus live service members) and [`ChannelSteward::reconcile`]
//! moves the MLS group there: devices that lost access are removed first,
//! then allowed devices with a pending access request are admitted.
//!
//! Every change is one frame in the governance journal, with targeted
//! deliveries for the devices that must process it. Deliveries are drained
//! by redelivering [`ChannelSteward::outbox`] until each device's cumulative
//! acknowledgement covers it. The sequence per change is:
//!
//! 1. stage the MLS commit;
//! 2. write the group with the staged commit;
//! 3. append the frame and its deliveries to the journal;
//! 4. merge the commit and write the group again.
//!
//! A crash after 3 is finished on the next open (the journal is ahead of
//! the group, so the staged commit is merged); a crash before 3 discards the
//! staged commit.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use collab_types::{
    ChannelId, Ed25519PublicKey, MlsAccessRequestV1, MlsGovernanceAckV1, MlsGovernanceAction,
    MlsGovernanceContext, MlsGovernanceFrameV1,
};
use ed25519_dalek::SigningKey;
use openmls::prelude::BasicCredential;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    ChannelContentKeys, ChannelGroup, Error, GovernanceJournal, KeyPackage, MemberKey, Result,
    VaultKey,
};

/// Durable, secret-free bookkeeping beside the group and journal.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StewardLedger {
    /// For each admitted device, the `issued_at` of the request it was
    /// admitted with. A newer request from a member means it lost its state
    /// and must be admitted again.
    admitted: BTreeMap<Ed25519PublicKey, u64>,
    /// The newest frame each device has acknowledged.
    acknowledged: BTreeMap<Ed25519PublicKey, [u8; 32]>,
}

/// What [`ChannelSteward::reconcile`] changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reconciled {
    pub admitted: Vec<Ed25519PublicKey>,
    /// Devices removed, including members removed to be admitted again.
    pub removed: Vec<Ed25519PublicKey>,
    /// Requests refused because their key package was malformed or named
    /// another device.
    pub refused: Vec<Ed25519PublicKey>,
}

impl Reconciled {
    /// Whether the group changed.
    pub fn changed(&self) -> bool {
        !self.admitted.is_empty() || !self.removed.is_empty()
    }
}

/// The governance frames one device still has to acknowledge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outgoing {
    pub recipient: Ed25519PublicKey,
    pub frames: Vec<MlsGovernanceFrameV1>,
}

/// A steward's group, journal and ledger for one channel.
pub struct ChannelSteward {
    channel: ChannelId,
    device_key: SigningKey,
    group: ChannelGroup,
    journal: GovernanceJournal,
    ledger: StewardLedger,
    group_path: PathBuf,
    ledger_path: PathBuf,
}

impl ChannelSteward {
    /// Open the steward state for `channel` under `dir`, creating the group
    /// and its genesis frame when absent, and finishing or discarding a
    /// change interrupted by a crash.
    ///
    /// # Errors
    ///
    /// Returns an error for corrupt or undecryptable state, or when a fresh
    /// group cannot be written.
    pub fn open_or_create(
        dir: &Path,
        channel: ChannelId,
        device_key: SigningKey,
        vault: &VaultKey,
    ) -> Result<Self> {
        let group_path = dir.join(format!("{}.group", channel.0));
        let ledger_path = dir.join(format!("{}.steward", channel.0));
        let journal = GovernanceJournal::open(dir.join(format!("{}.governance", channel.0)))?;
        let device = Ed25519PublicKey::from_bytes(device_key.verifying_key().to_bytes());
        let group = if group_path.exists() {
            ChannelGroup::load_sealed(&group_path, vault)?
        } else {
            if !journal.frames().is_empty() {
                return Err(Error::InvalidState(
                    "governance journal exists without its steward group".into(),
                ));
            }
            let group = ChannelGroup::create(channel, MemberKey::for_device(&device)?)?;
            group.save_sealed(&group_path, vault)?;
            group
        };
        if group.channel_id() != channel || group.member().name() != device.as_bytes() {
            return Err(Error::InvalidState(
                "steward group belongs to another channel or device".into(),
            ));
        }
        let ledger = if ledger_path.exists() {
            postcard::from_bytes(&std::fs::read(&ledger_path)?)
                .map_err(|error| Error::Serialization(error.to_string()))?
        } else {
            StewardLedger::default()
        };
        let mut steward = Self {
            channel,
            device_key,
            group,
            journal,
            ledger,
            group_path,
            ledger_path,
        };
        steward.recover(vault)?;
        if steward.journal.frames().is_empty() {
            steward.genesis()?;
        }
        Ok(steward)
    }

    fn recover(&mut self, vault: &VaultKey) -> Result<()> {
        let head_epoch = self.journal.head_epoch().unwrap_or(0);
        if head_epoch > self.group.epoch() {
            if !self.group.merge_pending_transaction()? {
                return Err(Error::InvalidState(
                    "journal is ahead of the steward group with no staged change".into(),
                ));
            }
            self.group.save_sealed(&self.group_path, vault)?;
        } else if self.group.discard_pending_transaction()? {
            self.group.save_sealed(&self.group_path, vault)?;
        }
        Ok(())
    }

    fn genesis(&mut self) -> Result<()> {
        let members = self.members();
        let governors = BTreeSet::from([self.device()]);
        let revision = self.next_revision();
        let frame = MlsGovernanceFrameV1::signed(
            self.channel,
            self.transaction_id(revision),
            revision,
            self.journal.head_digest(),
            self.group.epoch(),
            MlsGovernanceAction::Genesis,
            &self.device_key,
        )
        .map_err(|error| Error::Serialization(error.to_string()))?;
        let context = MlsGovernanceContext {
            channel: self.channel,
            membership_revision: revision,
            previous_frame_digest: self.journal.head_digest(),
            expected_epoch: self.group.epoch(),
            governors: &governors,
            desired_members: &members,
            current_mls_members: &members,
            durable_snapshot_ack: None,
        };
        self.journal.enqueue(frame, &context)?;
        Ok(())
    }

    /// The channel.
    pub fn channel_id(&self) -> ChannelId {
        self.channel
    }

    /// The steward's device key.
    pub fn device(&self) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(self.device_key.verifying_key().to_bytes())
    }

    /// The governance journal (the channel's plaintext gate and head epoch).
    pub fn journal(&self) -> &GovernanceJournal {
        &self.journal
    }

    /// The current epoch.
    pub fn epoch(&self) -> u64 {
        self.group.epoch()
    }

    /// Every device in the group, the steward included.
    pub fn members(&self) -> BTreeSet<Ed25519PublicKey> {
        self.group
            .member_identities()
            .into_iter()
            .filter_map(|(_, identity)| {
                <[u8; 32]>::try_from(identity.as_slice())
                    .ok()
                    .map(Ed25519PublicKey::from_bytes)
            })
            .collect()
    }

    /// Content keys for the steward device's own application endpoints.
    ///
    /// # Errors
    ///
    /// As [`ChannelGroup::content_keys`].
    pub fn content_keys(&self) -> Result<ChannelContentKeys> {
        self.group.content_keys()
    }

    /// Move the group to `allowed` (the steward is always kept): remove
    /// every member not allowed, then admit every allowed device that has a
    /// request in `requests` and is not a member, or is a member asking again
    /// with a newer request because it lost its state.
    ///
    /// # Errors
    ///
    /// Returns an error when an MLS change or its durable record fails; the
    /// changes made before it stay committed.
    pub fn reconcile(
        &mut self,
        allowed: &BTreeSet<Ed25519PublicKey>,
        requests: &BTreeMap<Ed25519PublicKey, MlsAccessRequestV1>,
        vault: &VaultKey,
    ) -> Result<Reconciled> {
        let steward = self.device();
        let mut outcome = Reconciled::default();
        for member in self.members() {
            if member != steward && !allowed.contains(&member) {
                self.remove(member, vault)?;
                outcome.removed.push(member);
            }
        }
        for (device, request) in requests {
            if *device == steward || request.device != *device || !allowed.contains(device) {
                continue;
            }
            let member = self.members().contains(device);
            let newer = self
                .ledger
                .admitted
                .get(device)
                .is_none_or(|admitted| request.issued_at > *admitted);
            if member && !newer {
                continue;
            }
            if request.channel != self.channel || request.verify().is_err() {
                outcome.refused.push(*device);
                continue;
            }
            let Ok(package) = device_key_package(request) else {
                outcome.refused.push(*device);
                continue;
            };
            if member {
                self.remove(*device, vault)?;
                outcome.removed.push(*device);
            }
            self.admit(*device, package, request.issued_at, vault)?;
            outcome.admitted.push(*device);
        }
        Ok(outcome)
    }

    fn admit(
        &mut self,
        device: Ed25519PublicKey,
        package: KeyPackage,
        issued_at: u64,
        vault: &VaultKey,
    ) -> Result<()> {
        let current = self.members();
        let mut desired = current.clone();
        desired.insert(device);
        let governors = BTreeSet::from([self.device()]);
        let revision = self.next_revision();
        let transaction = self.transaction_id(revision);
        let head = self.journal.head_digest();
        let (channel, group_path) = (self.channel, self.group_path.clone());
        let (journal, signer) = (&mut self.journal, &self.device_key);
        self.group
            .add_member_transactional(package, |group, addition| {
                group.save_sealed(&group_path, vault)?;
                let frame = MlsGovernanceFrameV1::signed(
                    channel,
                    transaction,
                    revision,
                    head,
                    addition.epoch,
                    MlsGovernanceAction::AddWithKeyHistory {
                        recipient: device,
                        welcome: addition.welcome.clone(),
                        existing_member_commit: addition.commit.clone(),
                        key_history: addition.key_history.clone(),
                    },
                    signer,
                )
                .map_err(|error| Error::Serialization(error.to_string()))?;
                journal.enqueue(
                    frame,
                    &MlsGovernanceContext {
                        channel,
                        membership_revision: revision,
                        previous_frame_digest: head,
                        expected_epoch: addition.epoch,
                        governors: &governors,
                        desired_members: &desired,
                        current_mls_members: &current,
                        durable_snapshot_ack: None,
                    },
                )?;
                Ok(())
            })?;
        self.group.save_sealed(&self.group_path, vault)?;
        self.ledger.admitted.insert(device, issued_at);
        self.ledger.acknowledged.remove(&device);
        self.save_ledger()
    }

    fn remove(&mut self, device: Ed25519PublicKey, vault: &VaultKey) -> Result<()> {
        let leaf = self
            .group
            .leaf_index_of(device.as_bytes())
            .ok_or_else(|| Error::InvalidState("device is not a member".into()))?;
        let current = self.members();
        let mut desired = current.clone();
        desired.remove(&device);
        let governors = BTreeSet::from([self.device()]);
        let revision = self.next_revision();
        let transaction = self.transaction_id(revision);
        let head = self.journal.head_digest();
        let (channel, group_path) = (self.channel, self.group_path.clone());
        let (journal, signer) = (&mut self.journal, &self.device_key);
        self.group
            .remove_member_transactional(leaf, |group, commit, epoch| {
                group.save_sealed(&group_path, vault)?;
                let frame = MlsGovernanceFrameV1::signed(
                    channel,
                    transaction,
                    revision,
                    head,
                    epoch,
                    MlsGovernanceAction::Remove {
                        removed: device,
                        commit: commit.to_vec(),
                    },
                    signer,
                )
                .map_err(|error| Error::Serialization(error.to_string()))?;
                journal.enqueue(
                    frame,
                    &MlsGovernanceContext {
                        channel,
                        membership_revision: revision,
                        previous_frame_digest: head,
                        expected_epoch: epoch,
                        governors: &governors,
                        desired_members: &desired,
                        current_mls_members: &current,
                        durable_snapshot_ack: None,
                    },
                )?;
                Ok(())
            })?;
        self.group.save_sealed(&self.group_path, vault)?;
        self.ledger.admitted.remove(&device);
        self.save_ledger()
    }

    /// Record a device's cumulative acknowledgement. Returns how many
    /// deliveries it completed. An acknowledgement for a frame this journal
    /// does not hold (from before a state reset) completes nothing.
    ///
    /// # Errors
    ///
    /// Returns an error for a bad signature or another channel, or a failed
    /// write.
    pub fn acknowledge(&mut self, ack: &MlsGovernanceAckV1) -> Result<usize> {
        if ack.channel != self.channel {
            return Err(Error::Governance(
                "acknowledgement names another channel".into(),
            ));
        }
        ack.verify()
            .map_err(|error| Error::Governance(error.to_string()))?;
        if !self.journal.contains(ack.through_digest) {
            return Ok(0);
        }
        let completed = self
            .journal
            .acknowledge_through(ack.device, ack.through_digest)?;
        let position = |digest: &[u8; 32]| {
            self.journal
                .frames()
                .iter()
                .position(|frame| frame.digest().ok().as_ref() == Some(digest))
        };
        let newer = self
            .ledger
            .acknowledged
            .get(&ack.device)
            .is_none_or(|known| position(known) < position(&ack.through_digest));
        if newer {
            self.ledger
                .acknowledged
                .insert(ack.device, ack.through_digest);
            self.save_ledger()?;
        }
        Ok(completed)
    }

    /// For every device with an unacknowledged delivery, the frames after
    /// the last one it acknowledged, oldest first. A device that never
    /// acknowledged gets the whole chain, so it can verify it from genesis.
    pub fn outbox(&self) -> Vec<Outgoing> {
        self.journal
            .pending_recipients()
            .into_iter()
            .filter_map(|recipient| {
                let after = self
                    .ledger
                    .acknowledged
                    .get(&recipient)
                    .copied()
                    .unwrap_or([0; 32]);
                let frames = self
                    .journal
                    .frames_after(after)
                    .unwrap_or_else(|| self.journal.frames())
                    .to_vec();
                (!frames.is_empty()).then_some(Outgoing { recipient, frames })
            })
            .collect()
    }

    fn next_revision(&self) -> u64 {
        u64::try_from(self.journal.frames().len())
            .unwrap_or(u64::MAX)
            .saturating_add(1)
    }

    fn transaction_id(&self, revision: u64) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"tilo.mls-steward.transaction.v1");
        hash.update(self.channel.0.as_bytes());
        hash.update(revision.to_le_bytes());
        hash.finalize().into()
    }

    fn save_ledger(&self) -> Result<()> {
        let bytes = postcard::to_allocvec(&self.ledger)
            .map_err(|error| Error::Serialization(error.to_string()))?;
        tilo_files::native::atomic::write_private(&self.ledger_path, &bytes).map_err(Error::Io)
    }
}

/// Decode a request's key package and check its credential names the
/// requesting device.
fn device_key_package(request: &MlsAccessRequestV1) -> Result<KeyPackage> {
    let package = crate::decode_key_package(&request.key_package)?;
    let credential = BasicCredential::try_from(package.leaf_node().credential().clone())
        .map_err(Error::openmls)?;
    if credential.identity() != request.device.as_bytes() {
        return Err(Error::Governance(
            "key package credential names another device".into(),
        ));
    }
    Ok(package)
}

impl std::fmt::Debug for ChannelSteward {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelSteward")
            .field("channel", &self.channel)
            .field("epoch", &self.epoch())
            .field("members", &self.members().len())
            .field("frames", &self.journal.frames().len())
            .finish_non_exhaustive()
    }
}
