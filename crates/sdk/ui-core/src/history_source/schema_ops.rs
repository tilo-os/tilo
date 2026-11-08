//! Stable schema-operation projection for app metadata indexes.
//!
//! Apps describe which schema objects belong to a resource; the toolkit owns
//! Automerge wire decoding and change timestamps.
use automerge::legacy::{Key, ObjectId, OpId};
use automerge::{Automerge, ChangeHash};
use std::collections::HashSet;
use tilo_resource::profiling::measure;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SchemaObjectId(String);

/// One committed change as an index of what changed when reads it: its
/// time, the objects it made and every object it wrote into, each once.
///
/// Typing a word is one operation per character on one text object; this
/// carries that object once, not one entry per character.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaChange {
    pub timestamp: Option<i64>,
    pub created: Vec<SchemaCreation>,
    /// Every object the change wrote into (set, inserted into, deleted
    /// from, or made an object in), each once.
    pub touched: Vec<SchemaObjectId>,
}

/// An object a change made: in which object, and under which map key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaCreation {
    pub parent: SchemaObjectId,
    pub object: SchemaObjectId,
    /// The map key it was made under; none in a list or text.
    pub key: Option<String>,
}

/// The committed changes of `doc` after `heads`, oldest first, each read as a
/// [`SchemaChange`]. Takes the document itself so a copy can be read off the
/// UI thread.
pub fn schema_changes_since(doc: &Automerge, heads: &[ChangeHash]) -> Vec<SchemaChange> {
    let changes = measure("schema_changes.collect", || doc.get_changes(heads));
    measure("schema_changes.read", || {
        changes
            .iter()
            .map(|change| read_change(&change.decode()))
            .collect()
    })
}

fn read_change(change: &automerge::ExpandedChange) -> SchemaChange {
    let mut touched = Vec::new();
    let mut created = Vec::new();
    let mut seen: HashSet<&ObjectId> = HashSet::new();
    let mut last: Option<&ObjectId> = None;
    for (offset, operation) in change.operations.iter().enumerate() {
        // A run of operations on one object (the characters of a typed
        // word) is looked up once.
        if last != Some(&operation.obj) {
            last = Some(&operation.obj);
            if seen.insert(&operation.obj) {
                touched.push(SchemaObjectId::from(operation.obj.clone()));
            }
        }
        if operation.obj_type().is_some() {
            created.push(SchemaCreation {
                parent: SchemaObjectId::from(operation.obj.clone()),
                object: SchemaObjectId::from(ObjectId::Id(OpId(
                    change.start_op.get().saturating_add(offset as u64),
                    change.actor_id.clone(),
                ))),
                key: match &operation.key {
                    Key::Map(key) => Some(key.to_string()),
                    Key::Seq(_) => None,
                },
            });
        }
    }
    SchemaChange {
        timestamp: (change.time > 0).then_some(change.time),
        created,
        touched,
    }
}

impl SchemaObjectId {
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0 == "root"
    }
}

impl From<ObjectId> for SchemaObjectId {
    fn from(value: ObjectId) -> Self {
        Self(match value {
            ObjectId::Root => "root".into(),
            ObjectId::Id(OpId(counter, actor)) => format!("{counter}@{actor}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::{CommitOptions, Transactable};
    use automerge::{AutoCommit, ObjType, ROOT};

    /// A change carries every object it wrote into once (one entry per object, not per
    /// operation), the objects it made with their parent and key, and its time.
    #[test]
    fn a_change_carries_each_written_object_once_and_what_it_made() {
        let mut doc = AutoCommit::new();
        let pages = doc.put_object(ROOT, "pages", ObjType::Map).unwrap();
        let page = doc.put_object(&pages, "first", ObjType::Map).unwrap();
        let body = doc.put_object(&page, "body", ObjType::Text).unwrap();
        doc.commit_with(CommitOptions::default().with_time(100));
        doc.splice_text(&body, 0, 0, "typed one character at a time")
            .unwrap();
        doc.put(&page, "title", "First").unwrap();
        doc.splice_text(&body, 0, 5, "").unwrap();
        doc.commit_with(CommitOptions::default().with_time(200));
        let untimed = doc.put_object(&pages, "legacy", ObjType::Map).unwrap();
        doc.put(&untimed, "title", "Legacy").unwrap();
        doc.commit_with(CommitOptions::default().with_time(0));

        let changes = schema_changes_since(doc.committed_document(), &[]);
        let times: Vec<_> = changes.iter().map(|change| change.timestamp).collect();
        assert_eq!(times, [Some(100), Some(200), None]);
        let made = &changes[0].created;
        let keys: Vec<_> = made.iter().map(|made| made.key.as_deref()).collect();
        assert_eq!(keys, [Some("pages"), Some("first"), Some("body")]);
        assert!(made[0].parent.is_root());
        assert_eq!(made[1].parent, made[0].object);
        assert_eq!(made[2].parent, made[1].object);
        assert_eq!(
            changes[0].touched,
            [
                SchemaObjectId::from(automerge::legacy::ObjectId::Root),
                made[0].object.clone(),
                made[1].object.clone()
            ],
            "the objects it made objects in, each once"
        );
        assert!(
            changes[1].created.is_empty(),
            "typing, titling and deleting make no object"
        );
        assert_eq!(
            changes[1].touched,
            [made[2].object.clone(), made[1].object.clone()],
            "typing and deleting in a body and titling its page carry two objects, not one per character"
        );
        let legacy = &changes[2].created[0];
        assert_eq!(legacy.parent, made[0].object);
        assert_eq!(legacy.key.as_deref(), Some("legacy"));
        assert_eq!(
            changes[2].touched,
            [made[0].object.clone(), legacy.object.clone()]
        );
        let later = schema_changes_since(doc.committed_document(), &doc.committed_heads());
        assert!(later.is_empty(), "nothing after the heads");
    }
}
