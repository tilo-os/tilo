//! Cross-check `role_permits` against the spec's documented matrix
//! for every (Role, `ArtifactKind`, `OpClass`) triple.
//!
//! The matrix has 5 roles × 5 artifact kinds × 8 op classes = 200
//! entries. Most are derivable from a few rules ("Owner permits
//! everything", "Viewer permits nothing"), but writing them out
//! exhaustively means a code change that flips a single row will
//! fail loudly here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use collab_types::{ArtifactKind, OpClass, Role, role_permits};

fn all_artifact_kinds() -> [ArtifactKind; 5] {
    [
        ArtifactKind::Note,
        ArtifactKind::File,
        ArtifactKind::Cluster,
        ArtifactKind::Kanban,
        ArtifactKind::ChatLog,
    ]
}

fn all_op_classes() -> [OpClass; 9] {
    [
        OpClass::Splice,
        OpClass::Move,
        OpClass::EditMeta,
        OpClass::Comment,
        OpClass::Tombstone,
        OpClass::Restore,
        OpClass::Govern,
        OpClass::Compaction,
        OpClass::Redact,
    ]
}

// The per-role matrix tests (`matrix_owner`, `matrix_editor`,
// `matrix_commenter`, `matrix_viewer`, `matrix_denied`) here and the
// per-role unit tests formerly in `src/role.rs` asserted a subset of the
// rows of this table over the same kinds, so they were folded into it.
#[test]
fn full_matrix_table_driven() {
    // Exhaustive table: every triple's expected outcome, listed
    // explicitly. If the spec changes, only this table moves.
    let cases: &[(Role, OpClass, bool)] = &[
        // Owner -> everything true (per-kind same)
        (Role::Owner, OpClass::Splice, true),
        (Role::Owner, OpClass::Move, true),
        (Role::Owner, OpClass::EditMeta, true),
        (Role::Owner, OpClass::Comment, true),
        (Role::Owner, OpClass::Tombstone, true),
        (Role::Owner, OpClass::Restore, true),
        (Role::Owner, OpClass::Govern, true),
        (Role::Owner, OpClass::Compaction, true),
        (Role::Owner, OpClass::Redact, true),
        // Editor -> all except Govern + Compaction + Redact
        (Role::Editor, OpClass::Splice, true),
        (Role::Editor, OpClass::Move, true),
        (Role::Editor, OpClass::EditMeta, true),
        (Role::Editor, OpClass::Comment, true),
        (Role::Editor, OpClass::Tombstone, true),
        (Role::Editor, OpClass::Restore, true),
        (Role::Editor, OpClass::Govern, false),
        // Only the channel owner may emit a Compaction checkpoint — an
        // Editor with full mutate permissions still does NOT get to evict
        // channel history.
        (Role::Editor, OpClass::Compaction, false),
        // Redaction is a governance act: an Editor can tombstone (the
        // soft-delete "I changed my mind" primitive) but cannot redact — the
        // role table pins Redact to the channel owner.
        (Role::Editor, OpClass::Redact, false),
        // Commenter -> Comment only
        (Role::Commenter, OpClass::Splice, false),
        (Role::Commenter, OpClass::Move, false),
        (Role::Commenter, OpClass::EditMeta, false),
        (Role::Commenter, OpClass::Comment, true),
        (Role::Commenter, OpClass::Tombstone, false),
        (Role::Commenter, OpClass::Restore, false),
        (Role::Commenter, OpClass::Govern, false),
        (Role::Commenter, OpClass::Compaction, false),
        (Role::Commenter, OpClass::Redact, false),
        // Viewer -> nothing
        (Role::Viewer, OpClass::Splice, false),
        (Role::Viewer, OpClass::Move, false),
        (Role::Viewer, OpClass::EditMeta, false),
        (Role::Viewer, OpClass::Comment, false),
        (Role::Viewer, OpClass::Tombstone, false),
        (Role::Viewer, OpClass::Restore, false),
        (Role::Viewer, OpClass::Govern, false),
        (Role::Viewer, OpClass::Compaction, false),
        (Role::Viewer, OpClass::Redact, false),
        // Denied -> nothing
        (Role::Denied, OpClass::Splice, false),
        (Role::Denied, OpClass::Move, false),
        (Role::Denied, OpClass::EditMeta, false),
        (Role::Denied, OpClass::Comment, false),
        (Role::Denied, OpClass::Tombstone, false),
        (Role::Denied, OpClass::Restore, false),
        (Role::Denied, OpClass::Govern, false),
        (Role::Denied, OpClass::Compaction, false),
        (Role::Denied, OpClass::Redact, false),
    ];
    // The table is exhaustive: every role lists every op class exactly once.
    for role in [
        Role::Owner,
        Role::Editor,
        Role::Commenter,
        Role::Viewer,
        Role::Denied,
    ] {
        for op in all_op_classes() {
            let rows = cases
                .iter()
                .filter(|(r, o, _)| *r == role && *o == op)
                .count();
            assert_eq!(rows, 1, "table must list ({role:?}, {op:?}) exactly once");
        }
    }
    for kind in all_artifact_kinds() {
        for (role, op, expected) in cases.iter().copied() {
            assert_eq!(
                role_permits(role, kind, op),
                expected,
                "row mismatch: role_permits({role:?}, {kind:?}, {op:?}) expected {expected}",
            );
        }
    }
}
