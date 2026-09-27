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
    CapOverride, CapRole, ResourceRef, Scope, Subject, Verb, Workspace, WorkspaceId,
};

#[test]
fn permission_state_round_trips_decides_and_is_admin_only() {
    // Phase `membership_round_trips_as_a_role`.
    {
        let mut ws = Workspace::new(WorkspaceId::new(), "Design");
        ws.set_owner("host");
        let room = ws.id.raw().to_string();
        ws.grant_as(
            &"host".to_string(),
            CapOverride::membership(Subject::person("editor-user"), room, Verb::Write),
        )
        .expect("the owner may write a membership");

        assert_eq!(
            ws.role_of(&"editor-user".to_string()),
            Some(CapRole::Editor)
        );
        assert_eq!(ws.role_of(&"host".to_string()), Some(CapRole::Owner));
        assert_eq!(
            ws.role_of(&"stranger".to_string()),
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
                Verb::Write,
            ),
        )
        .expect("the owner may grant");

        let actor = ws.actor_for_app("Files");
        assert!(
            ws.allows(
                &actor,
                &ResourceRef::FsPath("/var/workspace/design/brief.md".into()),
                Verb::Write
            ),
            "a path capability inherits down the tree"
        );
        assert!(
            !ws.allows(
                &actor,
                &ResourceRef::FsPath("/etc/shadow".into()),
                Verb::Write
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
                Verb::Write,
            ),
        );
        assert!(
            refused.is_none(),
            "grant_as must refuse a non-administrator"
        );
        assert_eq!(
            ws.capabilities.len(),
            1,
            "only the owner's own membership is recorded"
        );
    }
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use shell::shell::state::WorkspaceExt as _;
