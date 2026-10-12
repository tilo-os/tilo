//! Stable schema-operation projection for app metadata indexes.
//!
//! Apps describe which schema objects belong to a resource; the toolkit owns
//! Automerge wire decoding and change timestamps.
use automerge::legacy::{Key, ObjectId, OpId};
use automerge::{AutoCommit, ChangeHash};

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
