//! A member's own record in a workspace: one record of a kind per member
//! (what they are writing and have not sent, their browsing history, their
//! signature), kept in the workspace beside its other records and synced
//! with it to that member's devices, and to nobody else's.
//!
//! Its id names the member, `member:<root>:<kind>`, with `<root>` the
//! lowercase hex of their user root key, so every door decides it alike
//! without a grant: only that person reads or changes it, whatever their
//! role, and no other member does, the room's owner included
//! ([`crate::Party::decide`]).

use crate::{Level, Segment, TreePath};

const PREFIX: &str = "member:";

/// The id of `person`'s own record of `kind`; `person` is the lowercase hex
/// of their user root key.
#[must_use]
pub fn member_record_id(person: &str, kind: &str) -> String {
    format!("{PREFIX}{person}:{kind}")
}

/// The person whose own record `record` is, and its kind; `None` for any
/// other record.
#[must_use]
pub fn member_record(record: &str) -> Option<(&str, &str)> {
    let (person, kind) = record.strip_prefix(PREFIX)?.split_once(':')?;
    let root = person.len() == 64
        && person
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    let label = kind.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && kind.len() <= 64
        && kind
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    (root && label).then_some((person, kind))
}

/// The person whose own record `node` is in, if it is in one.
pub(crate) fn owner_of(node: &TreePath) -> Option<&str> {
    node.segments().iter().find_map(|segment| match segment {
        Segment::Name(Level::Resource, record) => member_record(record).map(|(person, _)| person),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_member_record_names_its_person_and_kind() {
        let person = "ab".repeat(32);
        let id = member_record_id(&person, "chat.drafts");
        assert_eq!(member_record(&id), Some((person.as_str(), "chat.drafts")));
        for other in [
            "derived:chat.drafts@tab:0000000000000000".to_owned(),
            format!("member:{}:chat.drafts", "AB".repeat(32)),
            format!("member:{}:chat.drafts", "ab".repeat(31)),
            format!("member:{person}:"),
            format!("member:{person}:1kind"),
            format!("member:{person}:a@b"),
        ] {
            assert_eq!(member_record(&other), None, "{other}");
        }
        let node = TreePath::room("r").app("a").resource(id).item("x");
        assert_eq!(owner_of(&node), Some(person.as_str()));
        assert_eq!(
            owner_of(
                &TreePath::room("r")
                    .app("a")
                    .resource("tab:0000000000000000")
            ),
            None
        );
    }
}
