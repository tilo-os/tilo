//! Undo, redo, restore and checkpoints written into the document itself, so
//! anyone who can invoke an action can use them: a person, a script, an
//! agent over the action door.
//!
//! [`UndoLog`](crate::state::UndoLog) is the process's own stack: it knows
//! only what this window did, and only while it runs. The verbs here read
//! the change graph instead, so "undo my last two steps" means the same on
//! every replica and after a restart, and an agent run is undone by naming
//! it. Every verb writes forward: the step being undone keeps its author,
//! and the undo is itself a step that can be undone.
//!
//! A step belongs to a [`Scope`]: an agent run (every invocation of one
//! agent connection), else the invoking principal, else the writing actor.
//! What a scope has undone is recorded at [`UNDONE_KEY`] root keys, one per
//! undone change. Ordinary undo writes completion in that same change;
//! outside compensation stays pending until its admitted receipt confirms it.
//! A redo removes completed markers through the same selective inversion.

use std::collections::HashSet;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ObjType, Patch, PatchAction, Prop, ReadDoc, ScalarValue};
use tilo_actions::ActionContext;
/// Whether a version is the schema genesis rather than anyone's step.
pub use tilo_doc::model::is_schema_genesis;
use tilo_resource::history::Version;

use crate::history_source::authors::AuthorDirectory;
use crate::history_source::revert::revert_changes;

/// Prefix of the root keys naming each undone change, by its full hash.
pub use tilo_actions::UNDONE_KEY;
/// Prefix of the root keys holding named versions.
pub const CHECKPOINT_KEY: &str = "tilo:checkpoint:";
/// Prefix every bookkeeping root key shares; a restore leaves them alone.
pub const BOOKKEEPING_PREFIX: &str = "tilo:";
/// Longest checkpoint name.
pub const MAX_CHECKPOINT_NAME: usize = 80;
/// The group kind of an agent connection's invocations.
pub const AGENT_RUN: &str = "agent-run";

/// Whose steps an undo or redo takes back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// One agent run, by its group id: every invocation of one connection.
    Run(String),
    /// The principal who invoked, outside any agent run.
    Invoker(String),
    /// The actor that wrote, for an invocation that names nobody.
    Actor(String),
}

impl Scope {
    /// The scope of the invocation `context` describes, written by
    /// `actor_hex`.
    pub fn of(context: Option<&ActionContext>, actor_hex: &str) -> Self {
        if let Some(group) = context
            .and_then(|context| context.group.as_ref())
            .filter(|group| group.kind == AGENT_RUN)
        {
            return Self::Run(group.id.clone());
        }
        if let Some(invoker) = context.and_then(|context| context.invoker.as_ref()) {
            return Self::Invoker(invoker.actor.clone());
        }
        Self::Actor(actor_hex.to_owned())
    }

    /// The scope of the invocation running now ([`tilo_actions::invoking_context`]).
    pub fn invoking(doc: &AutoCommit) -> Self {
        Self::of(
            tilo_actions::invoking_context().as_ref(),
            &doc.get_actor().to_hex_string(),
        )
    }

    /// Whether a change is one of this scope's steps.
    pub fn owns(&self, version: &Version) -> bool {
        let context = version.context.as_ref();
        let run = context
            .and_then(|context| context.group.as_ref())
            .filter(|group| group.kind == AGENT_RUN);
        match self {
            Self::Run(id) => run.is_some_and(|group| &group.id == id),
            Self::Invoker(actor) => {
                run.is_none()
                    && context
                        .and_then(|context| context.invoker.as_ref())
                        .is_some_and(|invoker| &invoker.actor == actor)
            }
            Self::Actor(actor) => run.is_none() && version.actor.eq_ignore_ascii_case(actor),
        }
    }

    /// Who has nothing to undo or redo, as a sentence starts: the caller's
    /// own scope, or the agent run it named.
    fn subject(&self, own: bool) -> String {
        match self {
            Self::Run(id) if own => format!("This agent run ({id:?}) has"),
            Self::Run(id) => format!("Agent run {id:?} has"),
            Self::Invoker(author) if !own => format!("Author {author:?} has"),
            Self::Invoker(_) | Self::Actor(_) => "You have".into(),
        }
    }
}

/// How many of the most recent versions a refusal looks through for the
/// agent runs whose steps are still in effect.
const RUNS_LOOKED_AT: usize = 2000;
/// How many such runs a refusal names.
const RUNS_NAMED: usize = 3;
/// The most steps a refusal counts of one run.
const RUN_STEPS_COUNTED: usize = 50;

/// An agent run whose steps are in effect, as a refusal names it.
struct RunInEffect {
    id: String,
    label: String,
    steps: usize,
}

/// The agent runs other than `scope` with steps still in effect, the most
/// recently active first: what the caller may have meant to undo.
fn runs_in_effect(doc: &mut AutoCommit, scope: &Scope) -> Vec<RunInEffect> {
    let versions = tilo_resource::history::versions(doc);
    let mut candidates: Vec<(String, String)> = Vec::new();
    for version in versions.iter().rev().take(RUNS_LOOKED_AT) {
        let Some(group) = version
            .context
            .as_ref()
            .and_then(|context| context.group.as_ref())
            .filter(|group| group.kind == AGENT_RUN)
        else {
            continue;
        };
        if matches!(scope, Scope::Run(id) if id == &group.id)
            || candidates.iter().any(|(id, _)| id == &group.id)
        {
            continue;
        }
        candidates.push((group.id.clone(), group.label.clone()));
    }
    candidates
        .into_iter()
        .filter_map(|(id, label)| {
            let steps = undo_targets(doc, &Scope::Run(id.clone()), RUN_STEPS_COUNTED).len();
            (steps > 0).then_some(RunInEffect { id, label, steps })
        })
        .take(RUNS_NAMED)
        .collect()
}

/// Why `scope` has nothing to undo, and the undo that takes back each run
/// whose steps are in effect: an agent whose earlier connection was a run of
/// its own copies the call.
fn nothing_to_undo(doc: &mut AutoCommit, scope: &Scope) -> String {
    let own = *scope == Scope::invoking(doc);
    let mut message = format!("{} nothing in effect to undo.", scope.subject(own));
    if own && matches!(scope, Scope::Run(_)) {
        message.push_str(
            " An agent's earlier connection is a run of its own, unless it connected again naming that run.",
        );
    }
    let runs = runs_in_effect(doc, scope);
    if runs.is_empty() {
        message.push_str(" history lists every step and its run_id.");
        return message;
    }
    let named: Vec<String> = runs
        .iter()
        .map(|run| {
            let more = if run.steps >= RUN_STEPS_COUNTED {
                "+"
            } else {
                ""
            };
            let noun = if run.steps == 1 { "step" } else { "steps" };
            format!(
                "{:?} by {} ({}{more} {noun}): undo {{\"run\": {:?}, \"steps\": {}}}",
                run.id, run.label, run.steps, run.id, run.steps
            )
        })
        .collect();
    format!(
        "{message} Agent runs with steps in effect, most recent first: {}. history lists every step and its run_id.",
        named.join("; ")
    )
}

/// What one change was to the steps of its scope.
#[derive(Clone, Debug)]
pub(crate) enum Kind {
    /// An undo: the changes it took back.
    Undo(Vec<String>),
    /// A redo: the changes whose undo it reverted.
    Redo(Vec<String>),
    /// An edit of the content.
    Edit,
    /// Nothing a person did to the content: a checkpoint, a schema genesis,
    /// housekeeping, content from outside.
    Bookkeeping,
}

fn root_key(patch: &Patch) -> Option<&str> {
    match patch.path.first() {
        Some((_, Prop::Map(key))) => Some(key.as_str()),
        Some(_) => None,
        None => match &patch.action {
            PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => Some(key.as_str()),
            _ => None,
        },
    }
}

/// Whether `patch` writes bookkeeping rather than content.
pub fn is_bookkeeping(patch: &Patch) -> bool {
    root_key(patch).is_some_and(|key| key.starts_with(BOOKKEEPING_PREFIX))
}

fn kind_of(doc: &AutoCommit, version: &Version) -> Kind {
    kind_in(doc.committed_document(), version)
}

/// What `version` was, read from the change's own operations: its writes to
/// [`UNDONE_KEY`] root keys, and whether it touches anything outside the
/// bookkeeping root keys. Never a diff of the document, so classifying a
/// change costs the same however large the document is.
fn kind_in(doc: &automerge::Automerge, version: &Version) -> Kind {
    use automerge::legacy::{Key, ObjectId, OpType};
    if tilo_doc::model::is_schema_genesis(version)
        || version
            .context
            .as_ref()
            .is_some_and(|context| !context.change.is_step())
    {
        return Kind::Bookkeeping;
    }
    let Some(change) = doc.get_change_by_hash(&version.hash) else {
        return Kind::Bookkeeping;
    };
    let change = change.decode();
    let start = change.start_op.get();
    let own = |id: &automerge::legacy::OpId| {
        id.actor() == &change.actor_id
            && id
                .counter()
                .checked_sub(start)
                .is_some_and(|at| usize::try_from(at).is_ok_and(|at| at < change.operations.len()))
    };
    // What the change wrote and took back again itself (a key put and
    // deleted, a value overwritten) is no effect of it.
    let superseded: HashSet<u64> = change
        .operations
        .iter()
        .filter(|op| !matches!(op.action, OpType::Increment(_)))
        .flat_map(|op| op.pred.iter())
        .filter(|pred| own(pred))
        .map(automerge::legacy::OpId::counter)
        .collect();
    let mut undone: Vec<String> = Vec::new();
    let mut redone: Vec<String> = Vec::new();
    let mut edits = false;
    // The root key each object the change writes into sits under.
    let mut roots: std::collections::HashMap<ObjectId, Option<String>> =
        std::collections::HashMap::new();
    for (counter, op) in (start..).zip(&change.operations) {
        let visible = match op.action {
            // A delete shows only where it removes what was there before.
            OpType::Delete => op.pred.iter().any(|pred| !own(pred)),
            _ => !superseded.contains(&counter),
        };
        if !visible {
            continue;
        }
        let root = match (&op.obj, &op.key) {
            (ObjectId::Root, Key::Map(key)) => match &op.action {
                // Native Redo is an intent on the canonical original, not a
                // new document patch whose inverse could bypass native
                // compensation.
                OpType::Put(value)
                    if key.starts_with(UNDONE_KEY)
                        && value.as_i64().is_some_and(|attempt| attempt > 0) =>
                {
                    return Kind::Bookkeeping;
                }
                OpType::Put(_) | OpType::Make(_) | OpType::Delete => {
                    let ledger = if matches!(op.action, OpType::Delete) {
                        &mut redone
                    } else {
                        &mut undone
                    };
                    if let Some(hash) = key.strip_prefix(UNDONE_KEY)
                        && !ledger.iter().any(|known| known == hash)
                    {
                        ledger.push(hash.to_owned());
                    }
                    Some(key.to_string())
                }
                // A counter or mark on the root names no key of its own.
                _ => None,
            },
            (ObjectId::Root, Key::Seq(_)) => None,
            (obj, _) => roots
                .entry(obj.clone())
                .or_insert_with(|| root_key_of(doc, obj))
                .clone(),
        };
        if !root.is_some_and(|key| key.starts_with(BOOKKEEPING_PREFIX)) {
            edits = true;
        }
    }
    // In the order the document keeps its root keys, as a reading of the
    // document lists them.
    undone.sort_unstable();
    redone.sort_unstable();
    if !undone.is_empty() {
        Kind::Undo(undone)
    } else if !redone.is_empty() {
        Kind::Redo(redone)
    } else if edits {
        Kind::Edit
    } else {
        Kind::Bookkeeping
    }
}

/// The root key the object `obj` sits under, if it is still in `doc`.
fn root_key_of(doc: &automerge::Automerge, obj: &automerge::legacy::ObjectId) -> Option<String> {
    let id = doc.import_obj(&obj.to_string()).ok()?;
    match doc.parents(&id).ok()?.last()?.prop {
        Prop::Map(key) => Some(key),
        Prop::Seq(_) => None,
    }
}

/// [`kind_in`] as a diff of the change against its dependencies reads it:
/// the reference its reading of the change's own operations must equal.
#[cfg(test)]
fn kind_by_diff(doc: &automerge::Automerge, version: &Version) -> Kind {
    if tilo_doc::model::is_schema_genesis(version)
        || version
            .context
            .as_ref()
            .is_some_and(|context| !context.change.is_step())
    {
        return Kind::Bookkeeping;
    }
    let deps = doc
        .get_change_meta_by_hash(&version.hash)
        .map(|meta| meta.deps.clone())
        .unwrap_or_default();
    let patches = doc.diff(&deps, &[version.hash]);
    let ledger = |put: bool| -> Vec<String> {
        patches
            .iter()
            .filter(|patch| patch.path.is_empty())
            .filter_map(|patch| match (&patch.action, put) {
                (PatchAction::PutMap { key, .. }, true)
                | (PatchAction::DeleteMap { key }, false) => {
                    key.strip_prefix(UNDONE_KEY).map(str::to_owned)
                }
                _ => None,
            })
            .collect()
    };
    if patches.iter().any(|patch| {
        matches!(&patch.action,
        PatchAction::PutMap { key, value, .. }
            if patch.path.is_empty() && key.starts_with(UNDONE_KEY)
                && value.0.as_i64().is_some_and(|attempt| attempt > 0))
    }) {
        // Native Redo is an intent on the canonical original, not a new
        // document patch whose inverse could bypass native compensation.
        return Kind::Bookkeeping;
    }
    let undone = ledger(true);
    if !undone.is_empty() {
        return Kind::Undo(undone);
    }
    let redone = ledger(false);
    if !redone.is_empty() {
        return Kind::Redo(redone);
    }
    if patches.iter().any(|patch| !is_bookkeeping(patch)) {
        Kind::Edit
    } else {
        Kind::Bookkeeping
    }
}

/// The changes currently undone, by full hash.
pub fn undone(doc: &AutoCommit) -> HashSet<String> {
    doc.keys(automerge::ROOT)
        .filter_map(|key| {
            let hash = key.strip_prefix(UNDONE_KEY)?;
            let (value, _) = doc.get(automerge::ROOT, &key).ok()??;
            (value.as_bool() == Some(true) || value.as_i64().is_some_and(|attempt| attempt > 0))
                .then(|| hash.to_owned())
        })
        .collect()
}

/// Recorded outside inverses which have not confirmed completion. This is
/// descriptive history data; callers must verify the original action too.
pub fn pending_compensations(doc: &AutoCommit) -> Vec<ChangeHash> {
    doc.keys(automerge::ROOT)
        .filter_map(|key| {
            let hash = key.strip_prefix(UNDONE_KEY)?.parse().ok()?;
            let (value, _) = doc.get(automerge::ROOT, &key).ok()??;
            value.as_i64().filter(|attempt| *attempt != 0).map(|_| hash)
        })
        .collect()
}

/// The changes `undo` would take back for `scope`, newest first: its steps
/// that are in effect, at most `limit` of them. The changes of one gesture
/// (the keystrokes of a word) are one step.
fn undo_targets(doc: &mut AutoCommit, scope: &Scope, limit: usize) -> Vec<Version> {
    doc.get_heads();
    steps_in_effect(doc, &|version| scope.owns(version), limit)
        .into_iter()
        .flatten()
        .collect()
}

/// What [`StepIndex`] keeps of a change: what telling its steps apart needs
/// (its author, time, run, invoker, gesture and change kind). The rest of
/// its entry (what it did, the action it ran, its resource) stays in the
/// document and is read again for the steps an undo or redo takes
/// ([`StepIndex::full`]): an index of a long history holds a few dozen bytes
/// per change, not the whole entry.
struct Step {
    hash: ChangeHash,
    /// Shared by every change of the same author.
    actor: std::rc::Rc<str>,
    time_ms: i64,
    /// Of its entry, when it has one.
    entry: Option<Box<StepEntry>>,
    /// The raw message of a change without an entry.
    message: Option<Box<str>>,
}

struct StepEntry {
    group: Option<tilo_actions::ActionGroup>,
    invoker: Option<tilo_actions::ActionInvoker>,
    gesture: Option<tilo_actions::ActionId>,
    kind: Option<tilo_actions::ActionKind>,
    change: tilo_actions::ChangeKind,
}

impl Step {
    fn of(version: Version, actor: std::rc::Rc<str>) -> Self {
        let entry = version.context.map(|context| {
            Box::new(StepEntry {
                group: context.group,
                invoker: context.invoker,
                gesture: context.gesture,
                kind: context.kind,
                change: context.change,
            })
        });
        Self {
            hash: version.hash,
            actor,
            time_ms: version.time_ms,
            message: version
                .message
                .filter(|_| entry.is_none())
                .map(String::into_boxed_str),
            entry,
        }
    }

    /// The change as a version holding what the index keeps of it.
    fn version(&self) -> Version {
        Version {
            hash: self.hash,
            actor: self.actor.to_string(),
            time_ms: self.time_ms,
            context: self
                .entry
                .as_deref()
                .map(|entry| tilo_actions::ActionContext {
                    group: entry.group.clone(),
                    invoker: entry.invoker.clone(),
                    gesture: entry.gesture,
                    kind: entry.kind,
                    change: entry.change,
                    ..tilo_actions::ActionContext::default()
                }),
            message: self.message.as_deref().map(str::to_owned),
        }
    }
}

/// A document's history read once and kept up to date: its changes in
/// causal order and what each one was to the steps of its author. A
/// change's kind never changes, so it is read once; new changes are read
/// as they arrive.
#[derive(Default)]
pub struct StepIndex {
    steps: Vec<Step>,
    actors: HashSet<std::rc::Rc<str>>,
    positions: std::collections::HashMap<ChangeHash, usize>,
    kinds: std::collections::HashMap<ChangeHash, Kind>,
    heads: Vec<ChangeHash>,
    /// The changes last read whole, newest last: an undo or redo asks for
    /// the same few again (its targets, their labels, what they brought
    /// back). Bounded, so the index stays small however long the history.
    read_whole: std::cell::RefCell<std::collections::VecDeque<Version>>,
}

/// How many changes [`StepIndex::full`] keeps read whole.
const KEPT_WHOLE: usize = 64;

impl StepIndex {
    /// Bring the index up to `doc`'s committed heads.
    pub fn refresh(&mut self, doc: &AutoCommit) {
        let heads = doc.base_heads();
        if heads == self.heads {
            return;
        }
        let document = doc.committed_document();
        let fresh = document.get_changes_meta(&self.heads);
        for meta in &fresh {
            if self.positions.contains_key(&meta.hash) {
                continue;
            }
            let version = tilo_resource::history::version_of(document, meta);
            let actor = if let Some(actor) = self.actors.get(version.actor.as_str()) {
                std::rc::Rc::clone(actor)
            } else {
                let actor: std::rc::Rc<str> = version.actor.as_str().into();
                self.actors.insert(std::rc::Rc::clone(&actor));
                actor
            };
            self.positions.insert(version.hash, self.steps.len());
            self.steps.push(Step::of(version, actor));
        }
        self.heads = heads;
    }

    /// How many changes the index holds.
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Whether the index holds no change.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// The hash of the change at `position` in causal order.
    pub fn hash(&self, position: usize) -> Option<ChangeHash> {
        self.steps.get(position).map(|step| step.hash)
    }

    /// The change at `position` with everything its entry records, read
    /// again from `doc`.
    pub fn full(&self, doc: &AutoCommit, position: usize) -> Option<Version> {
        let hash = self.steps.get(position)?.hash;
        let mut kept = self.read_whole.borrow_mut();
        if let Some(at) = kept.iter().position(|version| version.hash == hash) {
            let version = kept.remove(at)?;
            kept.push_back(version.clone());
            return Some(version);
        }
        let document = doc.committed_document();
        let meta = document.get_change_meta_by_hash(&hash)?;
        let version = tilo_resource::history::version_of(document, &meta);
        if kept.len() >= KEPT_WHOLE {
            kept.pop_front();
        }
        kept.push_back(version.clone());
        Some(version)
    }

    /// What the redo change `version` brought back, by full hash; empty
    /// for any other change ([`brought_back`], kept per change).
    pub fn brought_back(&mut self, doc: &AutoCommit, version: &Version) -> Vec<String> {
        match self.kind(doc, version) {
            Kind::Redo(hashes) => hashes,
            _ => Vec::new(),
        }
    }

    /// Where a change sits in causal order.
    pub fn position(&self, hash: &ChangeHash) -> Option<usize> {
        self.positions.get(hash).copied()
    }

    fn kind(&mut self, doc: &AutoCommit, version: &Version) -> Kind {
        if let Some(kind) = self.kinds.get(&version.hash) {
            return kind.clone();
        }
        let kind = kind_in(doc.committed_document(), version);
        self.kinds.insert(version.hash, kind.clone());
        kind
    }

    /// The steps in effect that `owns` claims, newest first, at most
    /// `limit`: each step the consecutive changes `step_of` puts together
    /// (one gesture), newest first. A step undone, or superseded by a redo
    /// that stands for it, is not in effect. What one engine undoes,
    /// whoever asks (Ctrl+Z, an agent's or a script's `undo`).
    pub fn steps(
        &mut self,
        doc: &AutoCommit,
        owns: &dyn Fn(&Version) -> bool,
        step_of: &dyn Fn(&Version) -> Option<String>,
        limit: usize,
    ) -> Vec<Vec<Version>> {
        self.refresh(doc);
        let undone = undone(doc);
        let mut superseded: HashSet<String> = HashSet::new();
        let mut steps: Vec<Vec<Version>> = Vec::new();
        let mut gesture = None;
        for index in (0..self.steps.len()).rev() {
            let Some(version) = self.steps.get(index).map(Step::version) else {
                continue;
            };
            if !owns(&version) {
                continue;
            }
            let hash = version.hash.to_string();
            match self.kind(doc, &version) {
                Kind::Undo(_) | Kind::Bookkeeping => continue,
                // Taken back again once what it brought back is undone.
                Kind::Redo(brought_back)
                    if brought_back.iter().all(|change| undone.contains(change)) =>
                {
                    continue;
                }
                // A redo stands for the steps it brought back: undoing it
                // again takes them back, so they are not steps of their own.
                Kind::Redo(brought_back) => superseded.extend(brought_back),
                Kind::Edit if superseded.contains(&hash) => continue,
                Kind::Edit => {}
            }
            if undone.contains(&hash) {
                gesture = None;
                continue;
            }
            let of = step_of(&version);
            let Some(full) = self.full(doc, index) else {
                continue;
            };
            if of.is_some()
                && of == gesture
                && let Some(step) = steps.last_mut()
            {
                step.push(full);
                continue;
            }
            if steps.len() >= limit {
                break;
            }
            gesture = of;
            steps.push(vec![full]);
        }
        steps
    }

    /// The undos in effect that `owns` claims, newest first, at most
    /// `limit`: what `redo` brings back. A newer step of the same owner ends
    /// them.
    pub fn undos(
        &mut self,
        doc: &AutoCommit,
        owns: &dyn Fn(&Version) -> bool,
        limit: usize,
    ) -> Vec<Version> {
        self.refresh(doc);
        let undone = undone(doc);
        let mut superseded: HashSet<String> = HashSet::new();
        let mut targets = Vec::new();
        for index in (0..self.steps.len()).rev() {
            if targets.len() >= limit {
                break;
            }
            let Some(version) = self.steps.get(index).map(Step::version) else {
                continue;
            };
            if !owns(&version) {
                continue;
            }
            let hash = version.hash.to_string();
            match self.kind(doc, &version) {
                Kind::Undo(took_back) => {
                    if took_back.iter().any(|change| undone.contains(change))
                        && let Some(version) = self.full(doc, index)
                    {
                        targets.push(version);
                    }
                }
                Kind::Redo(brought_back) => superseded.extend(brought_back),
                Kind::Bookkeeping => {}
                Kind::Edit if undone.contains(&hash) || superseded.contains(&hash) => {}
                Kind::Edit => break,
            }
        }
        targets
    }
}

pub fn steps_in_effect(
    doc: &AutoCommit,
    owns: &dyn Fn(&Version) -> bool,
    limit: usize,
) -> Vec<Vec<Version>> {
    StepIndex::default().steps(doc, owns, &gesture_of, limit)
}

/// The step a change recorded it belongs to.
pub fn gesture_of(version: &Version) -> Option<String> {
    version
        .context
        .as_ref()
        .and_then(|context| context.gesture)
        .map(|gesture| gesture.0.to_string())
}

/// What `undo` would take back for `scope` now: its last `limit` steps in
/// effect, newest first.
pub fn undo_preview(doc: &AutoCommit, scope: &Scope, limit: usize) -> Vec<Version> {
    steps_in_effect(doc, &|version| scope.owns(version), limit)
        .into_iter()
        .flatten()
        .collect()
}

/// The undo changes `redo` would revert for `scope`, newest first. A step
/// of the scope's own made after its undos ends them: redo never brings
/// back work over newer work.
fn redo_targets(doc: &mut AutoCommit, scope: &Scope, limit: usize) -> Vec<Version> {
    doc.get_heads();
    undos_in_effect(doc, &|version| scope.owns(version), limit)
}

/// The undos in effect that `owns` claims, newest first, at most `limit`
/// (see [`StepIndex::undos`]).
pub fn undos_in_effect(
    doc: &AutoCommit,
    owns: &dyn Fn(&Version) -> bool,
    limit: usize,
) -> Vec<Version> {
    StepIndex::default().undos(doc, owns, limit)
}

/// What one undo change took back, by full hash; empty for any other
/// change.
pub fn took_back(doc: &AutoCommit, version: &Version) -> Vec<String> {
    match kind_in(doc.committed_document(), version) {
        Kind::Undo(hashes) => hashes,
        _ => Vec::new(),
    }
}

/// What one redo change brought back, by full hash; empty for any other
/// change.
pub fn brought_back(doc: &AutoCommit, version: &Version) -> Vec<String> {
    match kind_in(doc.committed_document(), version) {
        Kind::Redo(hashes) => hashes,
        _ => Vec::new(),
    }
}

/// A one-line name of a step for a sentence.
pub fn step_label(version: &Version) -> String {
    let change = short(&version.hash);
    match version.context.as_ref() {
        Some(context) if context.what.is_some() => {
            format!("{} ({change})", super::sentence::sentence(context))
        }
        _ => change,
    }
}

/// The first twelve hex digits of a change hash: what history lists.
pub fn short(hash: &ChangeHash) -> String {
    hash.to_string().chars().take(12).collect()
}

fn revert(doc: &mut AutoCommit, targets: &[Version], verb: &str) -> Result<(), String> {
    let changes: Vec<(ChangeHash, String)> = targets
        .iter()
        .map(|version| (version.hash, version.actor.clone()))
        .collect();
    let outcomes = revert_changes(doc, &changes, &AuthorDirectory::default())
        .map_err(|error| format!("could not {verb}: {error:?}"))?;
    let kept: Vec<String> = targets
        .iter()
        .zip(&outcomes)
        .filter(|(_, outcome)| !outcome.is_complete())
        .map(|(version, outcome)| {
            let by: Vec<String> = outcome
                .blocked_by()
                .into_iter()
                .map(|author| author.name)
                .collect();
            format!(
                "{} (changed since by {})",
                step_label(version),
                by.join(", ")
            )
        })
        .collect();
    if !kept.is_empty() {
        return Err(format!(
            "Nothing was {verb}ne: somebody else changed what {} touched. Restore the items you mean from a version instead.",
            kept.join("; ")
        ));
    }
    Ok(())
}

/// The verified inverse action of a step, when it has one (it queued an
/// outside consequence): applied instead of reverting its patches. Supplied
/// by the caller's registry; it carries no authority of its own.
pub type Inverse = Box<dyn tilo_actions::Action>;

/// Take back `scope`'s last `limit` steps that are in effect, as one new
/// change in `doc`'s open transaction: each through the inverse `compensate`
/// gives for it, else by reverting its patches. Returns the steps, newest
/// first. On a refusal some of it may already be written: the caller
/// discards the transaction, as the action executor does for a refused
/// action.
///
/// # Errors
/// Says why nothing was undone: there is nothing of the scope's to undo,
/// somebody else has since changed what a step touched, or an inverse
/// cannot be applied.
pub fn undo(
    doc: &mut AutoCommit,
    scope: &Scope,
    limit: usize,
    compensate: impl FnMut(&Version) -> Result<Option<Inverse>, String>,
) -> Result<Vec<Version>, String> {
    let targets = undo_targets(doc, scope, limit);
    take_back(doc, scope, targets, compensate)
}

/// Take back every step of `scope`'s in effect made after the version
/// `heads`, as one new change: back to a checkpoint or a change from
/// history without counting steps. Returns the steps, newest first.
///
/// # Errors
/// As [`undo`].
pub fn undo_since(
    doc: &mut AutoCommit,
    scope: &Scope,
    heads: &[ChangeHash],
    compensate: impl FnMut(&Version) -> Result<Option<Inverse>, String>,
) -> Result<Vec<Version>, String> {
    let since: HashSet<ChangeHash> = doc
        .get_changes(heads)
        .iter()
        .map(|change| change.hash())
        .collect();
    let targets = undo_targets(doc, scope, usize::MAX)
        .into_iter()
        .filter(|version| since.contains(&version.hash))
        .collect();
    take_back(doc, scope, targets, compensate)
}

fn take_back(
    doc: &mut AutoCommit,
    scope: &Scope,
    targets: Vec<Version>,
    mut compensate: impl FnMut(&Version) -> Result<Option<Inverse>, String>,
) -> Result<Vec<Version>, String> {
    if targets.is_empty() {
        return Err(nothing_to_undo(doc, scope));
    }
    // Preparing every inversion before writing preserves one open Undo
    // transaction. Reading history between writes would auto-commit a
    // partial Undo and make Redo restore only the final step.
    let mut actions = Vec::new();
    let mut patches = Vec::new();
    for target in &targets {
        match compensate(target)? {
            Some(action) => actions.push((target.hash, action)),
            None => patches.push(target.clone()),
        }
    }
    if !patches.is_empty() {
        revert(doc, &patches, "undo")?;
    }
    let pending = apply_awaiting(doc, actions)?;
    for target in &targets {
        if pending.contains(&target.hash) {
            // The step stays in effect until the outside consequence its
            // inverse queued confirms it.
            tilo_actions::await_settlement(doc, &target.hash.to_string(), false)
                .map_err(|error| error.to_string())?;
        } else {
            doc.put(
                automerge::ROOT,
                format!("{UNDONE_KEY}{}", target.hash),
                ScalarValue::Boolean(true),
            )
            .map_err(|error| error.to_string())?;
        }
    }
    Ok(targets)
}

/// Apply each step's inverse (or restoration) inside a scope naming the
/// step: the steps whose action queued an outside consequence that settles
/// them.
fn apply_awaiting(
    doc: &mut AutoCommit,
    actions: Vec<(ChangeHash, Inverse)>,
) -> Result<HashSet<ChangeHash>, String> {
    let mut awaited = HashSet::new();
    for (hash, action) in actions {
        let compensating = tilo_actions::CompensatingScope::enter(vec![hash.to_string()]);
        action.apply(doc).map_err(|error| error.to_string())?;
        if compensating.awaited() {
            awaited.insert(hash);
        }
    }
    Ok(awaited)
}

/// Bring back the last `limit` undos of `scope`, as one new change: an
/// outside step through the action `restore` gives for it (its recorded
/// action, run again), any other by reverting the undo. A positive pending
/// marker keeps such a step undone until its restoration settles. Returns
/// the undo changes it brought back, newest first.
///
/// # Errors
/// Says why nothing was redone: no undo of the scope's is in effect, a
/// newer step of its own came after it, or a restoration cannot be applied.
pub fn redo(
    doc: &mut AutoCommit,
    scope: &Scope,
    limit: usize,
    mut restore: impl FnMut(&Version) -> Result<Option<Inverse>, String>,
) -> Result<Vec<Version>, String> {
    let targets = redo_targets(doc, scope, limit);
    if targets.is_empty() {
        return Err(format!(
            "{} nothing to redo: redo brings back an undo only while no newer step of the same author follows it.",
            scope.subject(*scope == Scope::invoking(doc))
        ));
    }
    let versions = tilo_resource::history::versions(doc);
    let mut patches = Vec::new();
    let mut actions = Vec::new();
    let mut originals = Vec::new();
    for target in &targets {
        let Kind::Undo(hashes) = kind_of(doc, target) else {
            return Err("redo target is not an Undo".into());
        };
        let mut outside = false;
        let mut ordinary = false;
        for hash in hashes {
            let original = versions
                .iter()
                .find(|version| version.hash.to_string() == hash)
                .ok_or("redo cannot resolve its original step")?;
            if let Some(action) = restore(original)? {
                outside = true;
                actions.push((original.hash, action));
                originals.push(original.hash);
            } else {
                ordinary = true;
            }
        }
        if outside && ordinary {
            return Err(
                "redo cannot mix outside restoration with document patches in one Undo".into(),
            );
        }
        if !outside {
            patches.push(target.clone());
        }
    }
    if !patches.is_empty() {
        revert(doc, &patches, "redo")?;
    }
    let awaited = apply_awaiting(doc, actions)?;
    for hash in originals {
        if awaited.contains(&hash) {
            tilo_actions::await_settlement(doc, &hash.to_string(), true)
                .map_err(|error| error.to_string())?;
        } else {
            doc.delete(automerge::ROOT, format!("{UNDONE_KEY}{hash}"))
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(targets)
}

/// A named version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub name: String,
    pub heads: Vec<ChangeHash>,
}

/// Name the document's current version `name`; naming another version
/// with a name in use moves the name.
///
/// # Errors
/// Refuses an empty or overlong name.
pub fn checkpoint(doc: &mut AutoCommit, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_CHECKPOINT_NAME {
        return Err(format!(
            "A checkpoint needs a name of 1 to {MAX_CHECKPOINT_NAME} characters."
        ));
    }
    let heads = doc.get_heads();
    let joined = heads
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let record = doc
        .put_object(
            automerge::ROOT,
            format!("{CHECKPOINT_KEY}{name}"),
            ObjType::Map,
        )
        .map_err(|error| error.to_string())?;
    doc.put(&record, "heads", joined)
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Every checkpoint, by name.
pub fn checkpoints(doc: &AutoCommit) -> Vec<Checkpoint> {
    let mut out: Vec<Checkpoint> = doc
        .keys(automerge::ROOT)
        .filter_map(|key| {
            let name = key.strip_prefix(CHECKPOINT_KEY)?.to_owned();
            let (_, record) = doc
                .get(automerge::ROOT, &key)
                .ok()
                .flatten()
                .filter(|(value, _)| matches!(value, automerge::Value::Object(ObjType::Map)))?;
            let joined = match doc.get(&record, "heads").ok().flatten()? {
                (automerge::Value::Scalar(value), _) => value.as_str()?.to_owned(),
                _ => return None,
            };
            let heads = joined
                .split(',')
                .map(|hash| hash.parse::<ChangeHash>().ok())
                .collect::<Option<Vec<_>>>()?;
            Some(Checkpoint { name, heads })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The heads `version` names: a checkpoint's name, or a change id from
/// history (at least six hex digits of it) — the document as of that change.
///
/// # Errors
/// Names what a version can be, and the checkpoints there are.
pub fn resolve(doc: &AutoCommit, version: &str) -> Result<Vec<ChangeHash>, String> {
    let version = version.trim();
    if let Some(found) = checkpoints(doc)
        .into_iter()
        .find(|checkpoint| checkpoint.name == version)
    {
        return Ok(found.heads);
    }
    let hex = version.to_ascii_lowercase();
    if hex.len() >= 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        let matching: Vec<ChangeHash> = tilo_resource::history::committed_versions(doc)
            .into_iter()
            .map(|version| version.hash)
            .filter(|hash| hash.to_string().starts_with(&hex))
            .collect();
        match matching.as_slice() {
            [only] => return Ok(vec![*only]),
            [] => {}
            _ => {
                return Err(format!(
                    "{version:?} names more than one change; give more of it."
                ));
            }
        }
    }
    let names: Vec<String> = checkpoints(doc)
        .into_iter()
        .map(|checkpoint| format!("{:?}", checkpoint.name))
        .collect();
    Err(format!(
        "There is no version {version:?}. Give a change id from history{}.",
        if names.is_empty() {
            String::new()
        } else {
            format!(" or a checkpoint: {}", names.join(", "))
        }
    ))
}

/// Whether `patch` is inside one of `ids`: an item key anywhere on its path
/// or the key it writes.
fn touches(patch: &Patch, ids: &[String]) -> bool {
    let on_path = patch
        .path
        .iter()
        .any(|(_, prop)| matches!(prop, Prop::Map(key) if ids.contains(key)));
    on_path
        || matches!(&patch.action,
            PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } if ids.contains(key))
}

/// An item a patch writes inside: a root key, and the entry key under it.
type Item = (String, Option<String>);

fn item_of(patch: &Patch) -> Option<Item> {
    let mut keys = patch
        .path
        .iter()
        .filter_map(|(_, prop)| match prop {
            Prop::Map(key) => Some(key.clone()),
            Prop::Seq(_) => None,
        })
        .chain(match &patch.action {
            PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => Some(key.clone()),
            _ => None,
        });
    Some((keys.next()?, keys.next()))
}

/// What `item` reads at `heads`, `None` when absent.
fn read_item(
    doc: &AutoCommit,
    item: &Item,
    heads: &[ChangeHash],
) -> Option<automerge::hydrate::Value> {
    let (mut value, mut id) = doc.get_at(automerge::ROOT, item.0.as_str(), heads).ok()??;
    if let (Some(entry), automerge::Value::Object(_)) = (&item.1, &value) {
        (value, id) = doc.get_at(&id, entry.as_str(), heads).ok()??;
    }
    match value {
        automerge::Value::Scalar(scalar) => {
            Some(automerge::hydrate::Value::Scalar(scalar.into_owned()))
        }
        automerge::Value::Object(_) => doc.hydrate(&id, Some(heads)).ok(),
    }
}

/// Whether `item` reads differently at the two versions. A diff is of
/// operations: a value written back reads the same by a different one.
fn differs(doc: &AutoCommit, item: &Item, from: &[ChangeHash], to: &[ChangeHash]) -> bool {
    read_item(doc, item, from) != read_item(doc, item, to)
}

/// Bring the document back to `heads` as one new change: all of it, or only
/// the items whose keys are `ids` (a mark, a layer, a field). Bookkeeping —
/// what is undone, the checkpoints — stays as it is. Returns how many
/// patches were written.
///
/// # Errors
/// Says when there is nothing to restore: the document already matches.
pub fn restore(
    doc: &mut AutoCommit,
    heads: &[ChangeHash],
    ids: &[String],
) -> Result<usize, String> {
    let current = doc.get_heads();
    let differing: HashSet<Item> = doc
        .diff(&current, heads)
        .iter()
        .filter_map(item_of)
        .filter(|item| differs(doc, item, &current, heads))
        .collect();
    let written = crate::state::write_revert_patches_where(doc, &current, heads, &|patch| {
        !is_bookkeeping(patch)
            && (ids.is_empty() || touches(patch, ids))
            && item_of(patch).is_some_and(|item| differing.contains(&item))
    });
    if written == 0 {
        return Err(if ids.is_empty() {
            "The document already matches that version.".into()
        } else {
            format!("{ids:?} already match that version, or no item has those ids.")
        });
    }
    Ok(written)
}

/// How one item differs between two versions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemChange {
    /// The item's place: a root key, or `map/id` for an entry of a keyed map.
    pub path: String,
    /// `added`, `removed` or `changed`.
    pub change: &'static str,
}

/// The items that differ between `from` and `to`, in document order,
/// bookkeeping left out.
pub fn changed_items(
    doc: &mut AutoCommit,
    from: &[ChangeHash],
    to: &[ChangeHash],
) -> Vec<ItemChange> {
    let mut out: Vec<ItemChange> = Vec::new();
    for patch in doc.diff(from, to) {
        if is_bookkeeping(&patch)
            || item_of(&patch).is_some_and(|item| !differs(doc, &item, from, to))
        {
            continue;
        }
        let mut keys = patch.path.iter().filter_map(|(_, prop)| match prop {
            Prop::Map(key) => Some(key.clone()),
            Prop::Seq(_) => None,
        });
        let (first, second) = (keys.next(), keys.next());
        let written = match &patch.action {
            PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => Some(key.clone()),
            _ => None,
        };
        let (path, change) = match (first, second, written) {
            (Some(map), None, Some(key)) if patch.path.len() == 1 => {
                let change = match &patch.action {
                    PatchAction::DeleteMap { .. } => "removed",
                    PatchAction::PutMap { value, .. }
                        if matches!(value.0, automerge::Value::Object(_))
                            && doc
                                .get_at(&patch.obj, key.as_str(), from)
                                .ok()
                                .flatten()
                                .is_none() =>
                    {
                        "added"
                    }
                    _ => "changed",
                };
                (format!("{map}/{key}"), change)
            }
            (Some(map), Some(item), _) => (format!("{map}/{item}"), "changed"),
            (Some(map), None, _) => (map, "changed"),
            (None, _, Some(key)) => (key, "changed"),
            (None, _, None) => continue,
        };
        match out.iter_mut().find(|item| item.path == path) {
            Some(item) if item.change == "changed" => item.change = change,
            Some(_) => {}
            None => out.push(ItemChange { path, change }),
        }
    }
    out
}

#[cfg(test)]
#[path = "steps_tests.rs"]
mod tests;
