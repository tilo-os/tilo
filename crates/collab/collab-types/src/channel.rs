//! Channels and artifacts — the working-area primitive.
//!
//! A channel groups members + artifacts inside a space; membership is a
//! subset of the parent space's members. Channels are cheap (no rekey
//! overhead before MLS rekey lands) so users create them liberally.
//!
//! `default_role` applies to space members not explicitly attested in
//! `members` (data-driven: `Editor` in small spaces, `Commenter` in mid,
//! `Viewer` in large — the owner picks at creation). `observe_default`
//! lets a space owner drop in as a Viewer without explicit invitation.
//! `min_clearance` and the per-artifact `classification` compose as
//! `max(channel.min_clearance, artifact.classification)`; lowering
//! `min_clearance` is allowed only on a fresh channel.
//! `presence_visible_for_viewers` defaults false — a Viewer is a lurker,
//! and outing them accidentally is hostile UX.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto::Ed25519PublicKey;
use crate::identity::{AttestationId, Clearance};
use crate::op::SignedOp;
use crate::role::{ArtifactKind, Role};
use crate::space::SpaceId;

/// How a channel protects its content — part of its signed governance,
/// fixed when the channel is created.
///
/// Both classes sign and validate every op the same way; they differ only in
/// whether content is encrypted to the membership.
///
/// - [`Self::Encrypted`] — a team channel. Content is sealed under the MLS
///   group's per-epoch keys, and every membership change rotates them.
/// - [`Self::Public`] — a community channel thousands join and leave. Content
///   is readable by anyone admitted, so rotating a group key per join would buy
///   nothing and cost a commit per member. No MLS group exists for it, joins and
///   leaves do no MLS work, and an encrypted envelope on it is refused rather
///   than trusted: a public channel that quietly carried ciphertext would tell
///   members something untrue about who can read it.
///
/// The class never changes after creation. In particular an encrypted channel
/// can never become public — members were told their content is sealed, and
/// re-labelling the channel would not un-share anything already posted under
/// that promise. See [`Self::may_become`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelClass {
    /// MLS-encrypted team content. The default for every channel a person
    /// creates.
    #[default]
    Encrypted,
    /// Signed but unencrypted community content.
    Public,
}

/// Why an op or a governance change is not admissible in a channel's class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChannelClassError {
    /// An op on a public channel carried encryption metadata.
    #[error("public channels carry no encrypted content")]
    EncryptedOpInPublicChannel,
    /// MLS group work (genesis, admission, removal, encryption) was requested
    /// for a public channel.
    #[error("public channels have no MLS group")]
    GroupKeyInPublicChannel,
    /// A governance change tried to alter the class of an existing channel.
    #[error("a channel's class is fixed at creation: {from:?} cannot become {to:?}")]
    ClassChange {
        /// The class the channel was created with.
        from: ChannelClass,
        /// The class the change asked for.
        to: ChannelClass,
    },
}

impl ChannelClass {
    /// Stable lowercase name, as written into governance documents and shown
    /// to members.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Encrypted => "encrypted",
            Self::Public => "public",
        }
    }

    /// Parse [`Self::as_str`]. Anything else is not a class.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "encrypted" => Some(Self::Encrypted),
            "public" => Some(Self::Public),
            _ => None,
        }
    }

    /// Whether content in this class is sealed to the membership.
    #[must_use]
    pub const fn is_encrypted(self) -> bool {
        matches!(self, Self::Encrypted)
    }

    /// Whether this class keeps an MLS group — and so whether admitting or
    /// removing a member does any group-key work.
    #[must_use]
    pub const fn keeps_group_key(self) -> bool {
        self.is_encrypted()
    }

    /// Refuse MLS group work for a channel of this class.
    ///
    /// # Errors
    ///
    /// [`ChannelClassError::GroupKeyInPublicChannel`] for a public channel.
    pub const fn admits_group_key(self) -> Result<(), ChannelClassError> {
        if self.keeps_group_key() {
            Ok(())
        } else {
            Err(ChannelClassError::GroupKeyInPublicChannel)
        }
    }

    /// The class-specific admission rule for one signed op, applied beside
    /// the ordinary signature, role and capability validation (never instead
    /// of it).
    ///
    /// # Errors
    ///
    /// [`ChannelClassError::EncryptedOpInPublicChannel`] when a public
    /// channel's op carries encryption metadata.
    pub fn admits_op(self, op: &SignedOp) -> Result<(), ChannelClassError> {
        if matches!(self, Self::Public) && op.encryption.is_some() {
            return Err(ChannelClassError::EncryptedOpInPublicChannel);
        }
        Ok(())
    }

    /// Whether governance recorded as `self` may be replaced by `next`. Only
    /// a restatement is allowed: the class is fixed at creation.
    ///
    /// # Errors
    ///
    /// [`ChannelClassError::ClassChange`] for any actual change.
    pub fn may_become(self, next: Self) -> Result<(), ChannelClassError> {
        if self == next {
            Ok(())
        } else {
            Err(ChannelClassError::ClassChange {
                from: self,
                to: next,
            })
        }
    }
}

/// Stable id for a channel. UUID v4 — same rationale as the others.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChannelId(pub Uuid);

impl ChannelId {
    /// Mint a fresh channel id.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ChannelId {
    fn default() -> Self {
        Self::new()
    }
}

/// Stable id for an artifact. UUID v4.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArtifactId(pub Uuid);

impl ArtifactId {
    /// Mint a fresh artifact id.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ArtifactId {
    fn default() -> Self {
        Self::new()
    }
}

/// Lightweight reference from a `Space` to a `Channel`.
///
/// Stored as `{ id, name }` rather than the full `Channel` since the same
/// channel may be open in two places and replicating the full struct
/// would invite drift; the full record loads by id on demand.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelRef {
    /// Stable id of the referenced channel.
    pub id: ChannelId,
    /// Cached display name, used without loading the full record; if stale,
    /// the next load corrects it.
    pub name: String,
}

/// Lightweight reference from a `Channel` to an `Artifact`. Mirrors
/// `ChannelRef`'s rationale; `crdt_doc` loads lazily so large CRDT
/// histories don't flow through channel summaries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    /// Stable id of the referenced artifact.
    pub id: ArtifactId,
    /// Cached display name. May be stale.
    pub name: String,
    /// Cached artifact kind, so the UI can pick the right tile icon without
    /// loading the full record.
    pub kind: ArtifactKind,
}

/// An explicit per-member role grant on a channel, overriding
/// `default_role`. `attestation` is the proof — a `Govern`-class signed
/// event in the channel's history minted this grant, looked up by id
/// when validating ops from this member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberRef {
    /// The member's identity public key.
    pub identity: Ed25519PublicKey,
    /// The granted role.
    pub role: Role,
    /// Id of the attestation that issued this grant; the validator resolves
    /// it against the channel's attestation store and walks the trust chain
    /// to an authorised signer.
    pub attestation: AttestationId,
}

/// A working-area inside a space — the unit of role enforcement: every
/// member has a role *in this channel*. Artifacts inherit the channel's
/// `min_clearance` floor and may carry a stricter `classification`, never weaker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    /// Stable id for this channel.
    pub id: ChannelId,
    /// Parent space; the validator uses this to look up the space's trust
    /// roots when resolving non-channel-local attestations (e.g. `ClearedAt`).
    pub space: SpaceId,
    /// Human-meaningful display name ("#engineering"); not unique, not used
    /// in equality.
    pub name: String,
    /// Whether the channel's content is encrypted to its members. Fixed at
    /// creation; see [`ChannelClass`].
    pub class: ChannelClass,
    /// Owner identity — initially the creator, transferred via
    /// `ChannelEvent::OwnershipTransfer`. Only key that can issue
    /// `Govern`-class ops unless delegated via an `AttestationIssued` event
    /// granting `Role::Owner`.
    pub owner: Ed25519PublicKey,
    /// Role applied to space members not explicitly listed in `members`;
    /// affects no one outside the parent space.
    pub default_role: Role,
    /// Explicit per-member role grants. Overrides `default_role`.
    pub members: Vec<MemberRef>,
    /// If true, space owners can drop in as a Viewer without an explicit
    /// invite (org-wide oversight); defaults false.
    pub observe_default: bool,
    /// Minimum clearance any member must hold to apply ops in this channel.
    /// `None` means no clearance gate (artifact-level `classification` may
    /// still apply).
    pub min_clearance: Option<Clearance>,
    /// Artifacts in this channel.
    pub artifacts: Vec<ArtifactRef>,
    /// Whether `Role::Viewer` members are visible in the presence roster.
    /// Defaults `false` — Viewer is "lurker", and outing them is hostile UX;
    /// owners can flip this for transparent-audit rooms.
    pub presence_visible_for_viewers: bool,
}

/// A single artifact: a note, file, kanban board, etc. `crdt_doc` is an
/// opaque handle (currently `Option<Vec<u8>>`, an Automerge change-set
/// snapshot or a content-addressed reference) — the exact representation
/// isn't pinned here since storage is allowed to vary; the wire shape is
/// what matters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    /// Stable id for this artifact.
    pub id: ArtifactId,
    /// Categorical kind, driving the role-table lookup and UI tile rendering.
    pub kind: ArtifactKind,
    /// Home channel; cross-channel references would point back here for governance.
    pub channel: ChannelId,
    /// Per-artifact classification, optionally stricter than the channel's
    /// `min_clearance`. May only be raised, never lowered, by a
    /// channel-owner-issued `Govern` op.
    pub classification: Option<Clearance>,
    /// Opaque CRDT-doc handle — an Automerge change-set blob, a
    /// content-addressed FUSE reference, or `None` for an empty artifact.
    /// The schema does not pin the storage path.
    pub crdt_doc: Option<DocumentHandle>,
}

/// Opaque pointer to a CRDT document. Wrapped in a newtype so future
/// variants (a content hash, a local path, an inline blob) can be added
/// without breaking `Artifact`'s wire shape. Currently a length-prefixed
/// opaque byte blob.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DocumentHandle(#[serde(with = "serde_bytes")] pub Vec<u8>);

/// Delta applied by a `ChannelEvent::SettingsUpdate` event. All fields
/// are optional — only fields present are mutated, so a single `Govern`
/// op can rename + change `default_role` + flip `observe_default` at once.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelSettingsDelta {
    /// New default role for un-attested space members.
    pub default_role: Option<Role>,
    /// Toggle space-owner observation drop-in.
    pub observe_default: Option<bool>,
    /// Raise (only) the minimum-clearance floor.
    pub min_clearance: Option<Clearance>,
    /// Toggle whether Viewer-role members appear in the presence
    /// roster.
    pub presence_visible_for_viewers: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{canonical_bytes, from_canonical_bytes};
    use crate::crypto::generate_keypair;

    /// A channel round-trips canonically, artifact ids are unique, and an
    /// empty settings delta changes nothing.
    #[test]
    fn channel_types_round_trip_and_start_empty() {
        // Case: channel round trip.
        let (_, owner) = generate_keypair();
        let ch = Channel {
            id: ChannelId::new(),
            space: SpaceId::new(),
            name: "#engineering".into(),
            class: ChannelClass::Encrypted,
            owner,
            default_role: Role::Editor,
            members: vec![],
            observe_default: false,
            min_clearance: Some(Clearance::INTERNAL),
            artifacts: vec![],
            presence_visible_for_viewers: false,
        };
        let bytes = canonical_bytes(&ch).unwrap();
        let back: Channel = from_canonical_bytes(&bytes).unwrap();
        assert_eq!(ch, back);

        // Case: artifact ids are unique.
        assert_ne!(ArtifactId::new(), ArtifactId::new());

        // Case: the settings delta default is all None.
        let d = ChannelSettingsDelta::default();
        assert!(d.default_role.is_none());
        assert!(d.observe_default.is_none());
        assert!(d.min_clearance.is_none());
        assert!(d.presence_visible_for_viewers.is_none());
    }

    #[test]
    fn public_channels_refuse_encrypted_ops_and_group_keys() {
        let (sk, author) = generate_keypair();
        let mut op = SignedOp::new_unsigned(
            b"hello".to_vec(),
            ArtifactId::new(),
            author,
            AttestationId::new(),
            crate::role::OpClass::Comment,
            std::time::SystemTime::UNIX_EPOCH,
            1,
        );
        crate::crypto::sign_op(&mut op, &sk).unwrap();
        assert_eq!(ChannelClass::Public.admits_op(&op), Ok(()));
        assert_eq!(ChannelClass::Encrypted.admits_op(&op), Ok(()));

        op.encryption = Some(crate::op::EncryptionMetadata::mls_application(
            3,
            ChannelId::new(),
        ));
        assert_eq!(
            ChannelClass::Public.admits_op(&op),
            Err(ChannelClassError::EncryptedOpInPublicChannel)
        );
        assert_eq!(ChannelClass::Encrypted.admits_op(&op), Ok(()));

        assert_eq!(
            ChannelClass::Public.admits_group_key(),
            Err(ChannelClassError::GroupKeyInPublicChannel)
        );
        assert_eq!(ChannelClass::Encrypted.admits_group_key(), Ok(()));
    }

    #[test]
    fn class_is_fixed_at_creation_in_both_directions() {
        assert_eq!(
            ChannelClass::Encrypted.may_become(ChannelClass::Public),
            Err(ChannelClassError::ClassChange {
                from: ChannelClass::Encrypted,
                to: ChannelClass::Public,
            }),
            "an encrypted channel can never be downgraded to public"
        );
        assert!(
            ChannelClass::Public
                .may_become(ChannelClass::Encrypted)
                .is_err()
        );
        assert_eq!(
            ChannelClass::Public.may_become(ChannelClass::Public),
            Ok(())
        );
        assert_eq!(ChannelClass::default(), ChannelClass::Encrypted);
    }

    #[test]
    fn class_names_round_trip_and_reject_unknown_values() {
        for class in [ChannelClass::Encrypted, ChannelClass::Public] {
            assert_eq!(ChannelClass::parse(class.as_str()), Some(class));
        }
        assert_eq!(ChannelClass::parse("Public"), None);
        assert_eq!(ChannelClass::parse(""), None);
    }
}
