//! `ChannelGroup` — one MLS group per channel.
//!
//! The runtime view a single device has of one channel's MLS group: it
//! owns the [`MlsGroup`] state, the [`MemberKey`] secret material this
//! device participates with, and the underlying provider. From this view
//! it encrypts outgoing payloads under the current epoch key, decrypts
//! incoming ones, and admits new members by emitting a Welcome.
//!
//! **Single-tenant**: one device, one membership. Two devices of the
//! same human are two separate `ChannelGroup` instances; so are two
//! channels. Sharing a provider between channels is not supported.
//!
//! ## Wire shapes
//!
//! All output bytes are TLS-CODEC-serialised `MlsMessageOut` envelopes.
//! The broker routes them; this crate stamps no metadata over them — the
//! companion `EncryptionMetadata` field on `collab_types::SignedOp`
//! names the channel + epoch so the receiver picks the right
//! `ChannelGroup` before calling [`ChannelGroup::decrypt`].

use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::*;

use collab_types::{ArtifactId, ChannelId, ContentRetention, EncryptionMetadata, EncryptionScheme};
use tilo_seal::SymmetricKey;
use zeroize::Zeroizing;

use crate::content::{
    CONTENT_KEY_EXPORTER_LABEL, ContentKeyring, HISTORY_TRANSPORT_EXPORTER_LABEL, KeyHistory,
};
use crate::crypto::{CIPHERSUITE, MemberKey};
use crate::errors::{Error, Result};

/// Everything an admission produces, handed to the caller's persistence
/// callback before the local epoch advances.
#[derive(Clone, Debug)]
pub struct MemberAddition {
    /// TLS-serialised MLS Welcome for the joiner.
    pub welcome: Vec<u8>,
    /// Commit existing members (other than the adder) must process; `None`
    /// for the very first join, when there is no other member.
    pub commit: Option<Vec<u8>>,
    /// The joiner's content-key history ([`KeyHistory::to_bytes`]): every
    /// content key the adder holds, sealed for the post-add epoch.
    pub key_history: Vec<u8>,
    /// The epoch the group moves to.
    pub epoch: u64,
}

/// One device's view of one channel's MLS group.
///
/// Holds `MemberKey` by value (move-in on `create`/`process_welcome`) —
/// the secret material in the member is what makes the group state
/// usable, so separating them would invite "wrong signer for this group" bugs.
pub struct ChannelGroup {
    /// The Tilo channel this group represents, so callers can route incoming
    /// ops via [`channel_id`](Self::channel_id) without a side table.
    pub(crate) channel_id: ChannelId,
    /// Secret material + provider — the group state lives in
    /// `member.provider`; do not allocate a second provider.
    pub(crate) member: MemberKey,
    /// The openmls group state, mutated in place, kept private so callers
    /// can't reach around the wrapper (e.g. commit without going through
    /// this crate).
    pub(crate) group: MlsGroup,
    /// Every content key this member holds (see [`crate::content`]).
    pub(crate) keyring: ContentKeyring,
}

/// `MlsGroupCreateConfig` toggles always on: `use_ratchet_tree_extension(true)`
/// makes every Welcome carry the ratchet tree inline, so a joiner needs
/// no out-of-band tree — simpler, and the size penalty is small at
/// human-scale channel sizes.
fn group_create_config() -> MlsGroupCreateConfig {
    MlsGroupCreateConfig::builder()
        .use_ratchet_tree_extension(true)
        .build()
}

/// Mirror of [`group_create_config`] for the joining side.
fn group_join_config() -> MlsGroupJoinConfig {
    MlsGroupJoinConfig::builder()
        .use_ratchet_tree_extension(true)
        .build()
}

impl ChannelGroup {
    /// Bootstrap a one-member channel. `owner` becomes the sole member; its
    /// `MemberKey`'s provider takes ownership of the group state. The
    /// returned `ChannelGroup` is usable on its own — further members via
    /// [`add_member`](Self::add_member).
    ///
    /// # Errors
    ///
    /// Returns [`Error::OpenMls`] if the underlying `MlsGroup::new` call fails.
    pub fn create(channel_id: ChannelId, owner: MemberKey) -> Result<Self> {
        let group = MlsGroup::new(
            &owner.provider,
            &owner.signer,
            &group_create_config(),
            owner.credential_with_key(),
        )
        .map_err(Error::openmls)?;
        let mut created = Self {
            channel_id,
            member: owner,
            group,
            keyring: ContentKeyring::new(),
        };
        created.record_epoch_key()?;
        Ok(created)
    }

    /// Derive the current epoch's content key from the MLS exporter and add
    /// it to the keyring. A member removed by the last commit is inactive and
    /// learns nothing new.
    pub(crate) fn record_epoch_key(&mut self) -> Result<()> {
        if !self.group.is_active() {
            return Ok(());
        }
        let key = self.export_key(CONTENT_KEY_EXPORTER_LABEL, self.channel_id.0.as_bytes())?;
        self.keyring.insert(self.epoch(), key)
    }

    fn export_key(&self, label: &str, context: &[u8]) -> Result<SymmetricKey> {
        let exported = Zeroizing::new(
            self.group
                .export_secret(
                    openmls_traits::OpenMlsProvider::crypto(&self.member.provider),
                    label,
                    context,
                    32,
                )
                .map_err(Error::openmls)?,
        );
        SymmetricKey::from_slice(&exported).map_err(Error::ContentCrypto)
    }

    fn history_transport_context(&self, epoch: u64) -> [u8; 24] {
        crate::content::history_record(self.channel_id, epoch)
    }

    /// Every content key this member holds.
    pub fn keyring(&self) -> &ContentKeyring {
        &self.keyring
    }

    /// Seal an op payload for `artifact` under the current epoch's content
    /// key (encryption scheme version 2). The returned metadata goes into
    /// `SignedOp.encryption`.
    ///
    /// # Errors
    ///
    /// [`Error::MissingContentKey`] if the current epoch has no key (the
    /// member was removed), or a sealing error.
    pub fn seal_content(
        &self,
        artifact: ArtifactId,
        retention: ContentRetention,
        plaintext: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>)> {
        if !self.group.is_active() {
            return Err(Error::MissingContentKey(self.epoch()));
        }
        self.keyring.seal(
            self.channel_id,
            self.epoch(),
            artifact,
            retention,
            plaintext,
        )
    }

    /// Open an op payload of any supported encryption scheme: version 2
    /// content-key envelopes through the keyring (any epoch this member holds
    /// a key for, any number of times), version 1 MLS application messages
    /// through the MLS ratchet (current epoch only, once).
    ///
    /// # Errors
    ///
    /// [`Error::MissingContentKey`], [`Error::ContentCrypto`] or the MLS
    /// decryption errors of [`Self::decrypt`].
    pub fn open_content(
        &mut self,
        artifact: ArtifactId,
        metadata: &EncryptionMetadata,
        payload: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>> {
        if metadata.channel != self.channel_id {
            return Err(Error::InvalidState("payload names another channel".into()));
        }
        match metadata.scheme {
            EncryptionScheme::ContentKey { .. } => {
                self.keyring
                    .open(self.channel_id, artifact, metadata, payload)
            }
            EncryptionScheme::MlsApplication => {
                if metadata.epoch > self.epoch() {
                    return Err(Error::MissingContentKey(metadata.epoch));
                }
                self.decrypt(payload).map(Zeroizing::new)
            }
        }
    }

    /// Seal this member's complete key history for the current epoch, so a
    /// member who joined at this epoch (and missed the admission's bundle)
    /// can be handed it again by anyone currently in the group.
    ///
    /// # Errors
    ///
    /// Export, sealing or encoding errors.
    pub fn current_key_history(&self) -> Result<Vec<u8>> {
        let epoch = self.epoch();
        let transport = self.export_key(
            HISTORY_TRANSPORT_EXPORTER_LABEL,
            &self.history_transport_context(epoch),
        )?;
        KeyHistory::seal(self.channel_id, epoch, &transport, &self.keyring)?.to_bytes()
    }

    /// Import a key history sealed for the current epoch (from
    /// [`MemberAddition::key_history`] or [`Self::current_key_history`]).
    ///
    /// # Errors
    ///
    /// Decoding, authentication, or key-conflict errors; the keyring is
    /// unchanged on error.
    pub fn import_key_history(&mut self, bytes: &[u8]) -> Result<()> {
        let history = KeyHistory::from_bytes(bytes)?;
        if history.epoch != self.epoch() {
            return Err(Error::InvalidState(format!(
                "key history sealed for epoch {}, group is at {}",
                history.epoch,
                self.epoch()
            )));
        }
        let transport = self.export_key(
            HISTORY_TRANSPORT_EXPORTER_LABEL,
            &self.history_transport_context(history.epoch),
        )?;
        let received = history.open(self.channel_id, &transport)?;
        self.keyring.merge(&received)
    }

    /// The channel this group encrypts for.
    pub fn channel_id(&self) -> ChannelId {
        self.channel_id
    }

    /// The MLS epoch counter, mirrored into the wire
    /// `EncryptionMetadata.epoch`. Each membership change (add, remove, key
    /// update) increments it. Ciphertexts from epoch *N* must be decrypted
    /// by a group at epoch *N* or later (older accepted up to openmls's
    /// configurable window; ciphertext from a *future* epoch is dropped
    /// until the recipient catches up).
    pub fn epoch(&self) -> u64 {
        self.group.epoch().as_u64()
    }

    /// Number of members in the current epoch. Cheap — openmls stores the
    /// count in the leaf-node vector; useful in tests for asserting "alice
    /// added bob → 2" without unpacking the full group state.
    pub fn member_count(&self) -> usize {
        // openmls's MlsGroup exposes `members()` iterating leaf nodes;
        // count rather than collect to avoid the allocation.
        self.group.members().count()
    }

    /// Whether this member still belongs to the group. A member removed by
    /// a processed commit is inactive: it derives no further content keys.
    pub fn is_active(&self) -> bool {
        self.group.is_active()
    }

    /// Every member's leaf index and credential identity (for devices, the
    /// 32-byte certified device key), in leaf order.
    pub fn member_identities(&self) -> Vec<(u32, Vec<u8>)> {
        self.group
            .members()
            .map(|member| {
                (
                    member.index.u32(),
                    BasicCredential::try_from(member.credential)
                        .map(|credential| credential.identity().to_vec())
                        .unwrap_or_default(),
                )
            })
            .collect()
    }

    /// The leaf of the member whose credential identity is `identity`.
    pub fn leaf_index_of(&self, identity: &[u8]) -> Option<u32> {
        self.member_identities()
            .into_iter()
            .find_map(|(leaf, member)| (member == identity).then_some(leaf))
    }

    /// What an application endpoint needs to seal and open this channel's
    /// content: the current epoch and every content key held. It carries
    /// none of the MLS signing, HPKE or ratchet secrets, so handing it to an
    /// app process never lets that process change membership.
    ///
    /// # Errors
    ///
    /// [`Error::MissingContentKey`] when this member was removed.
    pub fn content_keys(&self) -> Result<crate::ChannelContentKeys> {
        if !self.group.is_active() {
            return Err(Error::MissingContentKey(self.epoch()));
        }
        Ok(crate::ChannelContentKeys::new(
            self.channel_id,
            self.epoch(),
            self.keyring.clone(),
        ))
    }

    /// Read-only borrow of the underlying member key, so the broker can look
    /// up which identity public key this group operates under without
    /// re-deriving it from the attestation store.
    pub fn member(&self) -> &MemberKey {
        &self.member
    }

    /// Add a new member to the group. `candidate_key_package` is the key
    /// package the candidate generated via
    /// [`MemberKey::fresh_key_package`] and shipped out-of-band to whichever
    /// member is authorised to call this (enforced upstream by the
    /// collab-types role table).
    ///
    /// On success: the local group state advances one epoch via
    /// `merge_pending_commit`; a TLS-serialised `Welcome` is returned; the
    /// caller must broadcast the Welcome to the new member and the
    /// accompanying *commit* to existing members so they advance too.
    ///
    /// This convenience is valid only for the first join, when there is no
    /// pre-existing peer that needs the commit. Once the group has two or more
    /// members it refuses; callers must use
    /// [`add_member_with_commit`](Self::add_member_with_commit) and fan the
    /// returned commit out before considering the admission complete.
    ///
    /// # Errors
    ///
    /// Returns [`Error::OpenMls`] if `add_members` or merging the pending
    /// commit fails, or [`Error::TlsCodec`] if the resulting Welcome fails
    /// to serialise.
    pub fn add_member(&mut self, candidate_key_package: KeyPackage) -> Result<Vec<u8>> {
        if self.member_count() != 1 {
            return Err(Error::CommitPropagationRequired {
                operation: "ChannelGroup::add_member",
                required_api: "ChannelGroup::add_member_with_commit",
            });
        }
        let addition = self.add_member_transactional(candidate_key_package, |_, _| Ok(()))?;
        Ok(addition.welcome)
    }

    /// Admit a member and return everything the admission produced: the
    /// Welcome and key history for the joiner and, from the second join on,
    /// the commit for existing members.
    ///
    /// # Errors
    ///
    /// As [`Self::add_member_transactional`].
    pub fn admit(&mut self, candidate_key_package: KeyPackage) -> Result<MemberAddition> {
        self.add_member_transactional(candidate_key_package, |_, _| Ok(()))
    }

    /// Prepare an add, call `persist` with the opaque Welcome, optional
    /// existing-member commit, the joiner's sealed key history and the
    /// post-change epoch, and only then merge the pending commit locally. A
    /// persistence failure leaves the epoch unchanged, allowing the caller to
    /// retry instead of creating a locally admitted member with no durable
    /// delivery record.
    ///
    /// # Errors
    ///
    /// Returns the MLS/codec error, the callback's persistence error, or a
    /// storage error if rolling back a failed handoff fails.
    pub fn add_member_transactional<F>(
        &mut self,
        candidate_key_package: KeyPackage,
        persist: F,
    ) -> Result<MemberAddition>
    where
        F: FnOnce(&ChannelGroup, &MemberAddition) -> Result<()>,
    {
        let addition = self.stage_add(candidate_key_package)?;
        if let Err(error) = persist(self, &addition) {
            self.clear_pending()?;
            return Err(error);
        }
        self.merge_pending_transaction()?;
        Ok(addition)
    }

    /// Stage an add and leave it pending: the caller records it durably,
    /// then merges it ([`Self::merge_pending_transaction`]) or drops it
    /// ([`Self::discard_pending_transaction`]).
    pub(crate) fn stage_add(
        &mut self,
        candidate_key_package: KeyPackage,
    ) -> Result<MemberAddition> {
        let preexisting_members = self.member_count();
        let (commit_msg, welcome_msg, _gi) = self
            .group
            .add_members(
                &self.member.provider,
                &self.member.signer,
                &[candidate_key_package],
            )
            .map_err(Error::openmls)?;
        let staged = (|| {
            let welcome = welcome_msg
                .tls_serialize_detached()
                .map_err(Error::tls_codec)?;
            let commit = if preexisting_members > 1 {
                Some(
                    commit_msg
                        .tls_serialize_detached()
                        .map_err(Error::tls_codec)?,
                )
            } else {
                None
            };
            let epoch = self.epoch().saturating_add(1);
            let key_history = self.pending_key_history(epoch)?;
            Ok(MemberAddition {
                welcome,
                commit,
                key_history,
                epoch,
            })
        })();
        if staged.is_err() {
            self.clear_pending()?;
        }
        staged
    }

    fn clear_pending(&mut self) -> Result<()> {
        self.group
            .clear_pending_commit(openmls_traits::OpenMlsProvider::storage(
                &self.member.provider,
            ))
            .map_err(Error::storage)
    }

    /// Seal the key history for the epoch the pending commit moves to. The
    /// bundle includes that epoch's content key too, derived from the staged
    /// commit, so the joiner's keyring is complete on arrival.
    fn pending_key_history(&self, epoch: u64) -> Result<Vec<u8>> {
        let staged = self
            .group
            .pending_commit()
            .ok_or_else(|| Error::InvalidState("no pending commit".into()))?;
        let crypto = openmls_traits::OpenMlsProvider::crypto(&self.member.provider);
        let export = |label: &str, context: &[u8]| -> Result<SymmetricKey> {
            let exported = Zeroizing::new(
                staged
                    .export_secret(crypto, label, context, 32)
                    .map_err(Error::openmls)?,
            );
            SymmetricKey::from_slice(&exported).map_err(Error::ContentCrypto)
        };
        let mut keyring = self.keyring.clone();
        keyring.insert(
            epoch,
            export(CONTENT_KEY_EXPORTER_LABEL, self.channel_id.0.as_bytes())?,
        )?;
        let transport = export(
            HISTORY_TRANSPORT_EXPORTER_LABEL,
            &self.history_transport_context(epoch),
        )?;
        KeyHistory::seal(self.channel_id, epoch, &transport, &keyring)?.to_bytes()
    }

    /// Same as [`add_member`](Self::add_member) but also returns the commit
    /// bytes existing members need to advance their epoch — use this
    /// whenever there are more than two members. Delivered to every existing
    /// member except the caller (already merged); each calls
    /// [`process_commit`](Self::process_commit). Returns `(welcome_bytes, commit_bytes)`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::OpenMls`] if `add_members` or merging the pending
    /// commit fails, or [`Error::TlsCodec`] if the Welcome or commit fails
    /// to serialise.
    pub fn add_member_with_commit(
        &mut self,
        candidate_key_package: KeyPackage,
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let addition = self.add_member_transactional(candidate_key_package, |_, _| Ok(()))?;
        let commit = addition.commit.ok_or(Error::CommitPropagationRequired {
            operation: "ChannelGroup::add_member_with_commit",
            required_api: "ChannelGroup::add_member",
        })?;
        Ok((addition.welcome, commit))
    }

    /// Process an inbound commit and advance the local epoch. Called by
    /// every existing member when another authorised member admits a third
    /// party via [`add_member_with_commit`](Self::add_member_with_commit);
    /// advances `epoch()` by one and refreshes the application key schedule.
    /// The only inbound governance message accepted today is a commit from
    /// an already-trusted group member.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TlsCodec`] if `commit_bytes` fails to deserialise;
    /// [`Error::OpenMls`] if openmls rejects the commit (including a panic
    /// absorbed from its debug-assert path, per the comment in
    /// [`decrypt`](Self::decrypt)); or [`Error::UnexpectedMessageKind`] if
    /// the processed message isn't a commit.
    pub fn process_commit(&mut self, commit_bytes: &[u8]) -> Result<()> {
        let msg_in = MlsMessageIn::tls_deserialize_exact(commit_bytes).map_err(Error::tls_codec)?;
        let protocol_msg: ProtocolMessage =
            msg_in.try_into_protocol_message().map_err(Error::openmls)?;
        // Same defensive panic-catch as `decrypt`; see the comment there for
        // the openmls 0.8.1 debug-assert rationale.
        let processed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.group
                .process_message(&self.member.provider, protocol_msg)
        }))
        .map_err(|_| Error::OpenMls("commit processing failed (panic absorbed)".to_string()))?
        .map_err(Error::openmls)?;
        match processed.into_content() {
            ProcessedMessageContent::StagedCommitMessage(staged_commit) => {
                self.group
                    .merge_staged_commit(&self.member.provider, *staged_commit)
                    .map_err(Error::openmls)?;
                self.record_epoch_key()
            }
            _ => Err(Error::UnexpectedMessageKind {
                expected: "commit",
                actual: "non-commit content",
            }),
        }
    }

    /// Join an existing group by processing a Welcome. `welcome_bytes` is
    /// the TLS-serialised Welcome from [`add_member`](Self::add_member);
    /// `member` must be the same `MemberKey` that minted the `KeyPackage`
    /// the Welcome was addressed to (its HPKE private key lives in
    /// `member.provider`'s storage). The returned group starts at the
    /// inviter's post-add epoch — older epochs are unrecoverable, joiners
    /// get no history.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TlsCodec`] if `welcome_bytes` fails to deserialise;
    /// [`Error::UnexpectedMessageKind`] if it deserialises to something
    /// other than a Welcome; or [`Error::OpenMls`] if staging or joining
    /// the group from the Welcome fails.
    pub fn process_welcome(
        channel_id: ChannelId,
        welcome_bytes: &[u8],
        member: MemberKey,
    ) -> Result<Self> {
        let msg_in =
            MlsMessageIn::tls_deserialize_exact(welcome_bytes).map_err(Error::tls_codec)?;
        let welcome = match msg_in.extract() {
            MlsMessageBodyIn::Welcome(w) => w,
            MlsMessageBodyIn::PrivateMessage(_) => {
                return Err(Error::UnexpectedMessageKind {
                    expected: "Welcome",
                    actual: "PrivateMessage",
                });
            }
            MlsMessageBodyIn::PublicMessage(_) => {
                return Err(Error::UnexpectedMessageKind {
                    expected: "Welcome",
                    actual: "PublicMessage",
                });
            }
            MlsMessageBodyIn::GroupInfo(_) => {
                return Err(Error::UnexpectedMessageKind {
                    expected: "Welcome",
                    actual: "GroupInfo",
                });
            }
            MlsMessageBodyIn::KeyPackage(_) => {
                return Err(Error::UnexpectedMessageKind {
                    expected: "Welcome",
                    actual: "KeyPackage",
                });
            }
        };
        let staged = StagedWelcome::new_from_welcome(
            &member.provider,
            &group_join_config(),
            welcome,
            // No external ratchet tree needed — `use_ratchet_tree_extension(true)`
            // on creation embeds it in the Welcome.
            None,
        )
        .map_err(Error::openmls)?;
        let group = staged
            .into_group(&member.provider)
            .map_err(Error::openmls)?;
        // Sanity: joined group's ciphersuite must match our fixed
        // `CIPHERSUITE` — we never negotiate, so a mismatch means a peer
        // with a different build is on the wire.
        debug_assert_eq!(group.ciphersuite(), CIPHERSUITE);
        let mut joined = Self {
            channel_id,
            member,
            group,
            keyring: ContentKeyring::new(),
        };
        joined.record_epoch_key()?;
        Ok(joined)
    }

    /// Join from a Welcome and import the key history that came with it
    /// ([`MemberAddition::key_history`]), so every earlier content-key op is
    /// readable immediately and without any other member online.
    ///
    /// # Errors
    ///
    /// As [`Self::process_welcome`] and [`Self::import_key_history`].
    pub fn process_welcome_with_history(
        channel_id: ChannelId,
        welcome_bytes: &[u8],
        key_history: &[u8],
        member: MemberKey,
    ) -> Result<Self> {
        let mut joined = Self::process_welcome(channel_id, welcome_bytes, member)?;
        joined.import_key_history(key_history)?;
        Ok(joined)
    }

    /// Encrypt a payload under the current epoch key, returning
    /// TLS-serialised `MlsMessageOut` bytes. The caller wraps these in a
    /// `SignedOp.payload` with `encryption: Some(EncryptionMetadata { epoch:
    /// self.epoch(), channel: self.channel_id() })`.
    ///
    /// Two signatures are involved and are not interchangeable: the inner
    /// MLS handshake-level framing is signed by the current member's
    /// signer, binding the message to an MLS group member; the outer
    /// `SignedOp.signature` is a separate collab-types signature over the
    /// canonical envelope, binding it to an `Identity`/attestation.
    ///
    /// # Errors
    ///
    /// Returns [`Error::OpenMls`] if `create_message` fails, or
    /// [`Error::TlsCodec`] if the resulting message fails to serialise.
    pub fn encrypt(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        let msg = self
            .group
            .create_message(&self.member.provider, &self.member.signer, payload)
            .map_err(Error::openmls)?;
        msg.tls_serialize_detached().map_err(Error::tls_codec)
    }

    /// Decrypt an inbound ciphertext (the same bytes [`encrypt`](Self::encrypt)
    /// produces), returning the plaintext on success.
    ///
    /// Failure modes (all surfaced as `Error::OpenMls`): sender not a member
    /// (`MissingMemberError`); ciphertext from an unreachable epoch (too old
    /// or too far in the future); tampered ciphertext (AEAD tag check
    /// fails); wrong group (key schedule rejection). In every case the
    /// message is dropped and the broker's audit log records the reason.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TlsCodec`] if `ciphertext` fails to deserialise;
    /// [`Error::OpenMls`] for the protocol-level failure modes listed
    /// above (including a panic absorbed from openmls's debug-assert
    /// path — see the comment inside); or
    /// [`Error::NotApplicationMessage`] if the processed message isn't an
    /// application payload.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let msg_in = MlsMessageIn::tls_deserialize_exact(ciphertext).map_err(Error::tls_codec)?;
        let protocol_msg: ProtocolMessage =
            msg_in.try_into_protocol_message().map_err(Error::openmls)?;
        // Why catch_unwind: openmls 0.8.1 has a `debug_assert!(false, ...)` on
        // the AEAD-tag-mismatch path (`framing/private_message_in.rs:136`) —
        // it returns the error correctly in release but panics in debug. A
        // broker accepting Iroh messages must never crash on a malformed
        // peer payload, so the panic is absorbed here and turned into
        // `Error::OpenMls`. One extra `AssertUnwindSafe` per decrypt,
        // negligible. Remove once openmls fixes the debug-assert.
        let processed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.group
                .process_message(&self.member.provider, protocol_msg)
        }))
        .map_err(|_| Error::OpenMls("decryption failed (panic absorbed)".to_string()))?
        .map_err(Error::openmls)?;
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app) => Ok(app.into_bytes()),
            _ => Err(Error::NotApplicationMessage),
        }
    }

    /// Remove a member and return the commit every other member must process.
    /// The caller is already advanced locally; surviving remote members call
    /// [`process_commit`](Self::process_commit). The removed member should also
    /// receive the commit so `OpenMLS` marks its local group inactive.
    ///
    /// # Errors
    ///
    /// Returns [`Error::OpenMls`] when the leaf is invalid or `OpenMLS` refuses
    /// the change, and [`Error::TlsCodec`] when the commit cannot be encoded.
    pub fn remove_member(&mut self, leaf_index: u32) -> Result<Vec<u8>> {
        self.remove_member_transactional(leaf_index, |_, _, _| Ok(()))
    }

    /// Removal counterpart to [`Self::add_member_transactional`]. The opaque
    /// commit is durably handed to the caller before the local pending commit
    /// is merged.
    ///
    /// # Errors
    ///
    /// Returns the MLS/codec error, the callback's persistence error, or a
    /// storage error if rolling back a failed handoff fails.
    pub fn remove_member_transactional<F>(&mut self, leaf_index: u32, persist: F) -> Result<Vec<u8>>
    where
        F: FnOnce(&ChannelGroup, &[u8], u64) -> Result<()>,
    {
        let (commit, epoch) = self.stage_remove(leaf_index)?;
        if let Err(error) = persist(self, &commit, epoch) {
            self.clear_pending()?;
            return Err(error);
        }
        self.merge_pending_transaction()?;
        Ok(commit)
    }

    /// Stage a removal and leave it pending, as [`Self::stage_add`]. Returns
    /// the commit and the epoch it moves to.
    pub(crate) fn stage_remove(&mut self, leaf_index: u32) -> Result<(Vec<u8>, u64)> {
        let (commit, _welcome, _group_info) = self
            .group
            .remove_members(
                &self.member.provider,
                &self.member.signer,
                &[LeafNodeIndex::new(leaf_index)],
            )
            .map_err(Error::openmls)?;
        match commit.tls_serialize_detached() {
            Ok(commit) => Ok((commit, self.epoch().saturating_add(1))),
            Err(error) => {
                self.clear_pending()?;
                Err(Error::tls_codec(error))
            }
        }
    }

    /// Finish a pending member commit restored from a sealed crash-recovery
    /// checkpoint. Returns `false` when no transaction is pending.
    ///
    /// # Errors
    ///
    /// Returns an `OpenMLS` error when the pending commit cannot be merged.
    pub fn merge_pending_transaction(&mut self) -> Result<bool> {
        if self.group.pending_commit().is_none() {
            return Ok(false);
        }
        self.group
            .merge_pending_commit(&self.member.provider)
            .map_err(Error::openmls)?;
        self.record_epoch_key()?;
        Ok(true)
    }

    /// Drop a staged commit whose durable record was never written. Returns
    /// `false` when no commit is staged.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the staged commit cannot be cleared.
    pub fn discard_pending_transaction(&mut self) -> Result<bool> {
        if self.group.pending_commit().is_none() {
            return Ok(false);
        }
        self.clear_pending()?;
        Ok(true)
    }
}

impl std::fmt::Debug for ChannelGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `group: MlsGroup` doesn't implement `Debug` (openmls state), so
        // this can't show every field — `finish_non_exhaustive` says so
        // explicitly instead of silently pretending the summary is complete.
        f.debug_struct("ChannelGroup")
            .field("channel_id", &self.channel_id)
            .field("member", &self.member)
            .field("epoch", &self.epoch())
            .field("member_count", &self.member_count())
            .field("keyring", &self.keyring)
            .finish_non_exhaustive()
    }
}
