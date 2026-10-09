//! Ctrl+Z over the document's own history.
//!
//! There is one undo engine: the document's history
//! ([`steps`](crate::history_source::steps)). An undo marks what it took
//! back as undone in the same forward change, a redo reverts that undo, and
//! the changes of one gesture (the keystrokes of a word) share a step id so
//! they are taken back as one step. Ctrl+Z, an agent's `undo` and a script's
//! read and write the same history, and it survives a restart.
//!
//! [`UndoLog`] is what a window keeps beside it: where the next step starts
//! (the commit boundary and the open step id), and transient presentation of
//! the steps made in this session (where the caret was, which window instance
//! made them, who the host said made a change that carries no entry).

use std::collections::{HashMap, HashSet, VecDeque};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ReadDoc, ScalarValue};
use tilo_actions::{ActionContext, ActionId, Did, HistoryOp};
use tilo_resource::history::Version;

use super::owned_restore;
use crate::application::InstanceId;
use crate::history_source::authors;
use crate::history_source::steps::{self, UNDONE_KEY};
use crate::permissions::UserId;

/// One step of the history as a window shows it: the changes it brackets,
/// where the caret was, and who made it.
#[derive(Clone, Debug)]
pub struct UndoEntry {
    pub before: Vec<ChangeHash>,
    pub after: Vec<ChangeHash>,
    pub focused_node_idx: Option<usize>,
    pub widget_metadata: Option<String>,
    pub author: Option<UserId>,
    /// Runtime instance attribution, independent of authenticated author/CRDT actor.
    pub instance_id: Option<InstanceId>,
    /// The entry of the step this undoes or redoes, so history names "Undo
    /// Rename" after the step itself.
    pub step: Option<ActionContext>,
}

/// What a window remembers about a change it made, for this session only.
#[derive(Clone, Debug, Default)]
struct StepPresentation {
    focused_node_idx: Option<usize>,
    widget_metadata: Option<String>,
    /// Who the host said made it, when its changes name nobody.
    author: Option<UserId>,
    instance_id: Option<InstanceId>,
    /// The step it closed in, so this session takes it back with the rest
    /// of that step.
    step: u64,
}

/// A document's undo, as one window drives it. See the module docs.
pub struct UndoLog {
    /// Heads as of the last commit boundary: where the next step starts.
    pub(super) last_heads: Vec<ChangeHash>,
    /// Bumped every time the history moves. Lets a host tell "the history
    /// moved" from "same as last frame" without reading the document.
    pub(super) revision: u64,
    /// The step the changes made until the next boundary join.
    open: ActionId,
    /// This session's changes: where the caret was when their step closed,
    /// who the host said made them, and which instance.
    presented: HashMap<ChangeHash, StepPresentation>,
    /// Numbers this session's steps.
    steps: u64,
    /// The document's history as the engine reads it, kept up to date.
    index: std::cell::RefCell<steps::StepIndex>,
    /// How many changes the document had when this history began.
    origin_len: usize,
}

impl Default for UndoLog {
    /// A history with no baseline yet, for a holder that swaps a document's
    /// own in and out.
    fn default() -> Self {
        Self {
            last_heads: Vec::new(),
            revision: 0,
            open: ActionId::new(),
            presented: HashMap::new(),
            steps: 0,
            index: std::cell::RefCell::default(),
            origin_len: 0,
        }
    }
}

/// The key a change is remembered under.
fn step_key(version: &Version) -> ChangeHash {
    version.hash
}

/// The changes of the span `before..after`: those `after` reaches that came
/// after `before`.
fn span_changes(doc: &AutoCommit, before: &[ChangeHash], after: &[ChangeHash]) -> Vec<ChangeHash> {
    let document = doc.committed_document();
    let since: HashSet<ChangeHash> = document
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
        if let Some(meta) = document.get_change_meta_by_hash(&hash) {
            queue.extend(meta.deps.iter().copied());
        }
    }
    out
}

/// The heads a step starts from and ends at.
fn bracket(doc: &AutoCommit, step: &[Version]) -> (Vec<ChangeHash>, Vec<ChangeHash>) {
    let document = doc.committed_document();
    let hashes: HashSet<ChangeHash> = step.iter().map(|version| version.hash).collect();
    let mut before: Vec<ChangeHash> = step
        .iter()
        .filter_map(|version| document.get_change_meta_by_hash(&version.hash))
        .flat_map(|meta| meta.deps.clone())
        .filter(|dep| !hashes.contains(dep))
        .collect();
    before.sort();
    before.dedup();
    let after = step
        .first()
        .map(|newest| vec![newest.hash])
        .unwrap_or_default();
    (before, after)
}

/// A verified inverse: the changes it takes back, the action, and whether
/// it counts only once its outside consequence settles.
type Compensating = (Vec<ChangeHash>, Box<dyn tilo_actions::Action>, bool);

/// How a redo brings back the outside step an undo took back.
struct Restoration {
    action: Box<dyn tilo_actions::Action>,
    originals: Vec<String>,
    settles: bool,
}

/// What a history may take back: not the open step, only this device's.
struct Reach {
    open: HashSet<ChangeHash>,
    device: authors::DeviceActors,
}

/// Where a history move was taken (the window and its caret), and what
/// undoing it applies when that is an action.
struct Placement {
    instance: Option<InstanceId>,
    focus: Option<usize>,
    metadata: Option<String>,
    compensation: Option<tilo_actions::EncodedAction>,
}

/// Whether an entry is an undo or redo through history.
fn moves_history(context: &ActionContext) -> Option<&HistoryOp> {
    match &context.what {
        Some(Did::History(step)) if matches!(step.op, HistoryOp::Undo | HistoryOp::Redo) => {
            Some(&step.op)
        }
        _ => None,
    }
}

/// The entry of the step an undo or redo moved, in the app and resource it
/// was in.
fn moved_step(context: &ActionContext) -> Option<ActionContext> {
    let Some(Did::History(step)) = &context.what else {
        return None;
    };
    let of = step.of.as_deref().cloned()?;
    Some(
        ActionContext {
            what: None,
            item: None,
            gesture: None,
            ..context.clone()
        }
        .with_described(of),
    )
}

impl UndoLog {
    /// A history over `doc` as it stands. The current heads become the
    /// baseline the next step brackets against, so content already in the
    /// document is not attributed to a step nobody took here.
    pub fn new(doc: &mut AutoCommit) -> Self {
        let heads = doc.get_heads();
        let mut log = Self {
            last_heads: heads,
            revision: 0,
            open: ActionId::new(),
            presented: HashMap::new(),
            steps: 0,
            index: std::cell::RefCell::default(),
            origin_len: 0,
        };
        log.index.borrow_mut().refresh(doc);
        log.origin_len = log.index.borrow().versions().len();
        log.open_next();
        log
    }

    /// Open the next step: the changes made from now on join it.
    fn open_next(&mut self) {
        self.open = ActionId::new();
        tilo_actions::open_step(Some(self.open));
    }

    /// A counter that changes whenever the history this window drives moves,
    /// and never otherwise: the cheap "did anything change?" test for an
    /// Undo control.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether another distinct revision can be issued.
    pub(super) fn can_bump(&self) -> bool {
        self.revision != u64::MAX
    }

    fn bump(&mut self) {
        if let Some(next) = self.revision.checked_add(1) {
            self.revision = next;
        }
    }

    /// Re-baseline against the document's current heads: what a merge of
    /// remote changes needs, so the next local step brackets against the
    /// merged picture. A step open before the merge ends at it.
    pub fn rebase(&mut self, doc: &mut AutoCommit) {
        self.last_heads = doc.get_heads();
        // What arrived is its authors' steps, not this window's open one.
        let mut index = self.index.borrow_mut();
        index.refresh(doc);
        self.origin_len = index.versions().len();
        drop(index);
        self.open_next();
    }

    /// Who a change is by, as far as this window knows: the host's word for
    /// this session's steps, else the invoker its entry records.
    fn author_of<'v>(&'v self, version: &'v Version) -> Option<&'v str> {
        let presented = self.presented.get(&step_key(version));
        presented
            .and_then(|step| step.author.as_ref())
            .map(|author| author.0.as_str())
            .or_else(|| {
                version
                    .context
                    .as_ref()
                    .and_then(|context| context.invoker.as_ref())
                    .map(|invoker| invoker.actor.as_str())
            })
    }

    /// Whether a change is a step `author` takes back from `instance`:
    /// their own (an unattributed step of this session is anyone's), and
    /// for an instance, one it made or one from before this session.
    /// Without an author, any step of this session.
    fn claims(
        &self,
        version: &Version,
        author: Option<&UserId>,
        instance: Option<InstanceId>,
        reach: &Reach,
    ) -> bool {
        // The open step is no step yet: it ends at the next boundary.
        if reach.open.contains(&version.hash) {
            return false;
        }
        let presented = self.presented.get(&step_key(version));
        if version
            .context
            .as_ref()
            .and_then(|context| context.group.as_ref())
            .is_some_and(|group| group.kind == steps::AGENT_RUN)
        {
            return false;
        }
        let by = match author {
            None => presented.is_some(),
            Some(author) => match self.author_of(version) {
                Some(by) => by == author.0,
                None => presented.is_some(),
            },
        };
        // Only what was done here: a step on another device is that
        // device's to take back (or the published undo's, by scope).
        by && instance
            .is_none_or(|instance| presented.is_none_or(|step| step.instance_id == Some(instance)))
            && (presented.is_some() || reach.device.wrote(&version.actor))
    }

    /// The step a change is taken back with: the step this session closed
    /// it in, else the step its entry recorded.
    fn step_of(&self, version: &Version) -> Option<String> {
        match self.presented.get(&version.hash) {
            Some(presented) => Some(format!("session:{}", presented.step)),
            None => steps::gesture_of(version),
        }
    }

    /// What this history can take back in `doc`: not the changes since it
    /// began that it never closed a step over, and only what this device
    /// wrote.
    fn open_changes(&self, doc: &AutoCommit) -> Reach {
        // Changes made since this history began that it never closed a step
        // over (the open step, an unrecorded raw write) are nobody's step.
        let mut index = self.index.borrow_mut();
        index.refresh(doc);
        let open = index
            .versions()
            .get(self.origin_len..)
            .unwrap_or_default()
            .iter()
            .map(|version| version.hash)
            .filter(|hash| !self.presented.contains_key(hash))
            .collect();
        Reach {
            open,
            device: authors::DeviceActors::of(doc.get_actor()),
        }
    }

    fn step_entry(&self, doc: &AutoCommit, step: &[Version]) -> UndoEntry {
        let (before, after) = bracket(doc, step);
        let presented = step
            .first()
            .and_then(|newest| self.presented.get(&step_key(newest)));
        UndoEntry {
            before,
            after,
            focused_node_idx: presented.and_then(|step| step.focused_node_idx),
            widget_metadata: presented.and_then(|step| step.widget_metadata.clone()),
            author: step
                .first()
                .and_then(|newest| self.author_of(newest))
                .map(|by| UserId(by.to_owned())),
            instance_id: presented.and_then(|step| step.instance_id),
            // A redo stands for the step it brought back.
            step: step.first().and_then(|newest| {
                let context = newest.context.as_ref()?;
                if moves_history(context) == Some(&HistoryOp::Redo) {
                    moved_step(context)
                } else {
                    Some(context.clone())
                }
            }),
        }
    }

    fn next_undo_step(
        &self,
        doc: &AutoCommit,
        author: Option<&UserId>,
        instance: Option<InstanceId>,
    ) -> Option<Vec<Version>> {
        let open = self.open_changes(doc);
        self.index
            .borrow_mut()
            .steps(
                doc,
                &|version| self.claims(version, author, instance, &open),
                &|version| self.step_of(version),
                1,
            )
            .into_iter()
            .next()
    }

    fn next_redo_undo(
        &self,
        doc: &AutoCommit,
        author: Option<&UserId>,
        instance: Option<InstanceId>,
    ) -> Option<Version> {
        let open = self.open_changes(doc);
        self.index
            .borrow_mut()
            .undos(
                doc,
                &|version| self.claims(version, author, instance, &open),
                1,
            )
            .into_iter()
            .next()
    }

    /// The step a Ctrl+Z by `author` would take back, without taking it
    /// back. With an author, other people's steps are walked past; without
    /// one, any step of this session.
    pub fn next_undo_as(&self, doc: &AutoCommit, author: Option<&UserId>) -> Option<UndoEntry> {
        self.next_undo_step(doc, author, None)
            .map(|step| self.step_entry(doc, &step))
    }

    /// The undo a Ctrl+Shift+Z by `author` would bring back. The entry
    /// brackets the undo change; the caret is where it was when the undo
    /// was taken.
    pub fn next_redo_as(&self, doc: &AutoCommit, author: Option<&UserId>) -> Option<UndoEntry> {
        self.next_redo_undo(doc, author, None)
            .map(|undo| self.redo_entry(doc, &undo))
    }

    fn redo_entry(&self, doc: &AutoCommit, undo: &Version) -> UndoEntry {
        let mut entry = self.step_entry(doc, std::slice::from_ref(undo));
        // The step the undo took back, in the app and resource it was in.
        entry.step = undo.context.as_ref().and_then(moved_step);
        entry
    }

    /// Whether `author` has anything to undo — true exactly when
    /// [`undo_as`](Self::undo_as) would take a step back.
    pub fn can_undo_as(&self, doc: &AutoCommit, author: Option<&UserId>) -> bool {
        self.next_undo_step(doc, author, None).is_some()
    }

    /// Whether `author` has anything to redo.
    pub fn can_redo_as(&self, doc: &AutoCommit, author: Option<&UserId>) -> bool {
        self.next_redo_undo(doc, author, None).is_some()
    }

    /// The newest step of this session in effect, whoever made it.
    pub fn peek_last_undo(&self, doc: &AutoCommit) -> Option<UndoEntry> {
        self.next_undo_as(doc, None)
    }

    /// How many steps of this session are in effect, whoever made them.
    pub fn undo_depth(&self, doc: &AutoCommit) -> usize {
        let open = self.open_changes(doc);
        self.index
            .borrow_mut()
            .steps(
                doc,
                &|version| self.claims(version, None, None, &open),
                &|version| self.step_of(version),
                usize::MAX,
            )
            .len()
    }

    /// How many undos of this session redo can bring back.
    pub fn redo_depth(&self, doc: &AutoCommit) -> usize {
        let open = self.open_changes(doc);
        self.index
            .borrow_mut()
            .undos(
                doc,
                &|version| self.claims(version, None, None, &open),
                usize::MAX,
            )
            .len()
    }

    /// The brackets of this session's steps in effect and of its undos redo
    /// would bring back, newest first.
    #[cfg(test)]
    pub(crate) fn brackets(
        &self,
        doc: &AutoCommit,
    ) -> (
        Vec<(Vec<ChangeHash>, Vec<ChangeHash>)>,
        Vec<(Vec<ChangeHash>, Vec<ChangeHash>)>,
    ) {
        let open = self.open_changes(doc);
        let undo = self
            .index
            .borrow_mut()
            .steps(
                doc,
                &|version| self.claims(version, None, None, &open),
                &|version| self.step_of(version),
                usize::MAX,
            )
            .iter()
            .map(|step| bracket(doc, step))
            .collect();
        let open = self.open_changes(doc);
        let redo = self
            .index
            .borrow_mut()
            .undos(
                doc,
                &|version| self.claims(version, None, None, &open),
                usize::MAX,
            )
            .iter()
            .map(|undo| bracket(doc, std::slice::from_ref(undo)))
            .collect();
        (undo, redo)
    }

    /// Commit `doc`'s open transaction as a step, bracketed from the last
    /// boundary. Returns `true` iff this committed a change (there were
    /// pending ops). `false` means the transaction was empty — the caller can
    /// then skip any persistence / broadcast work.
    pub fn commit_action_authored(
        &mut self,
        doc: &mut AutoCommit,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) -> bool {
        self.commit_action_authored_with_message(doc, focused_node_idx, metadata, author, None)
    }

    /// As [`Self::commit_action_authored`], with the entry of native CRDT
    /// edits that deliberately do not travel through an action envelope (for
    /// example, co-edited flowing text).
    pub fn commit_action_authored_with_message(
        &mut self,
        doc: &mut AutoCommit,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
        message: Option<String>,
    ) -> bool {
        if !self.can_bump() {
            return false;
        }
        self.commit_owned_pending(
            doc,
            self.last_heads.clone(),
            owned_restore::OwnedActionAttribution {
                author,
                instance_id: None,
                focused_node_idx,
                widget_metadata: metadata,
                message,
            },
        )
        .changed
    }

    /// The entry of a change this window commits: the given one (or none),
    /// attributed to `author` and joined to the open step.
    pub(super) fn stamp(&self, message: Option<String>, author: Option<&UserId>) -> Option<String> {
        let mut context = message.as_deref().map_or_else(
            || Some(ActionContext::default()),
            ActionContext::from_change_message,
        );
        let Some(context) = context.as_mut() else {
            // An app's own commit message is kept as it is.
            return message;
        };
        if context.invoker.is_none()
            && let Some(author) = author
        {
            context.invoker = Some(tilo_actions::ActionInvoker::actor(author.0.clone()));
        }
        if context.gesture.is_none() {
            context.gesture = Some(self.open);
        }
        context.change_message()
    }

    /// Record a step already committed into `doc` out of band (an action the
    /// palette dispatched, which owns its own commit boundary), bracketed by
    /// `before`. Unattributed: an author-scoped Ctrl+Z of this session claims
    /// it.
    pub fn record_committed_action(
        &mut self,
        doc: &mut AutoCommit,
        before: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
    ) -> bool {
        self.record_committed_action_authored(doc, before, focused_node_idx, metadata, None)
    }

    /// Observe the recorded committed frontier without closing pending work.
    pub(crate) fn matches_recorded_frontier(&self, doc: &AutoCommit) -> bool {
        owned_restore::same_frontier(&self.last_heads, &doc.base_heads())
    }

    /// As [`Self::record_committed_action`], naming who performed it — for a
    /// change that reached this document from outside the window (an agent,
    /// a collaborator's published operation) under the caller's own actor.
    /// The name keeps the steps apart afterwards.
    pub fn record_committed_action_authored(
        &mut self,
        doc: &mut AutoCommit,
        before: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) -> bool {
        let after = doc.get_heads();
        self.record_committed_span(doc, before, after, focused_node_idx, metadata, author, None)
    }

    /// Record a bound action of one window instance.
    pub fn record_committed_action_in(
        &mut self,
        doc: &mut AutoCommit,
        before: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: UserId,
        instance: InstanceId,
    ) -> bool {
        let after = doc.get_heads();
        self.record_committed_span(
            doc,
            before,
            after,
            focused_node_idx,
            metadata,
            Some(author),
            Some(instance),
        )
    }

    /// Close the step `before..after`: remember where the caret was and who
    /// made it, and open the next step.
    pub(crate) fn record_committed_span(
        &mut self,
        doc: &AutoCommit,
        before: Vec<ChangeHash>,
        after: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
        instance_id: Option<InstanceId>,
    ) -> bool {
        if after == before {
            self.last_heads = after;
            return false;
        }
        if !self.can_bump() {
            return false;
        }
        self.steps = self.steps.wrapping_add(1);
        for hash in span_changes(doc, &before, &after) {
            self.presented.insert(
                hash,
                StepPresentation {
                    focused_node_idx,
                    widget_metadata: metadata.clone(),
                    author: author.clone(),
                    instance_id,
                    step: self.steps,
                },
            );
        }
        self.last_heads = after;
        self.open_next();
        self.bump();
        true
    }

    /// Take back the step `author` (from `instance`) would undo, as one new
    /// change that marks it undone. `focus` / `metadata` are where the caret
    /// is now, for the redo to return to. With `registry`, a step that
    /// queued an outside consequence is taken back through its verified
    /// inverse action.
    fn undo_step(
        &mut self,
        doc: &mut AutoCommit,
        author: Option<&UserId>,
        instance: Option<InstanceId>,
        focus: Option<usize>,
        metadata: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Option<UndoEntry> {
        if !self.can_bump() {
            return None;
        }
        doc.get_heads();
        let step = self.next_undo_step(doc, author, instance)?;
        let entry = self.step_entry(doc, &step);
        let compensation = registry.and_then(|registry| self.compensation(doc, &step, registry));
        // Undoing a redo takes back again the steps it brought back.
        let step = self.taken_back_by(doc, step);
        // An inverse that queues an outside consequence counts as done only
        // once its receipt settles it; until then the step stays in effect.
        let mut pending: HashSet<ChangeHash> = HashSet::new();
        let applied = compensation.is_some_and(|actions| {
            actions.iter().all(|(marks, action, settles)| {
                if *settles {
                    pending.extend(marks.iter().copied());
                }
                action.apply(doc).is_ok()
            })
        });
        if !applied {
            pending.clear();
            super::write_revert_patches(doc, &entry.after, &entry.before);
        }
        for version in &step {
            if version.context.as_ref().and_then(moves_history) == Some(&HistoryOp::Undo) {
                continue;
            }
            let key = format!("{UNDONE_KEY}{}", version.hash);
            let marker = if pending.contains(&version.hash) {
                let attempt = doc
                    .get(automerge::ROOT, &key)
                    .ok()
                    .flatten()
                    .and_then(|(value, _)| value.as_i64())
                    .filter(|attempt| *attempt < 0)
                    .unwrap_or(0)
                    .saturating_sub(1);
                ScalarValue::Int(attempt)
            } else {
                ScalarValue::Boolean(true)
            };
            let _ = doc.put(automerge::ROOT, key, marker);
        }
        let by = entry.author.clone().or_else(|| author.cloned());
        self.commit_move(
            doc,
            HistoryOp::Undo,
            entry.step.as_ref(),
            by,
            Placement {
                instance,
                focus,
                metadata,
                compensation: None,
            },
        );
        Some(entry)
    }

    /// The changes undoing `step` takes back: a redo's own originals, any
    /// other change itself.
    fn taken_back_by(&self, doc: &AutoCommit, step: Vec<Version>) -> Vec<Version> {
        let mut index = self.index.borrow_mut();
        index.refresh(doc);
        step.into_iter()
            .flat_map(|version| {
                let back = steps::brought_back(doc, &version);
                if back.is_empty() {
                    return vec![version];
                }
                back.iter()
                    .filter_map(|hash| {
                        let hash: ChangeHash = hash.parse().ok()?;
                        index.versions().get(index.position(&hash)?).cloned()
                    })
                    .collect()
            })
            .collect()
    }

    /// The verified inverse actions of a step that queued an outside
    /// consequence, newest first: every change's recorded compensation,
    /// each replaying exactly, with the changes it takes back and whether it
    /// settles. A redo's is the inverse of the restoration it applied.
    /// `None` when any change has none.
    fn compensation(
        &self,
        doc: &mut AutoCommit,
        step: &[Version],
        registry: &tilo_actions::ActionRegistry,
    ) -> Option<Vec<Compensating>> {
        let mut original = doc.fork();
        step.iter()
            .map(|change| {
                let brought_back: Vec<ChangeHash> = steps::brought_back(doc, change)
                    .iter()
                    .filter_map(|hash| hash.parse().ok())
                    .collect();
                if !brought_back.is_empty() {
                    let (action, settles) =
                        self.redo_compensation(doc, change, &brought_back, registry)?;
                    return Some((brought_back, action, settles));
                }
                let context = change.context.as_ref()?;
                let inverse = tilo_actions::executor::verified_compensation(
                    &mut original,
                    change.hash,
                    registry,
                )
                .ok()
                .flatten()?;
                let settles = registry
                    .decode(context.action.as_ref()?)
                    .ok()?
                    .inverse_requires_settlement();
                Some((vec![change.hash], inverse, settles))
            })
            .collect()
    }

    /// The inverse of the restoration the redo standing for `originals`
    /// applied, verified: the original replays, the redo recorded the
    /// restoration it declares at the redo's own state, and the inverse is
    /// that restoration's as the redo's writer computes it there.
    fn redo_compensation(
        &self,
        doc: &mut AutoCommit,
        standing: &Version,
        originals: &[ChangeHash],
        registry: &tilo_actions::ActionRegistry,
    ) -> Option<(Box<dyn tilo_actions::Action>, bool)> {
        let [original] = originals else {
            return None;
        };
        let keys: Vec<String> = originals
            .iter()
            .map(|hash| format!("{UNDONE_KEY}{hash}"))
            .collect();
        // Newest first from the change standing for the redo: the redo move
        // that brought the original back (a settle may have followed it).
        let candidates: Vec<Version> = {
            let mut index = self.index.borrow_mut();
            index.refresh(doc);
            let from = index.position(&standing.hash)?;
            index
                .versions()
                .get(..=from)?
                .iter()
                .rev()
                .cloned()
                .collect()
        };
        let moved = candidates.into_iter().find_map(|version| {
            let document = doc.committed_document();
            let meta = document.get_change_meta_by_hash(&version.hash)?;
            let marks = document
                .diff(&meta.deps, &[version.hash])
                .iter()
                .any(|patch| {
                    patch.path.is_empty()
                        && matches!(&patch.action,
                            automerge::PatchAction::PutMap { key, .. }
                            | automerge::PatchAction::DeleteMap { key } if keys.contains(key))
                });
            marks.then_some(version)
        })?;
        let context = moved.context.as_ref()?;
        if moves_history(context) != Some(&HistoryOp::Redo) {
            // An undo or a settle came first: nothing records the redo's run.
            return None;
        }
        let recorded = context.compensation.clone()?;
        let (deps, actor) = {
            let meta = doc
                .committed_document()
                .get_change_meta_by_hash(&moved.hash)?;
            (meta.deps.clone(), meta.actor.to_bytes().to_vec())
        };
        let mut replica = doc.fork();
        tilo_actions::executor::verified_compensation(&mut replica, *original, registry).ok()??;
        let (forward, again) = {
            let mut index = self.index.borrow_mut();
            index.refresh(doc);
            let position = index.position(original)?;
            let again = index
                .versions()
                .get(position)?
                .context
                .as_ref()?
                .action
                .clone()?;
            (registry.decode(&again).ok()?, again)
        };
        // What the redo ran there, and its inverse as the redo's writer.
        let mut at = doc.fork_at(&deps).ok()?;
        let declared = forward.redo_encoded(&at).unwrap_or(again);
        let restoration = registry.decode(&declared).ok()?;
        let inverse =
            tilo_actions::executor::inverse_encoded_by(&mut at, restoration.as_ref(), &actor)?;
        if inverse != recorded {
            return None;
        }
        Some((
            registry.decode(&recorded).ok()?,
            forward.inverse_requires_settlement(),
        ))
    }

    /// Commit an undo or redo of `step` by `by`, as its own step, from
    /// where `place` says it was taken.
    fn commit_move(
        &mut self,
        doc: &mut AutoCommit,
        op: HistoryOp,
        step: Option<&ActionContext>,
        by: Option<UserId>,
        place: Placement,
    ) {
        let Placement {
            instance,
            focus,
            metadata,
            compensation,
        } = place;
        let entry = ActionContext {
            what: Some(Did::history(op, 1, step.and_then(ActionContext::described))),
            app: step.and_then(|step| step.app.clone()),
            resource: step.and_then(|step| step.resource.clone()),
            group: step.and_then(|step| step.group.clone()),
            invoker: by
                .as_ref()
                .map(|by| tilo_actions::ActionInvoker::actor(by.0.clone()))
                .or_else(|| step.and_then(|step| step.invoker.clone())),
            gesture: Some(ActionId::new()),
            compensation,
            ..ActionContext::default()
        };
        let moved =
            tilo_resource::history::commit_stamped_with_message(doc, entry.change_message());
        let Some(moved) = moved else {
            return;
        };
        self.steps = self.steps.wrapping_add(1);
        self.presented.insert(
            moved,
            StepPresentation {
                focused_node_idx: focus,
                widget_metadata: metadata,
                author: by,
                instance_id: instance,
                step: self.steps,
            },
        );
        self.last_heads = doc.get_heads();
        self.open_next();
        self.bump();
    }

    /// Bring back the undo `author` (from `instance`) would redo, by
    /// reverting it.
    fn redo_step(
        &mut self,
        doc: &mut AutoCommit,
        author: Option<&UserId>,
        instance: Option<InstanceId>,
        focus: Option<usize>,
        metadata: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Option<UndoEntry> {
        if !self.can_bump() {
            return None;
        }
        doc.get_heads();
        let undo = self.next_redo_undo(doc, author, instance)?;
        let entry = self.redo_entry(doc, &undo);
        // A step that queued an outside consequence comes back through its
        // declared restoration; any other by reverting the undo.
        let restoration = registry.and_then(|registry| self.restoration(doc, &undo, registry));
        // What undoing this redo applies: the inverse of the restoration as
        // this writer runs it now (what it mints is its own).
        let restored = restoration.and_then(|restoration| {
            let actor = doc.get_actor().to_bytes().to_vec();
            let inverse = tilo_actions::executor::inverse_encoded_by(
                doc,
                restoration.action.as_ref(),
                &actor,
            );
            restoration.action.apply(doc).is_ok().then_some((
                restoration.originals,
                restoration.settles,
                inverse,
            ))
        });
        let mut compensation = None;
        match restored {
            Some((originals, settles, inverse)) => {
                compensation = inverse;
                for original in originals {
                    let key = format!("{UNDONE_KEY}{original}");
                    if settles {
                        // Brought back only once its receipt settles it.
                        let attempt = doc
                            .get(automerge::ROOT, &key)
                            .ok()
                            .flatten()
                            .and_then(|(value, _)| value.as_i64())
                            .filter(|attempt| *attempt > 0)
                            .unwrap_or(0)
                            .saturating_add(1);
                        let _ = doc.put(automerge::ROOT, key, ScalarValue::Int(attempt));
                    } else {
                        let _ = doc.delete(automerge::ROOT, key);
                    }
                }
            }
            None => super::write_revert_patches(doc, &entry.after, &entry.before),
        }
        let by = entry.author.clone().or_else(|| author.cloned());
        self.commit_move(
            doc,
            HistoryOp::Redo,
            entry.step.as_ref(),
            by,
            Placement {
                instance,
                focus,
                metadata,
                compensation,
            },
        );
        Some(entry)
    }

    /// The declared restoration of the one outside step an undo took back.
    fn restoration(
        &self,
        doc: &mut AutoCommit,
        undo: &Version,
        registry: &tilo_actions::ActionRegistry,
    ) -> Option<Restoration> {
        let originals = steps::took_back(doc, undo);
        let [original] = originals.as_slice() else {
            return None;
        };
        let hash: ChangeHash = original.parse().ok()?;
        let mut replica = doc.fork();
        tilo_actions::executor::verified_compensation(&mut replica, hash, registry).ok()??;
        let recorded = {
            let mut index = self.index.borrow_mut();
            index.refresh(doc);
            let position = index.position(&hash)?;
            index
                .versions()
                .get(position)?
                .context
                .as_ref()?
                .action
                .clone()?
        };
        let forward = registry.decode(&recorded).ok()?;
        let settles = forward.inverse_requires_settlement();
        // A declared restoration where the action has one; else the
        // original action again.
        let encoded = forward.redo_encoded(&replica).unwrap_or(recorded);
        let action = registry.decode(&encoded).ok()?;
        Some(Restoration {
            action,
            originals,
            settles,
        })
    }

    pub fn undo(
        &mut self,
        doc: &mut AutoCommit,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo_step(doc, None, None, current_focused_idx, current_metadata, None)
    }

    /// [`Self::undo`] with a registry for steps with outside consequences.
    pub fn undo_with_registry(
        &mut self,
        doc: &mut AutoCommit,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Option<UndoEntry> {
        self.undo_step(
            doc,
            None,
            None,
            current_focused_idx,
            current_metadata,
            registry,
        )
    }

    pub fn undo_for_author(
        &mut self,
        doc: &mut AutoCommit,
        author: &UserId,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo_step(
            doc,
            Some(author),
            None,
            current_focused_idx,
            current_metadata,
            None,
        )
    }

    pub fn undo_for_author_with_registry(
        &mut self,
        doc: &mut AutoCommit,
        author: &UserId,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Option<UndoEntry> {
        self.undo_step(
            doc,
            Some(author),
            None,
            current_focused_idx,
            current_metadata,
            registry,
        )
    }

    pub fn undo_as(
        &mut self,
        doc: &mut AutoCommit,
        author: Option<&UserId>,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo_step(
            doc,
            author,
            None,
            current_focused_idx,
            current_metadata,
            None,
        )
    }

    pub fn undo_as_with_registry(
        &mut self,
        doc: &mut AutoCommit,
        author: Option<&UserId>,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Option<UndoEntry> {
        self.undo_step(
            doc,
            author,
            None,
            current_focused_idx,
            current_metadata,
            registry,
        )
    }

    pub fn redo(
        &mut self,
        doc: &mut AutoCommit,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.redo_step(doc, None, None, current_focused_idx, current_metadata, None)
    }

    pub fn redo_as(
        &mut self,
        doc: &mut AutoCommit,
        author: Option<&UserId>,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.redo_step(
            doc,
            author,
            None,
            current_focused_idx,
            current_metadata,
            None,
        )
    }

    pub fn redo_as_with_registry(
        &mut self,
        doc: &mut AutoCommit,
        author: Option<&UserId>,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Option<UndoEntry> {
        self.redo_step(
            doc,
            author,
            None,
            current_focused_idx,
            current_metadata,
            registry,
        )
    }

    /// The step `author`'s window `instance` would undo.
    pub fn next_undo_in(
        &self,
        doc: &AutoCommit,
        author: &UserId,
        instance: InstanceId,
    ) -> Option<UndoEntry> {
        self.next_undo_step(doc, Some(author), Some(instance))
            .map(|step| self.step_entry(doc, &step))
    }

    /// The undo `author`'s window `instance` would redo.
    pub fn next_redo_in(
        &self,
        doc: &AutoCommit,
        author: &UserId,
        instance: InstanceId,
    ) -> Option<UndoEntry> {
        self.next_redo_undo(doc, Some(author), Some(instance))
            .map(|undo| self.redo_entry(doc, &undo))
    }

    pub fn undo_in(
        &mut self,
        doc: &mut AutoCommit,
        author: &UserId,
        instance: InstanceId,
        focus: Option<usize>,
        metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo_step(doc, Some(author), Some(instance), focus, metadata, None)
    }

    pub fn undo_in_with_registry(
        &mut self,
        doc: &mut AutoCommit,
        author: &UserId,
        instance: InstanceId,
        focus: Option<usize>,
        metadata: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Option<UndoEntry> {
        self.undo_step(doc, Some(author), Some(instance), focus, metadata, registry)
    }

    pub fn redo_in(
        &mut self,
        doc: &mut AutoCommit,
        author: &UserId,
        instance: InstanceId,
        focus: Option<usize>,
        metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.redo_step(doc, Some(author), Some(instance), focus, metadata, None)
    }

    pub fn redo_in_with_registry(
        &mut self,
        doc: &mut AutoCommit,
        author: &UserId,
        instance: InstanceId,
        focus: Option<usize>,
        metadata: Option<String>,
        registry: Option<&tilo_actions::ActionRegistry>,
    ) -> Option<UndoEntry> {
        self.redo_step(doc, Some(author), Some(instance), focus, metadata, registry)
    }

    /// See [`State::restore_to_heads`](super::State::restore_to_heads).
    pub fn restore_to_heads(
        &mut self,
        doc: &mut AutoCommit,
        target_heads: &[ChangeHash],
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) -> bool {
        self.restore_to_heads_owned(
            doc,
            target_heads,
            owned_restore::OwnedActionAttribution {
                author,
                instance_id: None,
                focused_node_idx,
                widget_metadata: metadata,
                message: None,
            },
        )
    }

    /// See [`State::restore_to_heads_with_context`](super::State::restore_to_heads_with_context).
    pub fn restore_to_heads_with_message(
        &mut self,
        doc: &mut AutoCommit,
        target_heads: &[ChangeHash],
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
        message: Option<String>,
    ) -> bool {
        self.restore_to_heads_owned(
            doc,
            target_heads,
            owned_restore::OwnedActionAttribution {
                author,
                instance_id: None,
                focused_node_idx,
                widget_metadata: metadata,
                message,
            },
        )
    }

    pub fn restore_to_heads_in(
        &mut self,
        doc: &mut AutoCommit,
        target_heads: &[ChangeHash],
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: UserId,
        instance: InstanceId,
    ) -> bool {
        self.restore_to_heads_owned(
            doc,
            target_heads,
            owned_restore::OwnedActionAttribution {
                author: Some(author),
                instance_id: Some(instance),
                focused_node_idx,
                widget_metadata: metadata,
                message: None,
            },
        )
    }

    fn restore_to_heads_owned(
        &mut self,
        doc: &mut AutoCommit,
        target_heads: &[ChangeHash],
        attribution: owned_restore::OwnedActionAttribution,
    ) -> bool {
        let current = doc.get_heads();
        if current == target_heads {
            return false;
        }
        if !self.can_bump() {
            return false;
        }
        super::write_revert_patches(doc, &current, target_heads);
        let receipt = self.commit_owned_pending(doc, current, attribution);
        receipt.changed
    }
}
