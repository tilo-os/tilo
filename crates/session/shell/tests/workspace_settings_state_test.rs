//! Workspace permission state round-trips through the one model the
//! enforcement points read: what a member may do is the verb on their
//! membership capability, and what an app may touch is a capability
//! scoped to a path or a device. The assertions below write a
//! capability, read it back, and check the value against the model
//! that actually decides.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::state::FreshId as _;
use shell::shell::state::WorkspaceAuthority as _;
use shell::shell::state::{
    Access, CapOverride, CapRole, ResourceRef, Scope, Subject, Workspace, WorkspaceId,
};

#[test]
fn permission_state_round_trips_decides_and_is_admin_only() {
    // Phase `membership_round_trips_as_a_role`.
    {
        let mut ws = Workspace::new(WorkspaceId::new(), "Design");
        ws.set_owner("host");
        ws.govern_as_at(
            "host",
            tilo_action_policy::Change::AddMember {
                person: "editor-user".into(),
                role: tilo_action_policy::Role::Editor,
            },
            tilo_clock::unix_now(),
        )
        .expect("the owner may write a membership");

        assert_eq!(ws.role_of("editor-user"), Some(CapRole::Editor));
        assert_eq!(ws.role_of("host"), Some(CapRole::Owner));
        assert_eq!(
            ws.role_of("stranger"),
            None,
            "not a member is distinct from a member with no rights"
        );
        let roster = ws.roster();
        assert_eq!(roster.len(), 2);
        assert!(
            roster
                .iter()
                .any(|(u, r)| u == "editor-user" && *r == CapRole::Editor)
        );
    }

    // Phase `a_file_capability_round_trips_and_decides`.
    {
        let mut ws = Workspace::new(WorkspaceId::new(), "Design");
        ws.set_owner("host");
        ws.grant_as(
            &"host".to_string(),
            CapOverride::grant(
                Subject::app("Files"),
                Scope::FsPath("/var/workspace/design".into()),
                Access::Write,
            ),
        )
        .expect("the owner may grant");

        let actor = ws.actor_for_app("Files");
        assert!(
            ws.allows(
                &actor,
                &ResourceRef::FsPath("/var/workspace/design/brief.md".into()),
                Access::Write
            ),
            "a path capability inherits down the tree"
        );
        assert!(
            !ws.allows(
                &actor,
                &ResourceRef::FsPath("/etc/shadow".into()),
                Access::Write
            ),
            "and does not leak outside it"
        );
    }

    // Phase `only_an_administrator_may_write_permission_state`.
    {
        let mut ws = Workspace::new(WorkspaceId::new(), "Design");
        ws.set_owner("host");
        let refused = ws.grant_as(
            &"stranger".to_string(),
            CapOverride::grant(
                Subject::app("Files"),
                Scope::FsPath("/var/workspace".into()),
                Access::Write,
            ),
        );
        assert!(
            refused.is_none(),
            "grant_as must refuse a non-administrator"
        );
        assert!(ws.sandbox.is_empty(), "nothing is recorded");
    }
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use shell::shell::state::WorkspaceExt as _;
