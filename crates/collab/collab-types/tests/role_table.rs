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

#[test]
fn matrix_owner() {
    for kind in all_artifact_kinds() {
        for op in all_op_classes() {
            assert!(
                role_permits(Role::Owner, kind, op),
                "Owner must permit {op:?} on {kind:?}",
            );
        }
    }
}

#[test]
fn matrix_editor() {
    for kind in all_artifact_kinds() {
        // Editor permits everything except Govern + Compaction.
        assert!(role_permits(Role::Editor, kind, OpClass::Splice));
        assert!(role_permits(Role::Editor, kind, OpClass::Move));
        assert!(role_permits(Role::Editor, kind, OpClass::EditMeta));
        assert!(role_permits(Role::Editor, kind, OpClass::Comment));
        assert!(role_permits(Role::Editor, kind, OpClass::Tombstone));
        assert!(role_permits(Role::Editor, kind, OpClass::Restore));
        assert!(!role_permits(Role::Editor, kind, OpClass::Govern));
        assert!(!role_permits(Role::Editor, kind, OpClass::Compaction));
        assert!(!role_permits(Role::Editor, kind, OpClass::Redact));
    }
}

#[test]
fn matrix_commenter() {
    for kind in all_artifact_kinds() {
        // Commenter permits only Comment.
        assert!(!role_permits(Role::Commenter, kind, OpClass::Splice));
        assert!(!role_permits(Role::Commenter, kind, OpClass::Move));
        assert!(!role_permits(Role::Commenter, kind, OpClass::EditMeta));
        assert!(role_permits(Role::Commenter, kind, OpClass::Comment));
        assert!(!role_permits(Role::Commenter, kind, OpClass::Tombstone));
        assert!(!role_permits(Role::Commenter, kind, OpClass::Restore));
        assert!(!role_permits(Role::Commenter, kind, OpClass::Govern));
        assert!(!role_permits(Role::Commenter, kind, OpClass::Compaction));
        assert!(!role_permits(Role::Commenter, kind, OpClass::Redact));
    }
}

#[test]
fn matrix_viewer() {
    for kind in all_artifact_kinds() {
        for op in all_op_classes() {
            assert!(
                !role_permits(Role::Viewer, kind, op),
                "Viewer must not permit {op:?} on {kind:?}",
            );
        }
    }
}

#[test]
fn matrix_denied() {
    for kind in all_artifact_kinds() {
        for op in all_op_classes() {
            assert!(
                !role_permits(Role::Denied, kind, op),
                "Denied must not permit {op:?} on {kind:?}",
            );
        }
    }
}

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
        (Role::Editor, OpClass::Compaction, false),
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
