//! Comment records: the one kind of replicated app state a Commenter writes.
//!
//! An app document is otherwise checked only as a whole: its changes are
//! `Splice`-class and need `Write`. A comment cannot live inside such a
//! document, because nothing a receiver can check tells a comment edit from
//! any other edit. So a comment is its own record, and the record's id names
//! its only writer:
//!
//! ```text
//! comment:<author device key, 64 lowercase hex>:<local>@<parent record id>
//! ```
//!
//! - **Owner.** The device key in the id is the only author whose ops the
//!   record accepts. It is compared with the envelope's signed `author`, so
//!   every receiver checks it without resolving who the device belongs to.
//! - **Class.** Every change to a comment record is signed [`OpClass::Comment`],
//!   which the role table grants to Commenter and above. [`check_document_route`]
//!   refuses any other writable class on a comment record, and refuses
//!   `Comment` on every route except a comment record and the room route
//!   (below). A Commenter who labels an edit to someone else's record as a
//!   comment is refused because the record is not theirs.
//! - **Parent.** What the comment is about: a document, a channel, another
//!   comment. Others respond by writing their *own* comment records whose
//!   parent is this one. Nobody appends into a record they do not own.
//!
//! The room route (record id `""`) is where an app keeps its room index. A
//! Commenter makes their comment discoverable by adding an index entry that
//! names a comment record they own, signed as `Comment`. A broker cannot
//! check that shape (it holds no document), so the receiving SDK does: it
//! applies the change to a copy of its index and refuses it unless the only
//! difference is new entries naming comment records owned by the envelope's
//! author.

use crate::crypto::Ed25519PublicKey;
use crate::role::OpClass;

/// Prefix of every comment record id.
pub const COMMENT_RECORD_PREFIX: &str = "comment:";

/// Longest `local` part of a comment record id.
pub const MAX_COMMENT_LOCAL_BYTES: usize = 64;

/// Longest comment record id, matching the record-id ceiling apps enforce.
pub const MAX_COMMENT_RECORD_ID_BYTES: usize = 1024;

/// A parsed comment record id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommentRecord<'a> {
    /// The device key of the record's only writer.
    pub author: Ed25519PublicKey,
    /// Distinguishes the author's comment records on one parent.
    pub local: &'a str,
    /// The record this comment is about.
    pub parent: &'a str,
}

/// Build the comment record id owned by `author`. Returns `None` when `local`
/// is not 1–64 of `[A-Za-z0-9_-]`, `parent` is empty or holds a control
/// character, or the id would exceed [`MAX_COMMENT_RECORD_ID_BYTES`].
#[must_use]
pub fn comment_record_id(author: &Ed25519PublicKey, local: &str, parent: &str) -> Option<String> {
    let id = format!(
        "{COMMENT_RECORD_PREFIX}{}:{local}@{parent}",
        hex::encode(author.as_bytes())
    );
    parse_comment_record(&id).is_some().then_some(id)
}

/// Parse a comment record id. Only the canonical form parses: lowercase hex,
/// a valid `local`, a non-empty parent without control characters.
#[must_use]
pub fn parse_comment_record(record_id: &str) -> Option<CommentRecord<'_>> {
    if record_id.len() > MAX_COMMENT_RECORD_ID_BYTES {
        return None;
    }
    let rest = record_id.strip_prefix(COMMENT_RECORD_PREFIX)?;
    let (author_hex, rest) = rest.split_once(':')?;
    let (local, parent) = rest.split_once('@')?;
    if author_hex.len() != 64
        || !author_hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    if local.is_empty()
        || local.len() > MAX_COMMENT_LOCAL_BYTES
        || !local
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return None;
    }
    if parent.is_empty() || parent.chars().any(char::is_control) {
        return None;
    }
    let mut author = [0_u8; 32];
    hex::decode_to_slice(author_hex, &mut author).ok()?;
    Some(CommentRecord {
        author: Ed25519PublicKey::from_bytes(author),
        local,
        parent,
    })
}

/// Whether `record_id` names a comment record owned by `author`.
#[must_use]
pub fn is_comment_record_of(record_id: &str, author: &Ed25519PublicKey) -> bool {
    parse_comment_record(record_id).is_some_and(|record| record.author == *author)
}

/// The class an SDK signs an ordinary change to `record_id` with: `Comment`
/// on a comment record, `Splice` everywhere else. (A room-route change that
/// only adds the sender's own comment references is also `Comment`; the SDK
/// decides that from the change itself.)
#[must_use]
pub fn document_op_class_for_record(record_id: &str) -> OpClass {
    if parse_comment_record(record_id).is_some() {
        OpClass::Comment
    } else {
        OpClass::Splice
    }
}

/// Why a document op's class does not fit its route.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CommentRouteError {
    /// A comment record accepts writes from its owner only.
    #[error("only the device that owns a comment record may write it")]
    NotOwner,
    /// A comment record's changes are `Comment`-class; any other writable
    /// class on it is refused.
    #[error("a comment record accepts only comment-class changes")]
    WrongClass,
    /// `Comment` is accepted only on comment records and the room route.
    #[error("comment-class changes are accepted only on comment records and the room index")]
    NotACommentRoute,
}

/// The route half of the document rule; the role table is the other half, and
/// callers apply both. `author` is the envelope's signed device key.
///
/// - A comment record takes `Comment` from its owner, and `Redact` (which the
///   role table grants to the owner of the room only). Nothing else.
/// - The room route (`record_id == ""`) takes any class; a `Comment` there is
///   checked structurally by the receiving SDK.
/// - Every other route takes any class except `Comment`.
///
/// # Errors
///
/// Returns the [`CommentRouteError`] naming the rule the op breaks.
pub fn check_document_route(
    record_id: &str,
    op_class: OpClass,
    author: &Ed25519PublicKey,
) -> Result<(), CommentRouteError> {
    if let Some(record) = parse_comment_record(record_id) {
        return match op_class {
            OpClass::Comment if record.author == *author => Ok(()),
            OpClass::Comment => Err(CommentRouteError::NotOwner),
            OpClass::Redact => Ok(()),
            _ => Err(CommentRouteError::WrongClass),
        };
    }
    if op_class == OpClass::Comment && !record_id.is_empty() {
        return Err(CommentRouteError::NotACommentRoute);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::role::{ArtifactKind, Role, role_permits};

    fn key(byte: u8) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes([byte; 32])
    }

    /// The whole document rule as the signer and every broker apply it.
    fn permits(role: Role, record: &str, class: OpClass, author: &Ed25519PublicKey) -> bool {
        role_permits(role, ArtifactKind::Note, class)
            && check_document_route(record, class, author).is_ok()
    }

    #[test]
    fn id_round_trips_and_names_its_owner() {
        let id = comment_record_id(&key(7), "m-1", "uuid:1d7e").expect("valid id");
        let parsed = parse_comment_record(&id).expect("parses");
        assert_eq!(parsed.author, key(7));
        assert_eq!(parsed.local, "m-1");
        assert_eq!(parsed.parent, "uuid:1d7e");
        assert!(is_comment_record_of(&id, &key(7)));
        assert!(!is_comment_record_of(&id, &key(8)));
    }

    #[test]
    fn a_reply_names_a_comment_as_its_parent() {
        let report = comment_record_id(&key(1), "report", "uuid:feedback").unwrap();
        let reply = comment_record_id(&key(2), "reply", &report).unwrap();
        let parsed = parse_comment_record(&reply).unwrap();
        assert_eq!(parsed.author, key(2));
        assert_eq!(parsed.parent, report);
    }

    #[test]
    fn non_canonical_ids_do_not_parse() {
        let upper = format!("comment:{}:m@p", "AB".repeat(32));
        assert!(parse_comment_record(&upper).is_none());
        let short = format!("comment:{}:m@p", "ab".repeat(31));
        assert!(parse_comment_record(&short).is_none());
        let hex = "ab".repeat(32);
        assert!(parse_comment_record(&format!("comment:{hex}:@p")).is_none());
        assert!(parse_comment_record(&format!("comment:{hex}:a/b@p")).is_none());
        assert!(parse_comment_record(&format!("comment:{hex}:m@")).is_none());
        assert!(parse_comment_record(&format!("comment:{hex}:m@p\n")).is_none());
        assert!(comment_record_id(&key(1), &"x".repeat(65), "p").is_none());
    }

    #[test]
    fn a_commenter_writes_their_own_comment_record() {
        let me = key(3);
        let mine = comment_record_id(&me, "m", "uuid:c").unwrap();
        assert_eq!(document_op_class_for_record(&mine), OpClass::Comment);
        assert!(permits(Role::Commenter, &mine, OpClass::Comment, &me));
    }

    #[test]
    fn a_commenter_cannot_write_someone_elses_comment_record() {
        let theirs = comment_record_id(&key(4), "m", "uuid:c").unwrap();
        assert_eq!(
            check_document_route(&theirs, OpClass::Comment, &key(3)),
            Err(CommentRouteError::NotOwner)
        );
        assert!(!permits(
            Role::Commenter,
            &theirs,
            OpClass::Comment,
            &key(3)
        ));
        // An editor cannot rewrite it either: the record has one writer.
        assert_eq!(
            check_document_route(&theirs, OpClass::Splice, &key(5)),
            Err(CommentRouteError::WrongClass)
        );
    }

    #[test]
    fn a_splice_labelled_as_a_comment_is_refused_on_any_other_record() {
        let me = key(3);
        assert_eq!(
            check_document_route("uuid:9", OpClass::Comment, &me),
            Err(CommentRouteError::NotACommentRoute)
        );
        assert!(!permits(Role::Commenter, "uuid:9", OpClass::Comment, &me));
        // Honestly labelled, the same edit needs Write.
        assert!(!permits(Role::Commenter, "uuid:9", OpClass::Splice, &me));
        assert!(permits(Role::Editor, "uuid:9", OpClass::Splice, &me));
    }

    #[test]
    fn a_commenter_cannot_edit_the_package_document() {
        let me = key(3);
        assert_eq!(document_op_class_for_record(""), OpClass::Splice);
        assert!(!permits(Role::Commenter, "", OpClass::Splice, &me));
        // A comment-class change is admitted to the room route by class only;
        // the receiving SDK checks that it adds nothing but the sender's own
        // comment references.
        assert!(permits(Role::Commenter, "", OpClass::Comment, &me));
    }

    #[test]
    fn viewers_write_nothing_and_owners_may_redact_comments() {
        let me = key(3);
        let mine = comment_record_id(&me, "m", "uuid:c").unwrap();
        assert!(!permits(Role::Viewer, &mine, OpClass::Comment, &me));
        assert!(!permits(Role::Denied, &mine, OpClass::Comment, &me));
        assert!(permits(Role::Owner, &mine, OpClass::Redact, &key(9)));
        assert!(!permits(Role::Editor, &mine, OpClass::Redact, &key(9)));
    }
}
