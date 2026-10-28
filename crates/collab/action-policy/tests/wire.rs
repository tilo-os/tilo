//! Postcard names an enum variant by its position, and rooms keep their
//! grants, and the digests of their administration requests, in those
//! bytes. A variant removed or moved changes bytes a room already holds and
//! fails here. A new variant goes at the end of its enum; the `match`es
//! without a wildcard ask for its bytes.

use tilo_action_policy::{ActionKind, Answer, Grant, KindSet, Lifetime, Role, Subject, TreePath};

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
            "{value:?} is written as rooms hold it"
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
fn a_subject_keeps_its_bytes() {
    let cases: &[(Subject, &[u8])] = &[
        (Subject::Person("p".into()), &[0x00, 0x01, b'p']),
        (Subject::Role(Role::Editor), &[0x01, 0x02]),
        (Subject::Agent("a".into()), &[0x02, 0x01, b'a']),
        (Subject::Task("t".into()), &[0x03, 0x01, b't']),
        (Subject::Shortcut("s".into()), &[0x04, 0x01, b's']),
        (Subject::App("x".into()), &[0x05, 0x01, b'x']),
        (Subject::Bot("b".into()), &[0x06, 0x01, b'b']),
        (Subject::ForeignDevice, &[0x07]),
    ];
    for (subject, golden) in cases {
        match subject {
            Subject::Person(_)
            | Subject::Role(_)
            | Subject::Agent(_)
            | Subject::Task(_)
            | Subject::Shortcut(_)
            | Subject::App(_)
            | Subject::Bot(_)
            | Subject::ForeignDevice => stored_as!(subject, golden),
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
        Subject::Person("p".into()),
        TreePath::room("r"),
        KindSet::ALL,
        Answer::Allow,
        Subject::Person("c".into()),
    );
    let mut golden = vec![0x01, b'g'];
    golden.extend([0x00, 0x01, b'p']);
    golden.push(0x06);
    golden.extend(b"room:r");
    golden.extend([0x03, 0x00, 0x01, 0x02]);
    golden.push(0x02);
    golden.push(0x04);
    golden.extend([0x00, 0x01, b'c']);
    stored_as!(&grant, &golden);
}
