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

#[test]
fn an_answer_keeps_its_bytes() {
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

#[test]
fn a_role_keeps_its_bytes() {
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

#[test]
fn an_action_kind_keeps_its_bytes() {
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

#[test]
fn a_grantee_keeps_its_bytes() {
    let cases: &[(Grantee, &[u8])] = &[
        (Grantee::person("p"), &[0x00, 0x00, 0x01, b'p']),
        (Grantee::agent("a"), &[0x00, 0x01, 0x01, b'a']),
        (Grantee::bot("b"), &[0x00, 0x02, 0x01, b'b']),
        (Grantee::role(Role::Editor), &[0x01, 0x02]),
    ];
    for (grantee, golden) in cases {
        match grantee {
            Grantee::Member(_) | Grantee::Role(_) => stored_as!(grantee, golden),
        }
    }
}

#[test]
fn a_lifetime_keeps_its_bytes_around_the_retired_task_lifetime() {
    let cases: &[(Lifetime, &[u8])] = &[
        (Lifetime::Once, &[0x00]),
        (Lifetime::Session("s1".into()), &[0x02, 0x02, b's', b'1']),
        (Lifetime::Until(5), &[0x03, 0x05]),
        (Lifetime::UntilRevoked, &[0x04]),
    ];
    for (lifetime, golden) in cases {
        match lifetime {
            Lifetime::Once | Lifetime::Session(_) | Lifetime::Until(_) | Lifetime::UntilRevoked => {
                stored_as!(lifetime, golden);
            }
        }
    }
}

#[test]
fn a_grant_given_while_a_task_was_open_still_reads_and_has_expired() {
    assert_eq!(
        postcard::from_bytes::<Lifetime>(&[0x01, 0x02, b't', b'1']).unwrap(),
        Lifetime::Until(0)
    );
}

#[test]
fn a_grant_keeps_its_bytes() {
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
    golden.push(0x06);
    golden.extend(b"room:r");
    golden.extend([0x03, 0x00, 0x01, 0x02]);
    golden.push(0x02);
    golden.push(0x04);
    golden.extend([0x00, 0x00, 0x01, b'c']);
    stored_as!(&grant, &golden);
}

#[test]
fn a_grant_written_while_the_level_was_named_for_workspaces_reads_as_the_same_grant() {
    // The same grant as above, with the node as one build wrote it: its bytes
    // still name a node, and the same one.
    let stored = b"\x01g\x00\x00\x01p\x0bworkspace:r\x03\x00\x01\x02\x02\x04\x00\x00\x01c";
    let read: Grant = postcard::from_bytes(stored).expect("the stored grant still loads");
    assert_eq!(read.node, TreePath::workspace("r"));
    assert_eq!(
        read,
        Grant::new(
            "g",
            Grantee::person("p"),
            TreePath::workspace("r"),
            KindSet::ALL,
            Answer::Allow,
            Grantee::person("c"),
        )
    );
}
