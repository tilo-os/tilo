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
use crate::role::{ArtifactKind, Role};
use crate::space::SpaceId;

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
}
