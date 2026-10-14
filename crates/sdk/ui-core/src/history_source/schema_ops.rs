//! Stable schema-operation projection for app metadata indexes.
//!
//! Apps describe which schema objects belong to a resource; the toolkit owns
//! Automerge wire decoding and change timestamps.
use automerge::legacy::{Key, ObjectId, OpId};
use automerge::{AutoCommit, Automerge, ChangeHash};
use std::collections::HashSet;
use tilo_resource::profiling::measure;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SchemaObjectId(String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaHistoryOp {
    pub object: SchemaObjectId,
    pub new_object: Option<SchemaObjectId>,
    pub key: Option<String>,
    pub timestamp: Option<i64>,
}

/// Decode committed operations after `heads` without mutating the document.
pub fn schema_operations_since(doc: &AutoCommit, heads: &[ChangeHash]) -> Vec<SchemaHistoryOp> {
    let mut operations = Vec::new();
    for change in doc.committed_document().get_changes(heads) {
        let decoded = change.decode();
        let timestamp = (decoded.time > 0).then_some(decoded.time);
        for (offset, operation) in decoded.operations.iter().enumerate() {
            let new_object = operation.obj_type().is_some().then(|| {
                SchemaObjectId::from(ObjectId::Id(OpId(
                    decoded.start_op.get().saturating_add(offset as u64),
                    decoded.actor_id.clone(),
                )))
            });
            operations.push(SchemaHistoryOp {
                object: SchemaObjectId::from(operation.obj.clone()),
                new_object,
                key: match &operation.key {
                    Key::Map(key) => Some(key.to_string()),
                    Key::Seq(_) => None,
                },
                timestamp,
            });
        }
    }
    operations
}

/// One committed change as an index of what changed when reads it: its
/// time, the objects it made and every object it wrote into, each once.
///
/// Typing a word is one operation per character on one text object; this
/// carries that object once, not one entry per character.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaChange {
    pub timestamp: Option<i64>,
    /// The objects the change made, in the order it made them.
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
    use automerge::{ObjType, ROOT};

    /// The per-change read carries what the per-operation read does: every
    /// object an operation was in, every object one made with its parent and
    /// key, and the change's time, but each written object once.
    #[test]
    fn changes_carry_what_operations_do_once_per_object() {
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

        let operations = schema_operations_since(&doc, &[]);
        let changes = schema_changes_since(doc.committed_document(), &[]);
        let times: Vec<_> = changes.iter().map(|change| change.timestamp).collect();
        assert_eq!(times, [Some(100), Some(200), None]);
        let made: Vec<_> = operations
            .iter()
            .filter_map(|op| {
                Some(SchemaCreation {
                    parent: op.object.clone(),
                    object: op.new_object.clone()?,
                    key: op.key.clone(),
                })
            })
            .collect();
        let created: Vec<_> = changes
            .iter()
            .flat_map(|change| change.created.clone())
            .collect();
        assert_eq!(created, made);
        let touched: Vec<_> = changes
            .iter()
            .flat_map(|change| change.touched.clone())
            .collect();
        for op in &operations {
            assert!(touched.contains(&op.object), "{:?} is carried", op.object);
        }
        assert_eq!(
            changes[1].touched.len(),
            2,
            "typing and deleting in a body and titling its page carry two objects: {:?}",
            changes[1]
        );
        assert!(
            operations.len() > 30,
            "the per-operation read has one entry per character"
        );
        let later = schema_changes_since(doc.committed_document(), &doc.committed_heads());
        assert!(later.is_empty(), "nothing after the heads");
    }
}
