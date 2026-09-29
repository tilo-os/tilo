//! A room's record index, as every holder of a room checks it.
//!
//! A room's index (`tilo:records`) names the records the room holds. Its map
//! is made by one change every peer makes byte for byte the same
//! ([`index_genesis`]), and a Commenter may change it in exactly one way:
//! adding entries for comment records they own. The toolkit that edits the
//! index, the broker that forwards changes without holding it and the room
//! core that admits them all check that one rule here.
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")
)]

use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, transaction::Transactable as _};

/// The key of a room's index map in the room route's document.
pub const INDEX: &str = "tilo:records";
/// Upper bound on a record kind's length, in bytes.
pub const MAX_RECORD_KIND_BYTES: usize = 64;

/// Whether `value` is a label: an ASCII letter, then letters, digits, `.`,
/// `_` or `-`, at most `max` bytes.
#[must_use]
pub fn valid_label(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value.bytes().enumerate().all(|(i, byte)| {
            (byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
                && (i != 0 || byte.is_ascii_alphabetic())
        })
}

/// Whether `kind` is a valid record kind.
#[must_use]
pub fn valid_kind(kind: &str) -> bool {
    valid_label(kind, MAX_RECORD_KIND_BYTES)
}

/// The room's index map, when the document holds exactly one.
#[must_use]
pub fn index_ro(doc: &AutoCommit) -> Option<ObjId> {
    let mut values = doc.get_all(&automerge::ROOT, INDEX).ok()?;
    if values.len() != 1 {
        return None;
    }
    match values.pop() {
        Some((automerge::Value::Object(ObjType::Map), id)) => Some(id),
        _ => None,
    }
}

/// The actor that makes every room's index map. Never a device's: no
/// one writes as it, it only ever makes the one change in
/// [`index_genesis`].
const INDEX_GENESIS_ACTOR: [u8; 16] = *b"tilo-room-index\0";

/// The change that makes a room's index map — byte for byte the same on
/// every peer, so it hashes the same. Two members who each bring the
/// first record into an empty room both apply this one change and end up
/// holding one index; had each made its own map under the same key, one
/// map would win and the other's entries would be hidden behind it.
pub fn index_genesis() -> Vec<automerge::Change> {
    thread_local! {
        static GENESIS: Vec<automerge::Change> = {
            let mut genesis = AutoCommit::new()
                .with_actor(automerge::ActorId::from(INDEX_GENESIS_ACTOR.as_slice()));
            let made = genesis.put_object(&automerge::ROOT, INDEX, ObjType::Map);
            debug_assert!(made.is_ok(), "an empty document takes a root map");
            genesis.commit_with(automerge::transaction::CommitOptions::default().with_time(0));
            genesis.get_changes(&[])
        };
    }
    GENESIS.with(Clone::clone)
}

/// Whether `change` is [`index_genesis`], which every room shares and
/// anyone may bring: it holds nothing but the empty index.
pub fn is_index_genesis(change: &automerge::Change) -> bool {
    index_genesis()
        .iter()
        .any(|genesis| genesis.hash() == change.hash())
}

pub fn made_by_genesis(index: &ObjId) -> bool {
    matches!(index, ObjId::Id(_, actor, _) if actor.to_bytes() == INDEX_GENESIS_ACTOR.as_slice())
}

/// Whether the incremental change `delta`, applied to `index`, does
/// nothing but add index entries for comment records `author` owns —
/// the only room-index change a `Comment`-class envelope may carry.
///
/// The check reads the change's operations, not the resulting state, so
/// an operation hidden by a concurrent conflict cannot slip through. An
/// allowed change makes an entry map under the existing index keyed by
/// the author's comment record, and puts a string `kind` in a map that
/// the same change made. Anything else — another key, another object, a
/// delete, a change whose dependencies are missing — is refused. `index`
/// is left untouched.
pub fn comment_references_only(index: &AutoCommit, delta: &[u8], author: &[u8; 32]) -> bool {
    let mut after = index.clone();
    let before_heads = after.get_heads();
    let missing_before = after.get_missing_deps(&[]).len();
    if tilo_automerge::ApplyIncoming::load_incoming(&mut after, delta).is_err()
        || after.get_missing_deps(&[]).len() != missing_before
    {
        return false;
    }
    // A change may bring the index only as the shared genesis: any
    // other creation would sit beside the room's and hide one of them.
    let Some(index_obj) = index_ro(&after) else {
        return false;
    };
    if index_ro(index).is_none() && !made_by_genesis(&index_obj) {
        return false;
    }
    changes_are_comment_references(&index_obj, &after.get_changes(&before_heads), author)
}

/// Whether this document's changes after `since` do nothing but add index
/// entries for comment records `author` owns — how an SDK decides to sign
/// its own pending room-index delta `Comment` rather than `Splice`.
pub fn local_changes_are_comment_references(
    index: &mut AutoCommit,
    since: &[automerge::ChangeHash],
    author: &[u8; 32],
) -> bool {
    let Some(index_obj) = index_ro(index) else {
        return false;
    };
    let changes = index.get_changes(since);
    changes_are_comment_references(&index_obj, &changes, author)
}

/// Whether the incremental change `delta` does nothing but add entries
/// for comment records `author` owns to the room index every room shares
/// — decided from the change alone, so a broker that holds no index can
/// refuse a `Comment`-class index change before it goes further. The
/// holder of the index still checks it against its state
/// ([`comment_references_only`]).
pub fn comment_references_delta(delta: &[u8], author: &[u8; 32]) -> bool {
    let Some(changes) = tilo_automerge::coverage::changes_in(delta) else {
        return false;
    };
    let mut shared = AutoCommit::new();
    if tilo_automerge::ApplyIncoming::apply_incoming(&mut shared, index_genesis()).is_err() {
        return false;
    }
    let Some(index_obj) = index_ro(&shared) else {
        return false;
    };
    changes_are_comment_references(&index_obj, &changes, author)
}

fn changes_are_comment_references(
    index_obj: &ObjId,
    changes: &[automerge::Change],
    author: &[u8; 32],
) -> bool {
    use automerge::legacy::{Key, ObjectId, OpType};

    let ObjId::Id(index_counter, index_actor, _) = index_obj else {
        return false;
    };
    let author = collab_types::Ed25519PublicKey::from_bytes(*author);
    if changes.is_empty() {
        return false;
    }
    for change in changes {
        if is_index_genesis(change) {
            continue;
        }
        let change = change.decode();
        let start = change.start_op.get();
        let mut made_entries: Vec<u64> = Vec::new();
        for (offset, op) in change.operations.iter().enumerate() {
            let Some(counter) = u64::try_from(offset)
                .ok()
                .and_then(|offset| start.checked_add(offset))
            else {
                return false;
            };
            if op.insert {
                return false;
            }
            let allowed = match (&op.obj, &op.key, &op.action) {
                (ObjectId::Id(obj), Key::Map(key), OpType::Make(ObjType::Map))
                    if obj.counter() == *index_counter
                        && obj.actor() == index_actor
                        && collab_types::is_comment_record_of(key.as_str(), &author) =>
                {
                    made_entries.push(counter);
                    true
                }
                (
                    ObjectId::Id(obj),
                    Key::Map(key),
                    OpType::Put(automerge::ScalarValue::Str(kind)),
                ) => {
                    key.as_str() == "kind"
                        && *obj.actor() == change.actor_id
                        && made_entries.contains(&obj.counter())
                        && valid_kind(kind.as_ref())
                }
                _ => false,
            };
            if !allowed {
                return false;
            }
        }
    }
    true
}
