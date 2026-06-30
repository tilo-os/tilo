//! Roles, op classes, and the spec's static permission table.
//!
//! `Role` and `OpClass` are the two enums that drive every "is this
//! mutation allowed?" decision in the system. `role_permits` is the
//! lookup function — the single source of truth, consulted by both
//! the local shell (to paint affordances and grey out forbidden
//! actions) and every receiving peer's validator (to reject invalid
//! ops on the wire).
//!
//! The table is intentionally hard-coded at compile time. Adding a
//! new role or op class is a schema-breaking change that needs a
//! version bump on every peer, so we don't pretend it is data.

use serde::{Deserialize, Serialize};

/// Roles a member can hold in a channel.
///
/// Roles are ordered logically (Owner most privileged, Denied least)
/// but `Ord` is **not** derived: ordering by enum discriminant would
/// be misleading because `Denied` is not "less than Viewer" — it is
/// an *explicit block* that overrides any inherited grant from the
/// channel's `default_role`. Callers comparing privilege must do so
/// against the permission table, not by ordering roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Role {
    /// Channel governance authority. Can issue + revoke
    /// attestations, transfer ownership, edit channel settings, and
    /// — by extension — perform every op class on every artifact.
    Owner,
    /// Mutate any artifact, but cannot govern. Editors are the
    /// default role for trust-rich teams.
    Editor,
    /// Append-only. Can comment + react but never splice, move,
    /// rename, or delete.
    Commenter,
    /// Read-only. No signed ops accepted from this role. Used for
    /// auditors, observers, and lurkers.
    Viewer,
    /// Explicit block. Overrides any inherited grant. Used to
    /// kick-but-not-delete: the member remains in the channel
    /// directory (so their historical attribution survives) but
    /// cannot apply any op of any class.
    Denied,
}

/// The semantic class of a single CRDT op, declared in the op
/// envelope itself.
///
/// The classification is **author-declared**, not validator-derived,
/// so a malicious client could lie. The validator catches lies by
/// re-checking the role table; the broker never relies on the
/// declared class for routing, only for fan-out filtering at coarse
/// granularity (e.g. only forwarding Comment-class ops to
/// Commenter-tier members).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum OpClass {
    /// Text edit. Insert/delete bytes into a CRDT text field.
    Splice,
    /// Reorder operation. Kanban card moves, file reordering,
    /// strip-swap in the compositor model.
    Move,
    /// Metadata mutation. Rename, retag, change description.
    EditMeta,
    /// Append a comment or reaction. The Commenter role's *only*
    /// permitted class.
    Comment,
    /// Soft delete. Marks the artifact as deleted but preserves
    /// history; the artifact's data and op log remain.
    Tombstone,
    /// Undo a tombstone. Available to Editor+ on every artifact
    /// kind.
    Restore,
    /// Governance op. Issue/revoke attestations, transfer ownership,
    /// rename channel, update channel settings. Only Owner role
    /// permits this class.
    Govern,
    /// Phase 6: host-driven compaction checkpoint. The channel's
    /// host (initial: the channel owner's primary device) emits one
    /// of these every N ops or M bytes; recipients use it to evict
    /// older op-log entries from in-memory storage and snapshot the
    /// document state to cold history. Only `Owner` role permits this
    /// class — the eventual quorum-elected backup host is a v2
    /// follow-up. The payload of a `SignedOp` with this class is a
    /// [`crate::compaction::CompactionCheckpoint`] encoded with
    /// [`crate::canonical::canonical_bytes`].
    Compaction,
    /// Phase 9 (second half): cooperative cryptographic redaction.
    /// The channel **owner** (and only the owner — see the role table
    /// in [`role_permits`]) mints a Redact op to declare "this earlier
    /// op is now compliance-required to be removed; peers, please
    /// honour it." The payload is a
    /// [`crate::redaction::RedactRequest`] encoded with
    /// [`crate::canonical::canonical_bytes`]. See
    /// `docs/superpowers/specs/2026-06-30-cryptographic-redaction-design.md`
    /// for the full threat model + rationale.
    ///
    /// Recursive redaction (a `Redact` whose `target_op_id` resolves to
    /// another `Redact`) is rejected by the validator — see
    /// [`crate::validator::ValidationError::RecursiveRedaction`].
    Redact,
    /// Announce a new live-streamed Wayland surface for a hosted
    /// legacy app. Payload is a
    /// [`crate::surface_stream::SurfaceStreamStart`] encoded with
    /// [`crate::canonical::canonical_bytes`].
    ///
    /// Hosting a remote-desktop surface is an authorial act — the
    /// host is publishing live pixels of an app the channel members
    /// will see and interact with — so the role table pins this class
    /// to `Editor`+. A `Commenter` or `Viewer` cannot start a stream.
    SurfaceStreamStart,
    /// Retire a previously-announced streamed surface. Payload is a
    /// [`crate::surface_stream::SurfaceStreamEnd`]. Same role gate as
    /// `SurfaceStreamStart` — only the originating editor / owner may
    /// take a stream down.
    SurfaceStreamEnd,
    /// One encoded video frame on an active stream. Payload is a
    /// [`crate::surface_stream::SurfaceFrame`].
    ///
    /// Same role gate as the Start / End pair: emitting frames is
    /// continuous authorship of the stream the editor opened. Routing
    /// the frame class through the same `OpClass` machinery as every
    /// other mutation is deliberate — the audit log, replay defence,
    /// and attestation flow apply uniformly.
    SurfaceFrame,
    /// Remote pointer / keyboard event from a channel member back to
    /// the host. Payload is a [`crate::surface_stream::RemoteInput`].
    ///
    /// Pinned to `Editor`+ because input injection mutates the live
    /// app's state on the host's machine. A `Commenter` watching a
    /// shared Inkscape session cannot drive Inkscape's cursor; they
    /// can only watch.
    RemoteInput,
}

/// The categorical kind of an artifact.
///
/// Each kind has its own row in the permission table because
/// "Comment on a Kanban" means something different from
/// "Comment on a Note" — the latter is a side-anchor on text, the
/// former is a card-attached thread. The toolkit handles both
/// uniformly; the permission rule is the same shape either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ArtifactKind {
    /// Free-form rich-text document. The qOS Notes app's primitive.
    Note,
    /// Binary file backed by the P2P FUSE layer. Op classes that
    /// "mutate" are interpreted as filesystem operations.
    File,
    /// A compositor cluster session — a tiled set of running
    /// windows that can be saved + reopened collaboratively.
    Cluster,
    /// Kanban board. `Move` is a first-class op for card
    /// transitions; `Splice` lands on card body text.
    Kanban,
    /// Append-only chat log inside a channel.
    ChatLog,
}

/// The spec's static permission table.
///
/// Returns `true` iff a member holding `role` is permitted to
/// emit an op of `op` class against an artifact of `kind`.
/// Owner permits everything (including Govern). Editor permits
/// every class except Govern, on every artifact kind. Commenter
/// permits Comment only. Viewer and Denied permit nothing.
///
/// The table is exhaustive — every (Role, ArtifactKind, OpClass)
/// triple has a defined answer. The tests in `tests/role_table.rs`
/// cross-check every row.
///
/// Stability: this is a wire-protocol decision. Adding a row
/// (e.g. a new ArtifactKind) is allowed; flipping an existing
/// row's value is a breaking change because two peers running
/// different versions would disagree on whether an op is valid.
pub fn role_permits(role: Role, _kind: ArtifactKind, op: OpClass) -> bool {
    // The implementation could in principle vary per artifact kind
    // (e.g. "Comment on a File doesn't exist"), but the spec's
    // table treats every kind uniformly: the role determines the
    // permitted op classes, the kind doesn't subtract from that.
    // If a future row needs to be kind-specific, branch here on
    // `_kind` and document the carve-out in the spec first.
    match role {
        Role::Owner => true,
        Role::Editor => match op {
            OpClass::Splice
            | OpClass::Move
            | OpClass::EditMeta
            | OpClass::Comment
            | OpClass::Tombstone
            | OpClass::Restore
            // Hosting a live-streamed Wayland surface (and driving
            // input back into it) is authorial — the editor publishes
            // pixels and accepts remote events that mutate the live
            // app. The role table pins all four surface-stream ops to
            // Editor+ so a Commenter or Viewer cannot start, end,
            // emit frames for, or inject input into a stream.
            | OpClass::SurfaceStreamStart
            | OpClass::SurfaceStreamEnd
            | OpClass::SurfaceFrame
            | OpClass::RemoteInput => true,
            // Editors mutate artifacts; they don't govern, they
            // don't issue compaction checkpoints (Phase 6 v1 pins the
            // host to the channel owner), and they don't redact.
            // Redaction is a governance act and stays with governance
            // authority; an editor who wants something removed asks
            // an owner.
            OpClass::Govern | OpClass::Compaction | OpClass::Redact => false,
        },
        Role::Commenter => matches!(op, OpClass::Comment),
        Role::Viewer => false,
        Role::Denied => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_can_do_everything() {
        for kind in [
            ArtifactKind::Note,
            ArtifactKind::File,
            ArtifactKind::Cluster,
            ArtifactKind::Kanban,
            ArtifactKind::ChatLog,
        ] {
            for op in [
                OpClass::Splice,
                OpClass::Move,
                OpClass::EditMeta,
                OpClass::Comment,
                OpClass::Tombstone,
                OpClass::Restore,
                OpClass::Govern,
                OpClass::Compaction,
                OpClass::Redact,
                OpClass::SurfaceStreamStart,
                OpClass::SurfaceStreamEnd,
                OpClass::SurfaceFrame,
                OpClass::RemoteInput,
            ] {
                assert!(
                    role_permits(Role::Owner, kind, op),
                    "Owner should permit {:?} on {:?}",
                    op,
                    kind
                );
            }
        }
    }

    #[test]
    fn editor_cannot_govern() {
        assert!(!role_permits(
            Role::Editor,
            ArtifactKind::Note,
            OpClass::Govern
        ));
    }

    #[test]
    fn editor_cannot_compact() {
        // Phase 6 v1: only the channel owner may emit a Compaction
        // checkpoint. An Editor with full mutate permissions still
        // does NOT get to evict history for the channel.
        assert!(!role_permits(
            Role::Editor,
            ArtifactKind::Note,
            OpClass::Compaction
        ));
    }

    #[test]
    fn editor_cannot_redact() {
        // Phase 9 (second half): redaction is a governance act. An
        // Editor with full mutate permissions can tombstone — the
        // soft-delete primitive that says "I changed my mind about
        // this edit" — but cannot redact: the validator's role table
        // pins Redact to the channel owner.
        assert!(!role_permits(
            Role::Editor,
            ArtifactKind::Note,
            OpClass::Redact
        ));
    }

    #[test]
    fn commenter_only_comment() {
        for op in [
            OpClass::Splice,
            OpClass::Move,
            OpClass::EditMeta,
            OpClass::Tombstone,
            OpClass::Restore,
            OpClass::Govern,
            OpClass::Compaction,
            OpClass::Redact,
            OpClass::SurfaceStreamStart,
            OpClass::SurfaceStreamEnd,
            OpClass::SurfaceFrame,
            OpClass::RemoteInput,
        ] {
            assert!(!role_permits(Role::Commenter, ArtifactKind::Note, op));
        }
        assert!(role_permits(
            Role::Commenter,
            ArtifactKind::Note,
            OpClass::Comment
        ));
    }

    #[test]
    fn viewer_nothing() {
        for op in [
            OpClass::Splice,
            OpClass::Move,
            OpClass::EditMeta,
            OpClass::Comment,
            OpClass::Tombstone,
            OpClass::Restore,
            OpClass::Govern,
            OpClass::Compaction,
            OpClass::Redact,
            OpClass::SurfaceStreamStart,
            OpClass::SurfaceStreamEnd,
            OpClass::SurfaceFrame,
            OpClass::RemoteInput,
        ] {
            assert!(!role_permits(Role::Viewer, ArtifactKind::Kanban, op));
        }
    }

    #[test]
    fn denied_nothing() {
        for op in [
            OpClass::Splice,
            OpClass::Move,
            OpClass::EditMeta,
            OpClass::Comment,
            OpClass::Tombstone,
            OpClass::Restore,
            OpClass::Govern,
            OpClass::Compaction,
            OpClass::Redact,
            OpClass::SurfaceStreamStart,
            OpClass::SurfaceStreamEnd,
            OpClass::SurfaceFrame,
            OpClass::RemoteInput,
        ] {
            assert!(!role_permits(Role::Denied, ArtifactKind::File, op));
        }
    }

    #[test]
    fn editor_can_drive_surface_streams() {
        // Pin the role-table decision for legacy-app remote-desktop
        // hosting: an Editor on the channel may emit all four
        // surface-stream op classes. Hosting a stream is authorial,
        // and so is injecting remote input into the hosted app.
        for kind in [
            ArtifactKind::Note,
            ArtifactKind::File,
            ArtifactKind::Cluster,
            ArtifactKind::Kanban,
            ArtifactKind::ChatLog,
        ] {
            assert!(role_permits(
                Role::Editor,
                kind,
                OpClass::SurfaceStreamStart
            ));
            assert!(role_permits(
                Role::Editor,
                kind,
                OpClass::SurfaceStreamEnd
            ));
            assert!(role_permits(Role::Editor, kind, OpClass::SurfaceFrame));
            assert!(role_permits(Role::Editor, kind, OpClass::RemoteInput));
        }
    }

    #[test]
    fn commenter_cannot_drive_surface_streams() {
        // Sister test to `editor_can_drive_surface_streams`: a
        // Commenter (or any sub-Editor role) cannot start, end, emit
        // frames for, or inject input into a stream.
        for op in [
            OpClass::SurfaceStreamStart,
            OpClass::SurfaceStreamEnd,
            OpClass::SurfaceFrame,
            OpClass::RemoteInput,
        ] {
            assert!(!role_permits(Role::Commenter, ArtifactKind::Cluster, op));
            assert!(!role_permits(Role::Viewer, ArtifactKind::Cluster, op));
            assert!(!role_permits(Role::Denied, ArtifactKind::Cluster, op));
        }
    }
}
