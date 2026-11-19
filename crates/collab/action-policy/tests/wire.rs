//! Postcard names an enum variant by its position, and workspaces keep their
//! grants, and the digests of their administration requests, in those
//! bytes. A variant removed or moved changes bytes a workspace already holds and
//! fails here. A new variant goes at the end of its enum; the `match`es
//! without a wildcard ask for its bytes.

use tilo_action_policy::{ActionKind, Answer, Grant, Grantee, KindSet, Lifetime, Role, TreePath};

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
            (Grantee::agent("a"), &[0x00, 0x01, 0x01, b'a']),
            (Grantee::bot("b"), &[0x00, 0x02, 0x01, b'b']),
            (Grantee::role(Role::Editor), &[0x01, 0x02]),
            (Grantee::app("n"), &[0x02, 0x01, b'n']),
        ];
        for (grantee, golden) in cases {
            match grantee {
                Grantee::Member(_) | Grantee::Role(_) | Grantee::App(_) | Grantee::CustomRole(_) => {
                    stored_as!(grantee, golden)
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
}
