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
//! undone change, written in the same change as the undo — so a redo, which
//! reverts that undo change, removes them again by the same inversion.

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
pub const UNDONE_KEY: &str = "tilo:undone:";
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

    fn owns(&self, version: &Version) -> bool {
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

    fn subject(&self) -> &'static str {
        match self {
            Self::Run(_) => "This agent run has",
            Self::Invoker(_) | Self::Actor(_) => "You have",
        }
    }
}

/// What one change was to the steps of its scope.
enum Kind {
    /// An undo: the changes it took back.
    Undo(Vec<String>),
    /// A redo: the changes whose undo it reverted.
    Redo(Vec<String>),
    /// An edit of the content.
    Edit,
    /// Nothing a person did to the content: a checkpoint, a schema genesis.
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

fn kind_of(doc: &mut AutoCommit, version: &Version) -> Kind {
    if tilo_doc::model::is_schema_genesis(version)
        || version
            .context
            .as_ref()
            .is_some_and(ActionContext::is_housekeeping)
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
        .filter_map(|key| key.strip_prefix(UNDONE_KEY).map(str::to_owned))
        .collect()
}

/// The steps `undo` would take back for `scope`, newest first: its changes
/// that are in effect, at most `limit` of them.
fn undo_targets(doc: &mut AutoCommit, scope: &Scope, limit: usize) -> Vec<Version> {
    let versions = tilo_resource::history::versions(doc);
    let undone = undone(doc);
    let mut superseded: HashSet<String> = HashSet::new();
    let mut targets = Vec::new();
    for version in versions.iter().rev().filter(|version| scope.owns(version)) {
        if targets.len() >= limit {
            break;
        }
        let hash = version.hash.to_string();
        match kind_of(doc, version) {
            Kind::Undo(_) | Kind::Bookkeeping => continue,
            // A redo stands for the steps it brought back: undoing it again
            // takes them back, so they are not steps of their own any more.
            Kind::Redo(brought_back) => superseded.extend(brought_back),
            Kind::Edit if superseded.contains(&hash) => continue,
            Kind::Edit => {}
        }
        if !undone.contains(&hash) {
            targets.push(version.clone());
        }
    }
    targets
}

/// The undo changes `redo` would revert for `scope`, newest first. A step
/// of the scope's own made after its undos ends them: redo never brings
/// back work over newer work.
fn redo_targets(doc: &mut AutoCommit, scope: &Scope, limit: usize) -> Vec<Version> {
    let versions = tilo_resource::history::versions(doc);
    let undone = undone(doc);
    let mut superseded: HashSet<String> = HashSet::new();
    let mut targets = Vec::new();
    for version in versions.iter().rev().filter(|version| scope.owns(version)) {
        if targets.len() >= limit {
            break;
        }
        let hash = version.hash.to_string();
        match kind_of(doc, version) {
            Kind::Undo(took_back) => {
                if took_back.iter().any(|change| undone.contains(change)) {
                    targets.push(version.clone());
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

/// A one-line name of a step for a sentence.
pub fn step_label(version: &Version) -> String {
    let change = short(&version.hash);
    match version.context.as_ref() {
        Some(context) if !context.action_label.is_empty() => {
            format!("{} ({change})", context.action_label)
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

/// Take back `scope`'s last `limit` steps that are in effect, as one new
/// change in `doc`'s open transaction. Returns the steps, newest first.
/// On a refusal some of it may already be written: the caller discards the
/// transaction, as the action executor does for a refused action.
///
/// # Errors
/// Says why nothing was undone: there is nothing of the scope's to undo, or
/// somebody else has since changed what a step touched.
pub fn undo(doc: &mut AutoCommit, scope: &Scope, limit: usize) -> Result<Vec<Version>, String> {
    let targets = undo_targets(doc, scope, limit);
    if targets.is_empty() {
        return Err(format!(
            "{} nothing in effect to undo. history lists every step and its run; undo with run takes back another run's steps.",
            scope.subject()
        ));
    }
    revert(doc, &targets, "undo")?;
    for target in &targets {
        let key = format!("{UNDONE_KEY}{}", target.hash);
        doc.put(automerge::ROOT, key, ScalarValue::Boolean(true))
            .map_err(|error| error.to_string())?;
    }
    Ok(targets)
}

/// Bring back the last `limit` undos of `scope`, as one new change.
/// Returns the undo changes it reverted, newest first.
///
/// # Errors
/// Says why nothing was redone: no undo of the scope's is in effect, or a
/// newer step of its own came after it.
pub fn redo(doc: &mut AutoCommit, scope: &Scope, limit: usize) -> Result<Vec<Version>, String> {
    let targets = redo_targets(doc, scope, limit);
    if targets.is_empty() {
        return Err(format!(
            "{} nothing to redo: redo brings back an undo only while no newer step of the same author follows it.",
            scope.subject()
        ));
    }
    revert(doc, &targets, "redo")?;
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
pub fn resolve(doc: &mut AutoCommit, version: &str) -> Result<Vec<ChangeHash>, String> {
    let version = version.trim();
    if let Some(found) = checkpoints(doc)
        .into_iter()
        .find(|checkpoint| checkpoint.name == version)
    {
        return Ok(found.heads);
    }
    let hex = version.to_ascii_lowercase();
    if hex.len() >= 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        let matching: Vec<ChangeHash> = tilo_resource::history::versions(doc)
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
