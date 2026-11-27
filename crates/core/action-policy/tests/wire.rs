//! Postcard names an enum variant by its position, and workspaces keep their
//! grants, and the digests of their administration requests, in those
//! bytes. A variant removed or moved changes bytes a workspace already holds and
//! fails here. A new variant goes at the end of its enum; the `match`es
//! without a wildcard ask for its bytes.

use tilo_action_policy::{
    Answer, Authority, Change, Context, Grant, GrantId, Grantee, Role, TreePath, WorkspacePolicy,
};
use tilo_action_types::{ActionKind, KindSet, Lifetime};

fn read_as<T: serde::de::DeserializeOwned>(_: &T, bytes: &[u8]) -> postcard::Result<T> {
    postcard::from_bytes(bytes)
}

/// `$value` is written as `$golden` and reads back as itself.
macro_rules! stored_as {
    ($value:expr, $golden:expr) => {{
        let value = $value;
        let golden: &[u8] = $golden;
        assert_eq!(
            postcard::to_stdvec(value).unwrap(),
            golden,
            "{value:?} is written as workspaces hold it"
        );
        assert_eq!(&read_as(value, golden).unwrap(), value);
    }};
}

tilo_test_env::scenario! {
    fn stored_policy_bytes_keep_their_meaning;

    phase an_answer_keeps_its_bytes;
    {
        let cases: &[(Answer, &[u8])] = &[
            (Answer::Deny, &[0x00]),
            (Answer::Ask, &[0x01]),
            (Answer::Allow, &[0x02]),
        ];
        for (answer, golden) in cases {
            match answer {
                Answer::Deny | Answer::Ask | Answer::Allow => stored_as!(answer, golden),
            }
        }
    }

    phase a_role_keeps_its_bytes;
    {
        let cases: &[(Role, &[u8])] = &[
            (Role::Viewer, &[0x00]),
            (Role::Commenter, &[0x01]),
            (Role::Editor, &[0x02]),
            (Role::Moderator, &[0x03]),
            (Role::Owner, &[0x04]),
        ];
        for (role, golden) in cases {
            match role {
                Role::Viewer | Role::Commenter | Role::Editor | Role::Moderator | Role::Owner => {
                    stored_as!(role, golden);
                }
            }
        }
    }

    phase an_action_kind_keeps_its_bytes;
    {
        let cases: &[(ActionKind, &[u8])] = &[
            (ActionKind::Read, &[0x00]),
            (ActionKind::Undoable, &[0x01]),
            (ActionKind::Outside, &[0x02]),
        ];
        for (kind, golden) in cases {
            match kind {
                ActionKind::Read | ActionKind::Undoable | ActionKind::Outside => {
                    stored_as!(kind, golden);
                }
            }
        }
        stored_as!(&KindSet::ALL, &[0x03, 0x00, 0x01, 0x02]);
    }

    phase a_grantee_keeps_its_bytes;
    {
        let cases: &[(Grantee, &[u8])] = &[
            (Grantee::person("p"), &[0x00, 0x00, 0x01, b'p']),
            (Grantee::agent("a", "p"), &[0x00, 0x01, 0x01, b'a', 0x01, b'p']),
            (Grantee::bot("b"), &[0x00, 0x02, 0x01, b'b']),
            (Grantee::role(Role::Editor), &[0x01, 0x02]),
            (Grantee::app("n"), &[0x02, 0x01, b'n']),
            (Grantee::AnyKey, &[0x04]),
        ];
        for (grantee, golden) in cases {
            match grantee {
                Grantee::Member(_)
                | Grantee::Role(_)
                | Grantee::App(_)
                | Grantee::CustomRole(_)
                | Grantee::AnyKey => {
                    stored_as!(grantee, golden);
                }
            }
        }
    }

    phase a_lifetime_keeps_its_bytes;
    {
        let cases: &[(Lifetime, &[u8])] = &[
            (Lifetime::Once, &[0x00]),
            (Lifetime::Session, &[0x01]),
            (Lifetime::Until(5), &[0x02, 0x05]),
            (Lifetime::UntilRevoked, &[0x03]),
        ];
        for (lifetime, golden) in cases {
            match lifetime {
                Lifetime::Once | Lifetime::Session | Lifetime::Until(_) | Lifetime::UntilRevoked => {
                    stored_as!(lifetime, golden);
                }
            }
        }
    }

    phase a_grant_keeps_its_bytes;
    {
        let grant = Grant::new(
            "g",
            Grantee::person("p"),
            TreePath::workspace("r"),
            KindSet::ALL,
            Answer::Allow,
            Grantee::person("c"),
        );
        let mut golden = vec![0x01, b'g'];
        golden.extend([0x00, 0x00, 0x01, b'p']);
        golden.push(0x0b);
        golden.extend(b"workspace:r");
        golden.extend([0x03, 0x00, 0x01, 0x02]);
        golden.push(0x02);
        golden.push(0x03);
        golden.extend([0x00, 0x00, 0x01, b'c']);
        stored_as!(&grant, &golden);
    }

    // A membership operation signs its changes' bytes and a workspace keeps
    // them: an ownership transfer that read as another variant, or not at all,
    // would leave a kept workspace owned by whoever it was handed away from,
    // and its state would no longer open as that owner's.
    phase a_change_keeps_its_bytes;
    {
        let p = || "p".to_owned();
        let cases: Vec<(Change, Vec<u8>)> = vec![
            (Change::AddMember { person: p(), role: Role::Editor }, vec![0x00, 0x01, b'p', 0x02]),
            (Change::RemoveMember { person: p() }, vec![0x01, 0x01, b'p']),
            (Change::Ban { person: p() }, vec![0x02, 0x01, b'p']),
            (Change::Unban { person: p() }, vec![0x03, 0x01, b'p']),
            (Change::Restrict { person: p(), until_ms: 5 }, vec![0x04, 0x01, b'p', 0x05]),
            (Change::Unrestrict { person: p() }, vec![0x05, 0x01, b'p']),
            (Change::SetRole { person: p(), role: Role::Viewer }, vec![0x06, 0x01, b'p', 0x00]),
            (Change::Revoke(GrantId::new("g")), vec![0x08, 0x01, b'g']),
            (
                Change::DefineRole { id: "i".into(), name: "n".into() },
                vec![0x09, 0x01, b'i', 0x01, b'n'],
            ),
            (Change::RemoveRole { id: "i".into() }, vec![0x0a, 0x01, b'i']),
            (
                Change::RetiredSetMemberRoles { person: p(), roles: ["i".to_owned()].into() },
                vec![0x0b, 0x01, b'p', 0x01, 0x01, b'i'],
            ),
            (
                Change::SetMemberCustomRole { person: p(), role: "i".into(), assigned: true },
                vec![0x0c, 0x01, b'p', 0x01, b'i', 0x01],
            ),
            (Change::TransferOwnership { person: p() }, vec![0x0d, 0x01, b'p']),
            (
                Change::SetScopedRole {
                    grantee: Grantee::role(Role::Viewer),
                    scope: TreePath::workspace("r").app("f"),
                    role: Some(Role::Editor),
                },
                [&[0x0e, 0x01, 0x00, 17][..], b"workspace:r/app:f", &[0x01, 0x02]].concat(),
            ),

        ];
        for (change, golden) in &cases {
            match change {
                Change::AddMember { .. }
                | Change::RemoveMember { .. }
                | Change::Ban { .. }
                | Change::Unban { .. }
                | Change::Restrict { .. }
                | Change::Unrestrict { .. }
                | Change::SetRole { .. }
                | Change::Grant(_)
                | Change::Revoke(_)
                | Change::DefineRole { .. }
                | Change::RemoveRole { .. }
                | Change::RetiredSetMemberRoles { .. }
                | Change::SetMemberCustomRole { .. }
                | Change::TransferOwnership { .. }
                | Change::SetScopedRole { .. } => stored_as!(change, golden),
            }
        }
        let grant = Grant::new(
            "g",
            Grantee::person("p"),
            TreePath::workspace("r"),
            KindSet::ALL,
            Answer::Allow,
            Grantee::person("c"),
        );
        let mut golden = vec![0x07];
        golden.extend(postcard::to_stdvec(&grant).unwrap());
        stored_as!(&Change::Grant(grant), &golden);

        // The whole-set assignment keeps its place and changes nothing.
        let mut workspace = WorkspacePolicy::new("r", "a11ce");
        let held = workspace.clone();
        let owner = Authority::person(held.party("a11ce"));
        let assignment = Change::RetiredSetMemberRoles { person: p(), roles: ["i".to_owned()].into() };
        assert!(workspace.apply("a11ce", &owner, assignment, &Context::at(1), false).is_err());
        assert_eq!(workspace, held);
    }

    // A build from before roles took a scope reads changes up to 0x0d and refuses the whole
    // operation that carries a share: it gives that build's devices no membership at all, never
    // a wider one, since a share never takes a role over the whole workspace away
    // (`policy.rs`, the scoped roles scenario).
    phase a_build_before_scopes_reads_no_share;
    {
        #[derive(serde::Deserialize, Debug)]
        enum Before { V0, V1, V2, V3, V4, V5, V6, V7, V8, V9, V10, V11, V12, V13 }
        let share = Change::SetScopedRole {
            grantee: Grantee::person("p"),
            scope: TreePath::workspace("r").app("f"),
            role: None,
        };
        let bytes = postcard::to_stdvec(&share).unwrap();
        assert!(postcard::from_bytes::<Before>(&bytes).is_err());
        assert!(postcard::from_bytes::<Before>(&[0x0d]).is_ok());
    }
}
