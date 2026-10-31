//! The versions of every resource, read from the room record's log (reflection, and what a
//! task run did: task-legibility.md §2): each package action's change is a version of each
//! resource it made, changed or removed; each other step on the record (an undo, a revert) a
//! version of each resource it wrote.

use std::collections::{BTreeMap, HashMap};

use tilolang_runtime::ids::{AppId, DocId, TypeId};
use tilolang_runtime::store::{ANY_FIELD, Left, Version, VersionKind};

use crate::index as idx;
use crate::reads::changed_by;

type Doc = tilo_resource::Doc;

/// Add the versions each of `changes` made to `index`, by the resources it changed: who made
/// it, when (the time its action recorded, else the change's), and by which action.
///
/// A change that is no action of a package but someone's step on the record all the same (an
/// undo, a revert from History) is a version too, of each resource it wrote, as `mirror` (which
/// holds it) places what it wrote: who took back an agent's run has seen what they took back.
pub(super) fn note_versions<'c>(
    index: &mut Versions,
    changes: impl IntoIterator<Item = &'c automerge::Change>,
    mirror: &Doc,
    resources: &BTreeMap<DocId, (AppId, TypeId)>,
) {
    for change in changes {
        let made = idx::reflect::versions_of(change);
        let made = if made.is_empty() {
            stepped(change, mirror, resources)
        } else {
            made
        };
        for mut version in made {
            // A removed item stood where it was last placed.
            if version.kind == VersionKind::Removed {
                version.left = index.placed.get(&version.resource).cloned();
            }
            index
                .by_resource
                .entry(version.resource.clone())
                .or_default()
                .push(version);
        }
        for (item, holder, field, ty) in idx::reflect::placed_of(change) {
            let ty = ty.or_else(|| index.placed.get(&item).and_then(|left| left.ty.clone()));
            index.placed.insert(item, Left { holder, field, ty });
        }
    }
}

/// The versions of every resource, and where each item was last placed.
#[derive(Default)]
pub(crate) struct Versions {
    pub(crate) by_resource: HashMap<String, Vec<Version>>,
    placed: HashMap<String, Left>,
}

/// The versions a step on the record that is no action of a package made (an undo, a revert):
/// one per resource it wrote, by whoever took it.
fn stepped(
    change: &automerge::Change,
    mirror: &Doc,
    resources: &BTreeMap<DocId, (AppId, TypeId)>,
) -> Vec<Version> {
    let Some(context) = change
        .message()
        .and_then(tilo_actions::ActionContext::from_change_message)
    else {
        return Vec::new();
    };
    // A package's action made the versions it says, if any; an outside effect settling is
    // nobody's step.
    if context
        .action
        .as_ref()
        .is_some_and(|action| action.type_id == crate::action::ACTION_TYPE)
    {
        return Vec::new();
    }
    let Some(invoker) = context.invoker.as_ref() else {
        return Vec::new();
    };
    let at = tilolang::calendar::time_text(change.timestamp());
    let hash = change.hash().to_string();
    let changed = changed_by(std::slice::from_ref(change), mirror, resources);
    let mut by_resource: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for key in changed.docs.into_values().flatten() {
        let fields = by_resource.entry(key.resource).or_default();
        if key.field != ANY_FIELD && !fields.contains(&key.field) {
            fields.push(key.field);
        }
    }
    by_resource
        .into_iter()
        .map(|(resource, fields)| Version {
            who: invoker.actor.clone(),
            at: at.clone(),
            action: idx::reflect::STEP_ACTION.to_owned(),
            task: idx::reflect::task_of(&context),
            kind: tilolang_runtime::store::VersionKind::Changed,
            fields,
            resource,
            change: hash.clone(),
            left: None,
        })
        .collect()
}

/// The versions of no resource: what a store holds before it has any.
pub(super) static NO_VERSIONS: std::sync::LazyLock<HashMap<String, Vec<Version>>> =
    std::sync::LazyLock::new(HashMap::new);
