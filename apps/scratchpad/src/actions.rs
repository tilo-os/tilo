//! Replayable note edits shared by the live app and headless drivers.

use automerge::{ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
use serde::{Deserialize, Serialize};
use tilo_actions::{Action, ActionError, ActionRegistry, Doc, ExecutionLocus};

pub(crate) const EDIT: &str = "so.tilo.scratchpad.edit";

/// One text-bearing value stored under the note's `body` key.
pub(crate) enum BodyValue {
    Text(ObjId),
    Legacy(String),
}

impl BodyValue {
    fn text(&self, doc: &impl ReadDoc) -> String {
        match self {
            Self::Text(id) => doc.text(id).unwrap_or_default(),
            Self::Legacy(text) => text.clone(),
        }
    }
}

/// Every text-bearing value under `body` in Automerge's deterministic
/// conflict order (identical on every replica), with the index of the one
/// `doc.get` — and so every plain reader, the editor included — shows.
///
/// More than one value means replicas each created the body concurrently:
/// before [`crate::init_document`] gave every note its `Text` up front, the
/// first edit created it, so two replicas' first edits (or two upgrades of a
/// legacy string body) conflicted and a plain read hid one of them.
pub(crate) fn body_values(doc: &impl ReadDoc) -> (Vec<BodyValue>, Option<usize>) {
    let values: Vec<(BodyValue, ObjId)> = doc
        .get_all(ROOT, "body")
        .map(|all| {
            all.into_iter()
                .filter_map(|(value, id)| match value {
                    Value::Object(ObjType::Text) => Some((BodyValue::Text(id.clone()), id)),
                    Value::Scalar(scalar) => match scalar.as_ref() {
                        ScalarValue::Str(text) => Some((BodyValue::Legacy(text.to_string()), id)),
                        _ => None,
                    },
                    Value::Object(_) => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let winner = doc
        .get(ROOT, "body")
        .ok()
        .flatten()
        .and_then(|(_, id)| values.iter().position(|(_, value_id)| *value_id == id));
    (values.into_iter().map(|(value, _)| value).collect(), winner)
}

/// The note as shown: every value under `body` joined in conflict order, so
/// a concurrently created body is never hidden.
pub(crate) fn joined_body(doc: &impl ReadDoc) -> String {
    body_values(doc)
        .0
        .iter()
        .map(|value| value.text(doc))
        .collect()
}

/// Edit the note through a diff, preserving unchanged collaborative text.
#[derive(Debug, Serialize, Deserialize)]
pub struct EditNote {
    pub text: String,
}

impl Action for EditNote {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let (values, winner) = body_values(doc);
        if let [BodyValue::Text(text)] = values.as_slice() {
            doc.update_text(text, &self.text)?;
            return Ok(());
        }
        // No body (a document that bypassed `init_document`), a legacy string,
        // or conflicting values: write one canonical `Text`. The editor shows
        // only the winning value, so `self.text` replaces that value alone and
        // every other value keeps its place in the joined projection.
        let mut replacement = String::new();
        for (index, value) in values.iter().enumerate() {
            if Some(index) == winner {
                replacement.push_str(&self.text);
            } else {
                replacement.push_str(&value.text(doc));
            }
        }
        if winner.is_none() {
            replacement.push_str(&self.text);
        }
        let text = doc.put_object(ROOT, "body", ObjType::Text)?;
        doc.splice_text(&text, 0, 0, &replacement)?;
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Install every document action exported by Scratchpad's resources.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<EditNote>(EDIT);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test assertions")]
mod tests {
    use super::*;
    use automerge::AutoCommit;

    fn edit(doc: &mut AutoCommit, text: &str) {
        EditNote { text: text.into() }.apply(doc).unwrap();
        doc.commit();
    }

    fn both_survive(base: &mut AutoCommit, left_text: &str, right_text: &str) -> String {
        let mut left = base.fork();
        let mut right = base.fork();
        edit(&mut left, left_text);
        edit(&mut right, right_text);
        left.merge(&mut right).unwrap();
        crate::body(&left)
    }

    /// A note document that never got a `body` (created outside `open`, or
    /// before the body came with the document): each replica's first edit
    /// creates it, and the merge must not hide either one.
    #[test]
    fn concurrent_first_edits_without_a_body_both_survive() {
        for _ in 0..16 {
            let body = both_survive(&mut AutoCommit::new(), "left", "right");
            assert!(
                body.contains("left") && body.contains("right"),
                "both first edits survive; got {body:?}"
            );
        }
    }

    /// A legacy plain-string body is a register, not a sequence CRDT: two
    /// replicas that each upgrade it on their first edit must both survive.
    #[test]
    fn concurrent_first_edits_of_a_legacy_string_body_both_survive() {
        for _ in 0..16 {
            let mut base = AutoCommit::new();
            base.put(ROOT, "body", "hello").unwrap();
            base.commit();
            assert_eq!(crate::body(&base), "hello");
            let body = both_survive(&mut base, "hello left", "hello right");
            assert!(
                body.contains("left") && body.contains("right"),
                "both upgrades survive; got {body:?}"
            );
        }
    }
}
