//! `ChannelGroup` — one MLS group per channel.
//!
//! A `ChannelGroup` is the runtime view a single device has of one
//! channel's MLS group: it owns the [`MlsGroup`] state, the
//! [`MemberKey`] secret material the device participates with, and
//! the underlying provider. From this view it can:
//!
//! - encrypt an outgoing application payload under the current epoch key;
//! - decrypt an incoming application payload from a fellow member;
//! - admit a new member by emitting a Welcome.
//!
//! `ChannelGroup` is **single-tenant**: one device, one membership.
//! Two devices belonging to the same human are two separate
//! `ChannelGroup` instances. Two channels are two instances. Sharing
//! a provider between channels is explicitly not supported.
//!
//! ## Wire shapes
//!
//! All output bytes are TLS-CODEC-serialised `MlsMessageOut`
//! envelopes (one of `PrivateMessage`, `Welcome`, etc.). The broker
//! is responsible for routing the bytes; this crate does not stamp
//! metadata over them. The companion `EncryptionMetadata` field
//! (added in `qos_collab_types::SignedOp`) names the channel + epoch
//! so the receiver can pick the right `ChannelGroup` before calling
//! [`ChannelGroup::decrypt`].

use openmls::prelude::*;
use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};

use qos_collab_types::ChannelId;

use crate::crypto::{CIPHERSUITE, MemberKey};
use crate::errors::{Error, Result};

/// One device's view of one channel's MLS group.
///
/// See module docs for the lifecycle. The struct deliberately holds
/// the `MemberKey` by value (move-in semantics on `create` /
/// `process_welcome`) — the secret material in the member is what
/// makes the group state usable; separating them would invite
/// "wrong signer for this group" bugs.
pub struct ChannelGroup {
    /// The qOS channel this group represents. Stored so the caller
    /// can route incoming ops via [`channel_id`](Self::channel_id)
    /// without keeping a side table.
    channel_id: ChannelId,
    /// Secret material + provider. The group state lives in
    /// `member.provider`; do not allocate a second provider.
    member: MemberKey,
    /// The openmls group state. Mutated in place by every operation;
    /// kept private so callers can't reach around the wrapper and
    /// (e.g.) commit without going through this crate.
    group: MlsGroup,
}

/// `MlsGroupCreateConfig` toggles we always want on:
///
/// - `use_ratchet_tree_extension(true)` makes every Welcome carry
///   the ratchet tree inline. Without it, a joiner has to receive
///   the tree out-of-band; for Phase 7 (single Welcome per join) the
///   self-contained form is simpler and the size penalty is small
///   for human-scale channels.
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
    /// Bootstrap a one-member channel.
    ///
    /// `owner` becomes the sole member of the group; its
    /// `MemberKey`'s provider takes ownership of the group state.
    /// The returned `ChannelGroup` is fully usable on its own —
    /// further members are added via
    /// [`add_member`](Self::add_member).
    pub fn create(channel_id: ChannelId, owner: MemberKey) -> Result<Self> {
        let group = MlsGroup::new(
            &owner.provider,
            &owner.signer,
            &group_create_config(),
            owner.credential_with_key(),
        )
        .map_err(Error::openmls)?;
        Ok(Self {
            channel_id,
            member: owner,
            group,
        })
    }

    /// The channel this group encrypts for.
    pub fn channel_id(&self) -> ChannelId {
        self.channel_id
    }

    /// The MLS epoch counter, mirrored into the wire `EncryptionMetadata.epoch`.
    ///
    /// Each membership change (add, remove, key update) increments
    /// the epoch. Ciphertexts produced at epoch *N* must be decrypted
    /// by a `ChannelGroup` that is at epoch *N* or later (older
    /// epoch ciphertexts are accepted up to a configurable window —
    /// openmls's default — but ciphertext from a *future* epoch is
    /// dropped until the recipient catches up).
    pub fn epoch(&self) -> u64 {
        self.group.epoch().as_u64()
    }

    /// Number of members in the current epoch.
    ///
    /// Cheap to call; openmls stores the count in the leaf-node
    /// vector. Useful in tests for asserting "alice added bob → 2"
    /// without unpacking the full group state.
    pub fn member_count(&self) -> usize {
        // openmls's MlsGroup exposes `members()` which iterates leaf
        // nodes. Count rather than collect to avoid the allocation.
        self.group.members().count()
    }

    /// Read-only borrow of the underlying member key.
    ///
    /// Exposed so the broker can look up which identity public key
    /// this group is operating under without re-deriving it from
    /// the channel's attestation store.
    pub fn member(&self) -> &MemberKey {
        &self.member
    }

    /// Add a new member to the group.
    ///
    /// `candidate_key_package` is the key package the candidate
    /// generated via [`MemberKey::fresh_key_package`] and shipped
    /// out-of-band to the owner (or to whichever member is
    /// authorised to call `add_member` — typically the channel
    /// owner; the role-table check in `qos-collab-types` enforces
    /// who is allowed to issue this op upstream).
    ///
    /// On success:
    ///
    /// - the local group state advances one epoch via
    ///   `merge_pending_commit`;
    /// - a TLS-serialised `Welcome` is returned;
    /// - the caller's responsibility is to broadcast the Welcome to
    ///   the new member (via the broker's `ChannelEvent::MemberAdded`
    ///   log entry) and to broadcast the accompanying *commit* to
    ///   existing members so they advance their own epoch too.
    ///
    /// **Phase 7 limitation**: we drop the commit on the floor. With
    /// only a single-member-creator / single-joiner test surface, the
    /// joiner reconstructs its state from the self-contained Welcome
    /// and the creator advances locally — both end up at the new
    /// epoch. A 3+-member group needs the commit fanned out to
    /// existing peers; that wiring lives in the broker integration
    /// step after this crate lands. For the in-crate `third_member_joins`
    /// test we synthesise the broker handoff by also returning the
    /// commit bytes via [`add_member_with_commit`](Self::add_member_with_commit).
    pub fn add_member(
        &mut self,
        candidate_key_package: KeyPackage,
    ) -> Result<Vec<u8>> {
        let (_commit, welcome_msg, _gi) = self
            .group
            .add_members(
                &self.member.provider,
                &self.member.signer,
                &[candidate_key_package],
            )
            .map_err(Error::openmls)?;
        self.group
            .merge_pending_commit(&self.member.provider)
            .map_err(Error::openmls)?;
        welcome_msg
            .tls_serialize_detached()
            .map_err(Error::tls_codec)
    }

    /// Same as [`add_member`](Self::add_member) but also returns the
    /// commit bytes that existing members need to process to advance
    /// their epoch.
    ///
    /// Use this in any setting with more than two members. The
    /// commit must be delivered to every existing member except the
    /// caller (the caller's local state has already been merged).
    /// Each existing member calls [`process_commit`](Self::process_commit)
    /// to advance.
    ///
    /// Returns `(welcome_bytes, commit_bytes)`.
    pub fn add_member_with_commit(
        &mut self,
        candidate_key_package: KeyPackage,
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let (commit_msg, welcome_msg, _gi) = self
            .group
            .add_members(
                &self.member.provider,
                &self.member.signer,
                &[candidate_key_package],
            )
            .map_err(Error::openmls)?;
        self.group
            .merge_pending_commit(&self.member.provider)
            .map_err(Error::openmls)?;
        let welcome = welcome_msg
            .tls_serialize_detached()
            .map_err(Error::tls_codec)?;
        let commit = commit_msg
            .tls_serialize_detached()
            .map_err(Error::tls_codec)?;
        Ok((welcome, commit))
    }

    /// Process an inbound commit message and advance the local epoch.
    ///
    /// Called by every existing member when another authorised member
    /// admits a third party (via
    /// [`add_member_with_commit`](Self::add_member_with_commit)). The
    /// commit advances `epoch()` by one and refreshes the application
    /// key schedule.
    ///
    /// Phase 7.1 will add `process_proposal` to the public API; for
    /// now the only inbound governance message we accept is a commit
    /// from an already-trusted group member.
    pub fn process_commit(&mut self, commit_bytes: &[u8]) -> Result<()> {
        let msg_in =
            MlsMessageIn::tls_deserialize_exact(commit_bytes).map_err(Error::tls_codec)?;
        let protocol_msg: ProtocolMessage =
            msg_in.try_into_protocol_message().map_err(Error::openmls)?;
        // Same defensive panic-catch as `decrypt`; see the comment
        // there for the openmls 0.8.1 debug-assert rationale.
        let processed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.group.process_message(&self.member.provider, protocol_msg)
        }))
        .map_err(|_| Error::OpenMls("commit processing failed (panic absorbed)".to_string()))?
        .map_err(Error::openmls)?;
        match processed.into_content() {
            ProcessedMessageContent::StagedCommitMessage(staged_commit) => {
                self.group
                    .merge_staged_commit(&self.member.provider, *staged_commit)
                    .map_err(Error::openmls)?;
                Ok(())
            }
            _ => Err(Error::UnexpectedMessageKind {
                expected: "commit",
                actual: "non-commit content",
            }),
        }
    }

    /// Join an existing group by processing a Welcome.
    ///
    /// `welcome_bytes` is the TLS-serialised Welcome message returned
    /// by [`add_member`](Self::add_member) on the inviting side.
    /// `member` is the joining party's key material — must be the
    /// same `MemberKey` that minted the `KeyPackage` the Welcome was
    /// addressed to (the HPKE private key needed to decrypt the
    /// Welcome lives in `member.provider`'s storage).
    ///
    /// The returned `ChannelGroup` starts at the epoch the inviter
    /// was at *after* the add. Older epochs are unrecoverable —
    /// joiners do not get history (see Phase 7 non-goals).
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
            // No external ratchet tree — we set
            // `use_ratchet_tree_extension(true)` on creation so the
            // Welcome embeds it.
            None,
        )
        .map_err(Error::openmls)?;
        let group = staged.into_group(&member.provider).map_err(Error::openmls)?;
        // Sanity: ciphersuite of the joined group must match our
        // fixed `CIPHERSUITE`. We never negotiate, so a mismatch
        // means a peer with a different build is on the wire.
        debug_assert_eq!(group.ciphersuite(), CIPHERSUITE);
        Ok(Self {
            channel_id,
            member,
            group,
        })
    }

    /// Encrypt a payload under the current epoch key.
    ///
    /// Returns the TLS-serialised `MlsMessageOut` bytes carrying an
    /// application-message ciphertext. The caller wraps these bytes
    /// in a `SignedOp.payload` with `encryption: Some(EncryptionMetadata
    /// { epoch: self.epoch(), channel: self.channel_id() })`.
    ///
    /// The current member's signer signs the inner MLS handshake-level
    /// commit framing; the outer `SignedOp.signature` is a separate
    /// `qos-collab-types` signature over the canonical bytes view of
    /// the envelope (including the ciphertext). Both signatures are
    /// needed: the inner one binds the message to an MLS group member;
    /// the outer one binds it to a `qos-collab-types::Identity` /
    /// attestation. They are not interchangeable.
    pub fn encrypt(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        let msg = self
            .group
            .create_message(&self.member.provider, &self.member.signer, payload)
            .map_err(Error::openmls)?;
        msg.tls_serialize_detached().map_err(Error::tls_codec)
    }

    /// Decrypt an inbound ciphertext.
    ///
    /// Accepts the same TLS-serialised bytes that
    /// [`encrypt`](Self::encrypt) produces. Returns the plaintext on
    /// success.
    ///
    /// Failure modes (all surfaced as `Error::OpenMls`):
    ///
    /// - sender not a member of this group (openmls reports
    ///   `MissingMemberError`);
    /// - ciphertext from an unreachable epoch (older than the
    ///   joiner's first epoch, or further in the future than we've
    ///   advanced);
    /// - tampered ciphertext (AEAD tag check fails);
    /// - wrong group altogether (key schedule rejection).
    ///
    /// In every case the inbound message is dropped; the broker's
    /// audit log records the rejection reason.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let msg_in =
            MlsMessageIn::tls_deserialize_exact(ciphertext).map_err(Error::tls_codec)?;
        let protocol_msg: ProtocolMessage =
            msg_in.try_into_protocol_message().map_err(Error::openmls)?;
        // Why catch_unwind: openmls 0.8.1 has a `debug_assert!(false,
        // ...)` on the AEAD-tag-mismatch path (see
        // `framing/private_message_in.rs:136`). It returns the error
        // correctly in release builds but panics in debug. A
        // broker accepting libp2p messages must never crash on a
        // malformed peer payload, so we absorb the panic here and
        // turn it into our own `Error::OpenMls`. The cost is one
        // extra `AssertUnwindSafe` per decrypt, which is negligible.
        // Once openmls fixes the debug-assert this wrapper can come
        // out without changing the public contract.
        let processed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.group.process_message(&self.member.provider, protocol_msg)
        }))
        .map_err(|_| Error::OpenMls("decryption failed (panic absorbed)".to_string()))?
        .map_err(Error::openmls)?;
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app) => Ok(app.into_bytes()),
            _ => Err(Error::NotApplicationMessage),
        }
    }

    /// Remove a member from the group.
    ///
    /// **Phase 7.1**: not implemented. Tracking ticket: the broker
    /// needs to fan out the resulting commit to surviving members
    /// (same plumbing as
    /// [`add_member_with_commit`](Self::add_member_with_commit) but
    /// without a Welcome), and the role-table needs a `Revoke` op
    /// class. Until that lands, callers can mint a fresh channel and
    /// invite only the desired members.
    pub fn remove_member(&mut self, _leaf_index: u32) -> ! {
        todo!(
            "Phase 7.1: member removal requires broker commit fan-out + role-table revoke wiring; \
             not in the Phase 7 minimum-viable surface"
        )
    }
}

impl std::fmt::Debug for ChannelGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelGroup")
            .field("channel_id", &self.channel_id)
            .field("member", &self.member)
            .field("epoch", &self.epoch())
            .field("member_count", &self.member_count())
            .finish()
    }
}
