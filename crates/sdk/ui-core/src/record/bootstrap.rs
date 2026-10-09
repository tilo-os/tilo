//! Idempotent construction of initially empty record slots.
//!
//! Independent cold replicas share the record genesis, but a normal
//! `put_object` gives each replica a different map or Text. Construct a slot
//! in a separate, immutable change based on its parent's creation instead.
//! This never changes the original genesis or replaces an existing object.

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ChangeHash, ObjId, ObjType, ReadDoc, Value};
use sha2::{Digest, Sha256};
use tilo_automerge::ApplyIncoming;

fn creation(doc: &mut AutoCommit, parent: &ObjId) -> Option<ChangeHash> {
    let genesis = super::genesis_actor(doc)?;
    doc.get_changes(&[]).into_iter().find_map(|change| {
        let creates_parent = match parent {
            ObjId::Root => change.actor_id() == &genesis && change.seq() == 1,
            ObjId::Id(op, actor, _) => {
                change.actor_id() == actor
                    && change.start_op().get() <= *op
                    && change.max_op() >= *op
            }
        };
        creates_parent.then(|| change.hash())
    })
}

fn actor(head: ChangeHash, domain: &[u8], address: &[u8]) -> ActorId {
    let digest = Sha256::new()
        .chain_update(b"tilo.record.slot.v1\0")
        .chain_update(head.0)
        .chain_update(domain)
        .chain_update(address)
        .finalize();
    ActorId::from(digest.as_slice())
}

/// Construct a missing map or Text with one identity on every cold replica.
/// Existing state is never replaced; a deleted canonical slot is never
/// resurrected. Only identified records can use deterministic slots.
pub fn object(doc: &mut AutoCommit, parent: &ObjId, key: &str, kind: ObjType) -> Option<ObjId> {
    if let Some((value, object)) = doc.get(parent, key).ok()? {
        return (value == Value::Object(kind)).then_some(object);
    }
    let domain: &[u8] = match kind {
        ObjType::Map => b"map\0",
        ObjType::Text => b"text\0",
        _ => return None,
    };
    // Reading the change graph closes the open transaction.
    close_pending(doc);
    let head = creation(doc, parent)?;
    let address = format!("{parent}\0{key}");
    let actor = actor(head, domain, address.as_bytes());
    // A prior construction followed by deletion is a tombstone, not a
    // request to resurrect data during an offline reopen.
    if doc
        .get_changes(&[])
        .iter()
        .any(|change| change.actor_id() == &actor)
    {
        return None;
    }
    let mut made = doc.fork_at(&[head]).ok()?.with_actor(actor);
    // A legacy parent may have created this slot in the same change.
    if made.get(parent, key).ok()?.is_some() {
        return None;
    }
    let object = made.put_object(parent, key, kind).ok()?;
    made.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Structure.change_message())
            .with_time(0),
    );
    doc.merge_incoming(&mut made).ok()?;
    Some(object)
}

/// Reading the change graph and merging close the open transaction. What it
/// holds was written directly and names no finer action than an edit of the
/// whole record, so it is committed as one before the slot joins.
fn close_pending(doc: &mut AutoCommit) {
    if doc.pending_ops() != 0 {
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Native.change_message()),
        );
    }
}

/// Seed a newly constructed Text once for this exact initial byte sequence.
/// Repeated imports of the same file/base produce the same change instead
/// of inserting the base twice. Call only when the buffer did not exist;
/// subsequent updates belong to the record's ordinary private writer.
pub fn seed_text(doc: &mut AutoCommit, object: &ObjId, seed: &str) -> Option<()> {
    if seed.is_empty() {
        return Some(());
    }
    close_pending(doc);
    let head = creation(doc, object)?;
    let digest = Sha256::digest(seed.as_bytes());
    let actor = actor(head, b"initial-text\0", digest.as_slice());
    if doc
        .get_changes(&[])
        .iter()
        .any(|change| change.actor_id() == &actor)
    {
        return Some(());
    }
    let mut made = doc.fork_at(&[head]).ok()?.with_actor(actor);
    if !made.text(object).ok()?.is_empty() {
        return None;
    }
    made.splice_text(object, 0, 0, seed).ok()?;
    made.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Structure.change_message())
            .with_time(0),
    );
    doc.merge_incoming(&mut made).ok()?;
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    fn fresh(room: &str, writer: &[u8]) -> Result<AutoCommit> {
        let mut doc = AutoCommit::new().with_actor(ActorId::from(writer));
        let checkout = crate::record::RecordCheckout::new(
            crate::record::RecordId::for_tab(73),
            crate::record::RecordKind::new("test.document"),
        );
        super::super::genesis(&mut doc, &checkout, room, |_| Ok(()))?;
        Ok(doc)
    }

    #[test]
    fn cold_slots_preserve_genesis_private_writers_and_room_isolation() -> Result<()> {
        let mut a = fresh("room-a", b"writer-a")?;
        let genesis = a.get_heads();
        let writer = a.get_actor().clone();
        let mut b = fresh("room-a", b"writer-b")?;
        let mut other_room = fresh("room-b", b"writer-c")?;
        let left = object(&mut a, &ObjId::Root, "files", ObjType::Map).ok_or("left map")?;
        let right = object(&mut b, &ObjId::Root, "files", ObjType::Map).ok_or("right map")?;
        assert_eq!(left, right);
        assert_ne!(
            left,
            object(&mut other_room, &ObjId::Root, "files", ObjType::Map).ok_or("other room")?
        );
        assert_eq!(a.get_actor(), &writer);
        assert_eq!(a.fork_at(&genesis)?.get_heads(), genesis);
        assert!(a.fork_at(&genesis)?.get(ObjId::Root, "files")?.is_none());
        a.put(&left, "left", "from A")?;
        b.put(&right, "right", "from B")?;
        a.merge(&mut b)?;
        assert_eq!(
            a.get(&left, "left")?.ok_or("left edit")?.0.as_str(),
            Some("from A")
        );
        assert_eq!(
            a.get(&left, "right")?.ok_or("right edit")?.0.as_str(),
            Some("from B")
        );
        Ok(())
    }

    #[test]
    fn existing_legacy_values_and_deleted_slots_are_never_replaced() -> Result<()> {
        let mut doc = fresh("room-a", b"writer-a")?;
        let legacy = doc.put_object(ObjId::Root, "files", ObjType::Map)?;
        doc.put(&legacy, "durable", "keep me")?;
        let heads = doc.get_heads();
        assert_eq!(
            object(&mut doc, &ObjId::Root, "files", ObjType::Map),
            Some(legacy.clone())
        );
        assert_eq!(doc.get_heads(), heads);
        assert!(object(&mut doc, &ObjId::Root, "files", ObjType::Text).is_none());
        let text = object(&mut doc, &legacy, "text", ObjType::Text).ok_or("text")?;
        seed_text(&mut doc, &text, "base").ok_or("seed")?;
        seed_text(&mut doc, &text, "base").ok_or("repeat seed")?;
        assert_eq!(doc.text(&text)?, "base");
        doc.delete(&legacy, "text")?;
        assert!(object(&mut doc, &legacy, "text", ObjType::Text).is_none());
        assert!(doc.get(&legacy, "text")?.is_none());
        assert_eq!(
            doc.get(&legacy, "durable")?
                .ok_or("durable value")?
                .0
                .as_str(),
            Some("keep me")
        );
        let mut local = AutoCommit::new();
        assert!(object(&mut local, &ObjId::Root, "files", ObjType::Map).is_none());
        assert!(local.get(ObjId::Root, "files")?.is_none());
        Ok(())
    }
}
