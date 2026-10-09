//! Where Ctrl+Z meets the document's own history.
//!
//! The undo and redo stacks of [`UndoLog`](super::UndoLog) hold this
//! session's spans (with the caret to restore), but what is undone is
//! recorded in the document, as the published `undo` / `redo` verbs record
//! it ([`steps`]): an undo marks each change it took back, a redo reverts
//! that undo. So Ctrl+Z, an agent's `undo` and a script see one history,
//! and after a restart Ctrl+Z goes on taking back the person's own steps
//! from the document.

use std::collections::{HashSet, VecDeque};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ScalarValue};
use tilo_actions::{ActionContext, Did};

use crate::history_source::steps::{self, Scope, UNDONE_KEY};
use crate::permissions::UserId;

/// The changes of the span `before..after`: those `after` reaches that
/// came after `before`.
pub(super) fn span_changes(
    doc: &AutoCommit,
    before: &[ChangeHash],
    after: &[ChangeHash],
) -> Vec<ChangeHash> {
    let since: HashSet<ChangeHash> = doc
        .committed_document()
        .get_changes_meta(before)
        .iter()
        .map(|meta| meta.hash)
        .collect();
    let mut seen = HashSet::new();
    let mut queue: VecDeque<ChangeHash> = after.iter().copied().collect();
    let mut out = Vec::new();
    while let Some(hash) = queue.pop_front() {
        if !since.contains(&hash) || !seen.insert(hash) {
            continue;
        }
        out.push(hash);
        if let Some(meta) = doc.committed_document().get_change_meta_by_hash(&hash) {
            queue.extend(meta.deps.iter().copied());
        }
    }
    out
}

/// The entry a change recorded, if any.
fn entry_of(doc: &AutoCommit, hash: &ChangeHash) -> Option<ActionContext> {
    doc.committed_document()
        .get_change_meta_by_hash(hash)?
        .message
        .as_deref()
        .and_then(ActionContext::from_change_message)
}

/// The changes of a span an undo marks as taken back: steps of the content
/// and redos (undoing a redo takes back what it brought back), not
/// housekeeping and not undos.
pub(super) fn steps_of(doc: &AutoCommit, changes: &[ChangeHash]) -> Vec<ChangeHash> {
    changes
        .iter()
        .filter(|hash| {
            entry_of(doc, hash).is_none_or(|entry| {
                entry.change.is_step()
                    && !matches!(
                        &entry.what,
                        Some(Did::History(step)) if step.op == tilo_actions::HistoryOp::Undo
                    )
            })
        })
        .copied()
        .collect()
}

/// Whether a span is nothing but undos and redos made through the published
/// verbs (an agent's or a script's): no step of anyone's to take back with
/// Ctrl+Z. This stack's own moves are native commits, never the executor's.
pub(super) fn published_moves_only(doc: &AutoCommit, changes: &[ChangeHash]) -> bool {
    !changes.is_empty()
        && changes.iter().all(|hash| {
            entry_of(doc, hash).is_some_and(|entry| {
                entry.executed_action.is_some()
                    && matches!(
                        &entry.what,
                        Some(Did::History(step))
                            if matches!(step.op, tilo_actions::HistoryOp::Undo | tilo_actions::HistoryOp::Redo)
                    )
            })
        })
}

/// Mark `changes` as undone in the open transaction.
pub(super) fn mark_undone(doc: &mut AutoCommit, changes: &[ChangeHash]) {
    for hash in changes {
        let _ = doc.put(
            automerge::ROOT,
            format!("{UNDONE_KEY}{hash}"),
            ScalarValue::Boolean(true),
        );
    }
}

/// Whether every step of a span is already undone, as another way in (an
/// agent's or a script's `undo`) left it.
pub(super) fn already_undone(doc: &AutoCommit, steps: &[ChangeHash]) -> bool {
    if steps.is_empty() {
        return false;
    }
    let undone = steps::undone(doc);
    steps.iter().all(|hash| undone.contains(&hash.to_string()))
}

/// Take back `author`'s last step in effect from the document's history, as
/// the published `undo` does, and commit it as their undo. Returns the
/// heads before and after, and the entry of the step taken back.
pub(super) fn undo_from_document(
    doc: &mut AutoCommit,
    author: &UserId,
    registry: Option<&tilo_actions::ActionRegistry>,
) -> Option<(Vec<ChangeHash>, Vec<ChangeHash>, Option<ActionContext>)> {
    let scope = Scope::Invoker(author.0.clone());
    let before = doc.get_heads();
    let mut original = doc.fork();
    let taken = steps::undo_with(doc, &scope, 1, |version| {
        let Some(registry) = registry else {
            return Ok(None);
        };
        let Some(_) =
            tilo_actions::executor::verified_compensation(&mut original, version.hash, registry)
                .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        let recorded = version
            .context
            .as_ref()
            .and_then(|context| context.compensation.as_ref())
            .ok_or("verified compensation absent")?;
        let action = registry
            .decode(&recorded.undo)
            .map_err(|error| error.to_string())?;
        let requires_settlement = registry
            .decode(&recorded.redo)
            .map_err(|error| error.to_string())?
            .inverse_requires_settlement();
        Ok(Some(steps::UndoCompensation {
            action,
            requires_settlement,
        }))
    });
    let Ok(taken) = taken else {
        doc.rollback();
        return None;
    };
    // Undoing a redo takes back the step it brought back, and says so.
    let step = taken
        .last()
        .and_then(|version| version.context.clone())
        .map(|context| match &context.what {
            Some(Did::History(step)) if step.op == tilo_actions::HistoryOp::Redo => step
                .of
                .as_deref()
                .cloned()
                .map_or(context.clone(), |described| {
                    ActionContext::default().with_described(described)
                }),
            _ => context,
        });
    commit_history(doc, author, tilo_actions::HistoryOp::Undo, step.as_ref());
    Some((before, doc.get_heads(), step))
}

/// Bring back `author`'s last undo from the document's history, as the
/// published `redo` does.
pub(super) fn redo_from_document(
    doc: &mut AutoCommit,
    author: &UserId,
) -> Option<(Vec<ChangeHash>, Vec<ChangeHash>)> {
    let scope = Scope::Invoker(author.0.clone());
    let before = doc.get_heads();
    let Ok(brought) = steps::redo(doc, &scope, 1) else {
        doc.rollback();
        return None;
    };
    let step = brought
        .last()
        .and_then(|version| version.context.as_ref())
        .and_then(|undo| match &undo.what {
            Some(Did::History(step)) => step.of.as_deref().cloned(),
            _ => None,
        })
        .map(|described| ActionContext::default().with_described(described));
    commit_history(doc, author, tilo_actions::HistoryOp::Redo, step.as_ref());
    Some((before, doc.get_heads()))
}

/// Commit the open transaction as `author`'s undo or redo of `step`.
fn commit_history(
    doc: &mut AutoCommit,
    author: &UserId,
    op: tilo_actions::HistoryOp,
    step: Option<&ActionContext>,
) {
    let entry = ActionContext {
        what: Some(Did::history(op, 1, step.and_then(ActionContext::described))),
        app: step.and_then(|step| step.app.clone()),
        resource: step.and_then(|step| step.resource.clone()),
        group: step.and_then(|step| step.group.clone()),
        invoker: Some(tilo_actions::ActionInvoker::actor(author.0.clone())),
        ..ActionContext::default()
    };
    let _ = tilo_resource::history::commit_stamped_with_message(doc, entry.change_message());
}
