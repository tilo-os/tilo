//! Channels and artifacts — the working-area primitive.
//!
//! A channel groups members + artifacts inside a space. Channels are
//! cheap (no rekey overhead in this phase; MLS rekey lands in Phase 7)
//! so users create them liberally — one per workstream, project,
//! topic. Membership in a channel is a subset of the parent space's
//! members.
//!
//! ## Field rationale
//!
//! - `default_role` applies to space members not explicitly attested
//!   in `members`. The defaults table (spec "Defaults" section) is
//!   data-driven: `Editor` in small spaces, `Commenter` in mid, `Viewer`
//!   in large. The channel owner picks at creation.
//! - `observe_default` is for org-wide oversight: a space owner can
//!   drop in as a Viewer without explicit invitation.
//! - `min_clearance` and the per-artifact `classification` field
//!   compose: the effective floor for an artifact is
//!   `max(channel.min_clearance, artifact.classification)`. Lowering
//!   `min_clearance` is allowed only on a fresh channel.
//! - `presence_visible_for_viewers` is false by default — a Viewer
//!   is a lurker, not a teammate; making their cursor visible
//!   accidentally outs lurking which is hostile UX.

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
/// Stored as `{ id, name }` rather than the full `Channel` because
/// the same channel may be open in two places (wheel disc + browser)
/// and replicating the full struct would invite drift. The full
/// channel record is loaded by id on demand.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelRef {
    /// Stable id of the referenced channel.
    pub id: ChannelId,
    /// Cached display name. UI uses it without loading the full
    /// record; if stale, the user sees a one-frame mismatch which
    /// the next load corrects.
    pub name: String,
}

/// Lightweight reference from a `Channel` to an `Artifact`.
///
/// Mirrors `ChannelRef`'s rationale. The artifact's `crdt_doc` is
/// loaded lazily — large CRDT histories don't need to flow through
/// channel summaries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    /// Stable id of the referenced artifact.
    pub id: ArtifactId,
    /// Cached display name. May be stale.
    pub name: String,
    /// Cached artifact kind, so the UI can pick the right tile
    /// icon without loading the full record.
    pub kind: ArtifactKind,
}

/// An explicit per-member role grant on a channel.
///
/// `MemberRef`s override the channel's `default_role`. The
/// `attestation` field is the proof — a `Govern`-class signed event
/// in the channel's history minted this grant; the validator looks
/// it up by id when validating ops from this member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberRef {
    /// The member's identity public key.
    pub identity: Ed25519PublicKey,
    /// The granted role.
    pub role: Role,
    /// Id of the attestation that issued this grant. The validator
    /// resolves this id against the channel's attestation store
    /// and walks the trust chain back to an authorised signer.
    pub attestation: AttestationId,
}

/// A working-area inside a space.
///
/// Channels are the unit of role enforcement: every member has a
/// role *in this channel*. Artifacts inherit the channel's
/// `min_clearance` floor; they can carry a stricter
/// `classification` but never weaker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    /// Stable id for this channel.
    pub id: ChannelId,
    /// Parent space. Used by the validator to look up the space's
    /// trust roots when resolving non-channel-local attestations
    /// (e.g. `ClearedAt`).
    pub space: SpaceId,
    /// Human-meaningful display name. "#engineering". Not unique;
    /// not used in any equality decision.
    pub name: String,
    /// Owner identity. Initially the channel creator; transferred
    /// via `ChannelEvent::OwnershipTransfer`. The owner is the
    /// only key that can issue `Govern`-class ops *unless* they
    /// have delegated to another key via an `AttestationIssued`
    /// event granting `Role::Owner`.
    pub owner: Ed25519PublicKey,
    /// Role to apply to space members not explicitly listed in
    /// `members`. Affects no one not in the parent space.
    pub default_role: Role,
    /// Explicit per-member role grants. Overrides `default_role`.
    pub members: Vec<MemberRef>,
    /// If true, space owners can drop in as a Viewer without an
    /// explicit invite. Used for org-wide oversight; defaults false.
    pub observe_default: bool,
    /// Minimum clearance any member must hold to apply ops on
    /// artifacts inside this channel. `None` means no clearance
    /// gate (artifact-level `classification` may still apply).
    pub min_clearance: Option<Clearance>,
    /// Artifacts in this channel.
    pub artifacts: Vec<ArtifactRef>,
    /// Whether members with `Role::Viewer` are visible in the
    /// presence roster. Defaults to `false` — Viewer is intended
    /// as "lurker", and outing the lurker is hostile UX. Channel
    /// owners can flip this to `true` for transparent-audit
    /// rooms.
    pub presence_visible_for_viewers: bool,
}

/// A single artifact: a note, file, kanban board, etc.
///
/// The CRDT document handle (`crdt_doc`) is an opaque value
/// (currently `Option<Vec<u8>>` for an Automerge change-set snapshot
/// or a content-addressed reference). The exact representation is
/// not pinned in this crate because the storage layer is allowed
/// to vary across phases — the wire shape is what matters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    /// Stable id for this artifact.
    pub id: ArtifactId,
    /// Categorical kind. Drives the role-table lookup and the UI
    /// tile rendering.
    pub kind: ArtifactKind,
    /// Home channel. Cross-channel references (Phase 7+ feature)
    /// would point back here for governance.
    pub channel: ChannelId,
    /// Per-artifact classification, optionally stricter than the
    /// channel's `min_clearance`. May only be raised, never
    /// lowered, by a channel-owner-issued `Govern` op.
    pub classification: Option<Clearance>,
    /// Opaque CRDT-doc handle. The concrete type is deliberately
    /// loose: an Automerge change-set blob, a content-addressed
    /// reference into the FUSE layer, or `None` for an empty
    /// artifact. The schema does not pin the storage path.
    pub crdt_doc: Option<DocumentHandle>,
}

/// Opaque pointer to a CRDT document.
///
/// Wrapped in a newtype so future variants (a content-addressed
/// hash, a local path, an inline blob) can be added without
/// breaking the `Artifact` wire shape. Currently a length-prefixed
/// opaque byte blob.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DocumentHandle(#[serde(with = "serde_bytes")] pub Vec<u8>);

/// Delta applied by a `ChannelEvent::SettingsUpdate` event.
///
/// All fields are optional — only the fields present in the delta
/// are mutated on the channel. A single `Govern` op can therefore
/// rename + change `default_role` + flip `observe_default` in one
/// signed event.
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

    #[test]
    fn channel_round_trip() {
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
    }

    #[test]
    fn artifact_id_unique() {
        assert_ne!(ArtifactId::new(), ArtifactId::new());
    }

    #[test]
    fn settings_delta_default_is_all_none() {
        let d = ChannelSettingsDelta::default();
        assert!(d.default_role.is_none());
        assert!(d.observe_default.is_none());
        assert!(d.min_clearance.is_none());
        assert!(d.presence_visible_for_viewers.is_none());
    }
}
