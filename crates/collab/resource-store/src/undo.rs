//! Taking back one invocation's edits of a content, and no one else's: the characters it
//! inserted into a text go, the ones it deleted come back where they stood, and a counter loses
//! what it added. Any other part of a content holds what it held before the invocation.

use std::collections::BTreeSet;

use automerge::transaction::{Transactable as _, Transaction};
use automerge::{
    ActorId, Automerge, ChangeHash, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value,
};

use crate::admit::Stamp;

/// The stretch of a document's history one invocation wrote.
pub(crate) struct Span {
    /// The heads before its first change.
    pub before: Vec<ChangeHash>,
    /// The heads its last change made.
    pub after: Vec<ChangeHash>,
    /// Each of its changes' operations: the actor, and the first and last counter.
    ops: Vec<(ActorId, u64, u64)>,
}

impl Span {
    /// The stretch `invocation` wrote in `doc`; `None` for a document it did not change.
    pub(crate) fn of(doc: &Automerge, invocation: &str) -> Option<Self> {
        let mut before = None;
        let mut last = None;
        let mut ops = Vec::new();
        for change in doc.get_changes_meta(&[]) {
            let stamp = change
                .message
                .as_deref()
                .and_then(|message| serde_json::from_str::<Stamp>(message).ok());
            if stamp.is_some_and(|stamp| stamp.invocation == invocation) {
                before.get_or_insert(change.deps);
                last = Some(change.hash);
                ops.push((change.actor.into_owned(), change.start_op, change.max_op));
            }
        }
        Some(Self {
            before: before?,
            after: vec![last?],
            ops,
        })
    }

    /// Whether the operation `id` is one of the invocation's.
    fn made(&self, id: &ObjId) -> bool {
        let ObjId::Id(counter, actor, _) = id else {
            return false;
        };
        self.ops
            .iter()
            .any(|(by, first, last)| by == actor && (*first..=*last).contains(counter))
    }
}

/// Takes back what the invocation `span` did to the key `key` at the root of a content: `was`
/// is the content before it, `became` after it, and `root` the content now.
pub(crate) fn undo_key(
    span: &Span,
    (was, became): (&Automerge, &Automerge),
    key: &str,
    tx: &mut Transaction<'_>,
    root: &ObjId,
) -> Result<(), automerge::AutomergeError> {
    let held = (
        was.get(ROOT, key)?,
        became.get(ROOT, key)?,
        tx.get(root, key)?,
    );
    match held {
        // The same text throughout: character by character.
        (
            Some((Value::Object(ObjType::Text), before)),
            Some((Value::Object(ObjType::Text), after)),
            Some((Value::Object(ObjType::Text), now)),
        ) if before == after && after == now => undo_text(span, (was, became), &before, tx, &now),
        // A counter throughout: it loses what the invocation added.
        (
            Some((Value::Scalar(before), _)),
            Some((Value::Scalar(after), _)),
            Some((Value::Scalar(now), _)),
        ) if counted(&before).is_some() && counted(&after).is_some() && counted(&now).is_some() => {
            let added = counted(&after)
                .unwrap_or_default()
                .saturating_sub(counted(&before).unwrap_or_default());
            tx.increment(root, key, added.saturating_neg())
        }
        _ => crate::store::restore_key(was, key, tx, root),
    }
}

fn counted(value: &ScalarValue) -> Option<i64> {
    match value {
        ScalarValue::Counter(counter) => Some(i64::from(counter)),
        _ => None,
    }
}

/// Takes back the invocation's edits of the text `obj`: deletes what it inserted, and puts back
/// what it deleted after the character that stood before it.
fn undo_text(
    span: &Span,
    (was, became): (&Automerge, &Automerge),
    obj: &ObjId,
    tx: &mut Transaction<'_>,
    now: &ObjId,
) -> Result<(), automerge::AutomergeError> {
    let inserted: Vec<usize> = tx
        .list_range(now, ..)
        .filter(|item| span.made(&item.id()))
        .map(|item| item.index)
        .collect();
    for at in inserted.into_iter().rev() {
        tx.splice_text(now, at, 1, "")?;
    }
    // What it deleted: the characters before it that are gone after it, in stretches, each after the
    // character that stood before the stretch.
    let kept: BTreeSet<ObjId> = became.list_range(obj, ..).map(|item| item.id()).collect();
    let mut gaps: Vec<(Option<ObjId>, String)> = Vec::new();
    let mut anchor: Option<ObjId> = None;
    let mut in_gap = false;
    for item in was.list_range(obj, ..) {
        let id = item.id();
        if kept.contains(&id) {
            anchor = Some(id);
            in_gap = false;
            continue;
        }
        let Some(text) = Value::from(item.value).into_string().ok() else {
            continue;
        };
        match gaps.last_mut() {
            Some((_, gap)) if in_gap => gap.push_str(&text),
            _ => gaps.push((anchor.clone(), text)),
        }
        in_gap = true;
    }
    for (anchor, text) in gaps {
        let at = match &anchor {
            None => 0,
            Some(anchor) => {
                match tx.list_range(now, ..).find(|item| &item.id() == anchor) {
                    Some(item) => item.index.saturating_add(1),
                    // The character before it is gone too: at the start.
                    None => 0,
                }
            }
        };
        tx.splice_text(now, at, 0, &text)?;
    }
    Ok(())
}
