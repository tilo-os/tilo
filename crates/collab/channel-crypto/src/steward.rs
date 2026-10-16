//! The one device that issues a channel's governance frames.
//!
//! A channel's MLS group is created by its steward, a device of whoever
//! governs the channel's room. The steward is the only issuer of commits,
//! so there is never a fork to resolve: every other device applies the
//! steward's frames in chain order ([`crate::member`]).
//!
//! The steward does not decide who may read the channel. Its caller computes
//! the device set the access policy allows for this channel (the certified
//! devices of every room member, plus live service members, less those a
//! Read Deny on the channel's record withholds) and [`ChannelSteward::reconcile`]
//! moves the MLS group there: devices that lost access are removed first,
//! then allowed devices with a pending access request are admitted.
//!
//! Every change is one frame in the governance journal, with targeted
//! deliveries for the devices that must process it. Every device asking
//! since the last change is admitted by one commit, so a channel many
//! people join at once moves one epoch per batch, not per person. The
//! journal keeps only its head frame ([`GovernanceJournal::compact`]): a
//! joiner needs only the Welcome that admits it, and a member that missed a
//! commit asks again. Deliveries are drained by redelivering
//! [`ChannelSteward::outbox`] until each device's cumulative
//! acknowledgement covers it. The sequence per change is:
//!
//! 1. stage the MLS commit and write the group with it;
//! 2. sign the frame with the device key, which may take a while (a
//!    browser's key signs through a promise);
//! 3. write the ledger: which request the device is admitted with;
//! 4. append the frame and its deliveries to the journal;
//! 5. merge the commit and write the group again.
//!
//! A crash after 4 is finished on the next open (the journal is ahead of
//! the group, so the staged commit is merged); a crash before 4 discards the
//! staged commit. A ledger ahead of the journal is harmless: a device it
//! names as admitted that is not a member is admitted when it asks, and one
//! whose entry was dropped for a removal that did not happen is removed
//! again or admitted anew. The records live in a [`StewardStore`]: files on a native
//! device, or memory exported to a browser's `IndexedDB`; [`crate::store`]
//! describes what a store must guarantee for this to hold.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use collab_types::{
    ChannelId, Ed25519PublicKey, JoinMaterial, Joining, MlsAccessRequestV1, MlsGovernanceAckV1,
    MlsGovernanceAction, MlsGovernanceContext, MlsGovernanceFrameV1,
};
use ed25519_dalek::SigningKey;
use openmls::prelude::BasicCredential;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::signer::{DeviceSigner, complete_now};
use crate::store::{FileStewardStore, StewardRecord, StewardStore};
use crate::{
    ChannelContentKeys, ChannelGroup, Error, GovernanceJournal, KeyPackage, MemberKey, Result,
    VaultKey,
};

/// Durable, secret-free bookkeeping beside the group and journal.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StewardLedger {
    /// For each admitted device, the `issued_at` of the request it was
    /// admitted with.
    admitted: BTreeMap<Ed25519PublicKey, u64>,
    /// The newest frame each device has acknowledged.
    acknowledged: BTreeMap<Ed25519PublicKey, [u8; 32]>,
    /// For each device, the digests of every key package it was admitted
    /// with. A member asking with a key package not among them lost its
    /// state and must be admitted again, whatever its request's clock says;
    /// one asking with a package already used is a replay.
    packages: BTreeMap<Ed25519PublicKey, BTreeSet<[u8; 32]>>,
}

impl StewardLedger {
    fn decode(bytes: &[u8]) -> Result<Self> {
        postcard::from_bytes(bytes).map_err(|error| Error::Serialization(error.to_string()))
    }

    fn admitted_with(&self, device: &Ed25519PublicKey, request: &MlsAccessRequestV1) -> bool {
        self.packages
            .get(device)
            .is_some_and(|used| used.contains(&package_digest(request)))
    }
}

fn package_digest(request: &MlsAccessRequestV1) -> [u8; 32] {
    use sha2::Digest as _;
    sha2::Sha256::digest(&request.key_package).into()
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

/// A steward's group, journal and ledger for one channel, kept in `S`.
pub struct ChannelSteward<S: StewardStore = FileStewardStore> {
    channel: ChannelId,
    signer: Box<dyn DeviceSigner>,
    group: ChannelGroup,
    journal: GovernanceJournal,
    ledger: StewardLedger,
    store: S,
}

impl ChannelSteward {
    /// Recover an already-persisted channel without creating a group.
    /// Uses the same crash recovery and device/channel validation as opening.
    ///
    /// # Errors
    /// Refuses missing, corrupt or undecryptable state and another channel/device.
    pub fn open_existing(
        dir: &Path,
        channel: ChannelId,
        device_key: SigningKey,
        vault: &VaultKey,
    ) -> Result<Self> {
        complete_now(Self::open_with_creation(
            FileStewardStore::new(dir, channel),
            channel,
            Box::new(device_key),
            vault,
            false,
        ))
    }

    /// Open the steward state for `channel` under `dir`, as [`Self::open`]
    /// does, signing with `device_key`.
    ///
    /// # Errors
    ///
    /// As [`Self::open`].
    pub fn open_or_create(
        dir: &Path,
        channel: ChannelId,
        device_key: SigningKey,
        vault: &VaultKey,
    ) -> Result<Self> {
        complete_now(Self::open(
            FileStewardStore::new(dir, channel),
            channel,
            Box::new(device_key),
            vault,
        ))
    }
}

impl<S: StewardStore> ChannelSteward<S> {
    /// Open the steward state for `channel` in `store`, creating the group
    /// and its genesis frame when absent, and finishing or discarding a
    /// change interrupted by a crash.
    ///
    /// # Errors
    ///
    /// Returns an error for corrupt or undecryptable state, when a fresh
    /// group cannot be written, or when the signer fails.
    pub async fn open(
        store: S,
        channel: ChannelId,
        signer: Box<dyn DeviceSigner>,
        vault: &VaultKey,
    ) -> Result<Self> {
        Self::open_with_creation(store, channel, signer, vault, true).await
    }

    async fn open_with_creation(
        mut store: S,
        channel: ChannelId,
        signer: Box<dyn DeviceSigner>,
        vault: &VaultKey,
        create: bool,
    ) -> Result<Self> {
        let device = Ed25519PublicKey::from_bytes(signer.device_public_key());
        let journal = match store.read(StewardRecord::Journal)? {
            Some(bytes) => GovernanceJournal::from_bytes(&bytes)?,
            None => GovernanceJournal::in_memory(),
        };
        let group = if let Some(sealed) = store.read(StewardRecord::Group)? {
            ChannelGroup::open_sealed(&sealed, vault)?
        } else {
            if !create {
                return Err(Error::InvalidState(
                    "existing steward group is missing".into(),
                ));
            }
            if !journal.frames().is_empty() {
                return Err(Error::InvalidState(
                    "governance journal exists without its steward group".into(),
                ));
            }
            let group = ChannelGroup::create(channel, MemberKey::for_device(&device)?)?;
            store.write(StewardRecord::Group, &group.seal(vault)?)?;
            group
        };
        if group.channel_id() != channel || group.member().name() != device.as_bytes() {
            return Err(Error::InvalidState(
                "steward group belongs to another channel or device".into(),
            ));
        }
        let ledger = match store.read(StewardRecord::Ledger)? {
            Some(bytes) => StewardLedger::decode(&bytes)?,
            None => StewardLedger::default(),
        };
        let mut steward = Self {
            channel,
            signer,
            group,
            journal,
            ledger,
            store,
        };
        steward.recover(vault)?;
        if steward.journal.frames().is_empty() {
            steward.genesis().await?;
        }
        Ok(steward)
    }

    /// The store holding this steward's records.
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Finish a change whose frame is in the journal, or drop one whose frame
    /// is not: after a crash, or after a change was abandoned while it
    /// awaited its signature.
    fn recover(&mut self, vault: &VaultKey) -> Result<()> {
        let head_epoch = self.journal.head_epoch().unwrap_or(0);
        if head_epoch > self.group.epoch() {
            if !self.group.merge_pending_transaction()? {
                return Err(Error::InvalidState(
                    "journal is ahead of the steward group with no staged change".into(),
                ));
            }
            self.save_group(vault)?;
        } else if self.group.discard_pending_transaction()? {
            self.save_group(vault)?;
        }
        Ok(())
    }

    async fn genesis(&mut self) -> Result<()> {
        let members = self.members();
        let governors = BTreeSet::from([self.device()]);
        let revision = self.next_revision();
        let frame = self
            .sign(MlsGovernanceFrameV1::unsigned(
                self.channel,
                self.transaction_id(revision),
                revision,
                self.journal.head_digest(),
                self.group.epoch(),
                MlsGovernanceAction::Genesis,
                self.device(),
            ))
            .await?;
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
        self.append(frame, &context)
    }

    /// The channel.
    pub fn channel_id(&self) -> ChannelId {
        self.channel
    }

    /// The steward's device key.
    pub fn device(&self) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(self.signer.device_public_key())
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
    /// This waits for nothing, so it needs a signer that answers at once;
    /// [`Self::reconcile_async`] takes any signer.
    ///
    /// # Errors
    ///
    /// As [`Self::reconcile_async`], or when the signer would have to be
    /// awaited.
    pub fn reconcile(
        &mut self,
        allowed: &BTreeSet<Ed25519PublicKey>,
        requests: &BTreeMap<Ed25519PublicKey, MlsAccessRequestV1>,
        vault: &VaultKey,
    ) -> Result<Reconciled> {
        complete_now(self.reconcile_async(allowed, requests, vault))
    }

    /// [`Self::reconcile`], awaiting each frame's signature.
    ///
    /// # Errors
    ///
    /// Returns an error when an MLS change, its signature or its durable
    /// record fails; the changes made before it stay committed.
    pub async fn reconcile_async(
        &mut self,
        allowed: &BTreeSet<Ed25519PublicKey>,
        requests: &BTreeMap<Ed25519PublicKey, MlsAccessRequestV1>,
        vault: &VaultKey,
    ) -> Result<Reconciled> {
        self.recover(vault)?;
        let steward = self.device();
        let mut outcome = Reconciled::default();
        for member in self.members() {
            if member != steward && !allowed.contains(&member) {
                self.remove(member, vault).await?;
                outcome.removed.push(member);
            }
        }
        let mut admissions = Vec::new();
        for (device, request) in requests {
            if *device == steward || request.device != *device || !allowed.contains(device) {
                continue;
            }
            let member = self.members().contains(device);
            if member && self.ledger.admitted_with(device, request) {
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
                self.remove(*device, vault).await?;
                outcome.removed.push(*device);
            }
            admissions.push((*device, package, request));
        }
        if !admissions.is_empty() {
            self.admit(&admissions, vault).await?;
            outcome
                .admitted
                .extend(admissions.iter().map(|(device, _, _)| *device));
        }
        Ok(outcome)
    }

    /// Admit every device in `admissions` with one commit.
    async fn admit(
        &mut self,
        admissions: &[(Ed25519PublicKey, KeyPackage, &MlsAccessRequestV1)],
        vault: &VaultKey,
    ) -> Result<()> {
        let current = self.members();
        let recipients: Vec<Ed25519PublicKey> =
            admissions.iter().map(|(device, _, _)| *device).collect();
        let mut desired = current.clone();
        desired.extend(recipients.iter().copied());
        let governors = BTreeSet::from([self.device()]);
        let revision = self.next_revision();
        let transaction = self.transaction_id(revision);
        let head = self.journal.head_digest();
        let packages: Vec<KeyPackage> = admissions
            .iter()
            .map(|(_, package, _)| package.clone())
            .collect();
        let addition = self.group.stage_add(&packages)?;
        let recorded = async {
            self.save_group(vault)?;
            let frame = self
                .sign(MlsGovernanceFrameV1::unsigned(
                    self.channel,
                    transaction,
                    revision,
                    head,
                    addition.epoch,
                    MlsGovernanceAction::AddWithKeyHistory {
                        recipients,
                        existing_member_commit: addition.commit.clone(),
                        joining: Joining::Material(JoinMaterial {
                            welcome: addition.welcome.clone(),
                            key_history: addition.key_history.clone(),
                        }),
                    },
                    self.device(),
                ))
                .await?;
            for (device, _, request) in admissions {
                self.ledger.admitted.insert(*device, request.issued_at);
                self.ledger
                    .packages
                    .entry(*device)
                    .or_default()
                    .insert(package_digest(request));
                self.ledger.acknowledged.remove(device);
            }
            self.save_ledger()?;
            self.append(
                frame,
                &MlsGovernanceContext {
                    channel: self.channel,
                    membership_revision: revision,
                    previous_frame_digest: head,
                    expected_epoch: addition.epoch,
                    governors: &governors,
                    desired_members: &desired,
                    current_mls_members: &current,
                    durable_snapshot_ack: None,
                },
            )
        }
        .await;
        self.finish(recorded, vault)
    }

    async fn remove(&mut self, device: Ed25519PublicKey, vault: &VaultKey) -> Result<()> {
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
        let (commit, epoch) = self.group.stage_remove(leaf)?;
        let recorded = async {
            self.save_group(vault)?;
            let frame = self
                .sign(MlsGovernanceFrameV1::unsigned(
                    self.channel,
                    transaction,
                    revision,
                    head,
                    epoch,
                    MlsGovernanceAction::Remove {
                        removed: device,
                        commit,
                    },
                    self.device(),
                ))
                .await?;
            self.ledger.admitted.remove(&device);
            self.save_ledger()?;
            self.append(
                frame,
                &MlsGovernanceContext {
                    channel: self.channel,
                    membership_revision: revision,
                    previous_frame_digest: head,
                    expected_epoch: epoch,
                    governors: &governors,
                    desired_members: &desired,
                    current_mls_members: &current,
                    durable_snapshot_ack: None,
                },
            )
        }
        .await;
        self.finish(recorded, vault)
    }

    /// Merge the staged commit once its frame is recorded, or drop it.
    fn finish(&mut self, recorded: Result<()>, vault: &VaultKey) -> Result<()> {
        if let Err(error) = recorded {
            // A staged group that was already written is dropped on the next
            // open or change either way, so only the original error matters.
            if self.group.discard_pending_transaction().unwrap_or(false) {
                self.save_group(vault).ok();
            }
            return Err(error);
        }
        self.group.merge_pending_transaction()?;
        self.save_group(vault)
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
        let mut journal = self.journal.clone();
        let completed = journal.acknowledge_through(ack.device, ack.through_digest)?;
        if completed > 0 {
            self.store
                .write(StewardRecord::Journal, &journal.to_bytes()?)?;
            self.journal = journal;
        }
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

    /// [`Self::acknowledge`] for many acknowledgements at once, each record
    /// written once: a channel many people joined at once is acknowledged by
    /// all of them for every change. One that is not this channel's or not
    /// validly signed is skipped. Returns how many deliveries they
    /// completed.
    ///
    /// # Errors
    ///
    /// Returns an error when a durable write fails.
    pub fn acknowledge_all(&mut self, acks: &[MlsGovernanceAckV1]) -> Result<usize> {
        let valid: Vec<(Ed25519PublicKey, [u8; 32])> = acks
            .iter()
            .filter(|ack| ack.channel == self.channel && ack.verify().is_ok())
            .map(|ack| (ack.device, ack.through_digest))
            .collect();
        if valid.is_empty() {
            return Ok(0);
        }
        let previous = self.journal.clone();
        let completed = self.journal.acknowledge_all(&valid)?;
        if completed > 0 {
            let written = self
                .journal
                .to_bytes()
                .and_then(|bytes| self.store.write(StewardRecord::Journal, &bytes));
            if let Err(error) = written {
                self.journal = previous;
                return Err(error);
            }
        }
        drop(previous);
        let positions: Vec<Option<[u8; 32]>> = self
            .journal
            .frames()
            .iter()
            .map(|frame| frame.digest().ok())
            .collect();
        let position =
            |digest: &[u8; 32]| positions.iter().position(|known| *known == Some(*digest));
        let mut moved = false;
        for (device, through) in valid {
            let Some(at) = position(&through) else {
                continue;
            };
            let newer = self
                .ledger
                .acknowledged
                .get(&device)
                .is_none_or(|known| position(known).is_none_or(|known| known < at));
            if newer {
                self.ledger.acknowledged.insert(device, through);
                moved = true;
            }
        }
        if moved {
            self.save_ledger()?;
        }
        Ok(completed)
    }

    /// Every frame some device with an unacknowledged delivery still lacks,
    /// each once, oldest first: what a transport reaching every device at
    /// once (a host retaining the channel) publishes, instead of
    /// [`Self::outbox`]'s copy per device.
    pub fn outbox_frames(&self) -> Vec<MlsGovernanceFrameV1> {
        let frames = self.journal.frames();
        let digests: Vec<Option<[u8; 32]>> =
            frames.iter().map(|frame| frame.digest().ok()).collect();
        let start = self
            .journal
            .pending_recipients()
            .iter()
            .map(|recipient| {
                self.ledger
                    .acknowledged
                    .get(recipient)
                    .and_then(|after| digests.iter().position(|digest| *digest == Some(*after)))
                    .map_or(0, |position| position.saturating_add(1))
            })
            .min();
        start
            .and_then(|start| frames.get(start..))
            .map(<[MlsGovernanceFrameV1]>::to_vec)
            .unwrap_or_default()
    }

    /// For every device with an unacknowledged delivery, the frames after
    /// the last one it acknowledged, oldest first: the head frame, as the
    /// journal keeps no other.
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

    /// One more than the head frame's: the journal is compacted, so its
    /// length no longer counts the changes made.
    fn next_revision(&self) -> u64 {
        self.journal
            .frames()
            .last()
            .map_or(1, |frame| frame.membership_revision.saturating_add(1))
    }

    fn transaction_id(&self, revision: u64) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"tilo.mls-steward.transaction.v1");
        hash.update(self.channel.0.as_bytes());
        hash.update(revision.to_le_bytes());
        hash.finalize().into()
    }

    async fn sign(&self, mut frame: MlsGovernanceFrameV1) -> Result<MlsGovernanceFrameV1> {
        let message = frame
            .signing_bytes()
            .map_err(|error| Error::Serialization(error.to_string()))?;
        frame.signature = collab_types::Signature::from_bytes(
            self.signer
                .sign(message)
                .await
                .map_err(|error| Error::Signer(error.0))?,
        );
        frame
            .verify_signature()
            .map_err(|error| Error::Signer(error.to_string()))?;
        Ok(frame)
    }

    /// Append `frame` to the journal once the journal holding it is stored.
    fn append(
        &mut self,
        frame: MlsGovernanceFrameV1,
        context: &MlsGovernanceContext<'_>,
    ) -> Result<()> {
        let mut journal = self.journal.clone();
        journal.enqueue(frame, context)?;
        journal.compact()?;
        self.store
            .write(StewardRecord::Journal, &journal.to_bytes()?)?;
        self.journal = journal;
        Ok(())
    }

    fn save_group(&mut self, vault: &VaultKey) -> Result<()> {
        let sealed = self.group.seal(vault)?;
        self.store.write(StewardRecord::Group, &sealed)
    }

    fn save_ledger(&mut self) -> Result<()> {
        let bytes = postcard::to_allocvec(&self.ledger)
            .map_err(|error| Error::Serialization(error.to_string()))?;
        self.store.write(StewardRecord::Ledger, &bytes)
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

impl<S: StewardStore> std::fmt::Debug for ChannelSteward<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelSteward")
            .field("channel", &self.channel)
            .field("epoch", &self.epoch())
            .field("members", &self.members().len())
            .field("frames", &self.journal.frames().len())
            .finish_non_exhaustive()
    }
}
