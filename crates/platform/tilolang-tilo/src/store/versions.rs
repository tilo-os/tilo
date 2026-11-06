//! The versions of every resource, read from the workspace record's log (reflection, and what a
//! task run did: task-legibility.md §2): each package action's change is a version of each
//! resource it made, changed or removed; each other step on the record (an undo, a revert) a
//! version of each resource it wrote.

use std::collections::{BTreeMap, HashMap};

use tilolang_runtime::ids::{AppId, DocId, TypeId};
use tilolang_runtime::store::{Left, Version, VersionKind};

use crate::index as idx;

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
            idx::reflect::stepped(change, mirror, resources)
        } else {
            made
        };
        for made in made {
            let subject = made
                .from
                .as_ref()
                .map(tilolang_runtime::value::ResourceRef::id_string);
            let stood = made.clone();
            let at = made.resource.clone();
            note(index, &at, made);
            // What was made from a resource is a change to it too: a comment under a task
            // changed that task, for whoever made or read it, though not one of its fields.
            if let Some(subject) = subject {
                let mut under = stood;
                under.kind = VersionKind::Changed;
                under.fields = Vec::new();
                under.placed = Vec::new();
                under.left = None;
                note(index, &subject, under);
            }
        }
        for (item, holder, field, ty) in idx::reflect::placed_of(change) {
            let ty = ty.or_else(|| index.placed.get(&item).and_then(|left| left.ty.clone()));
            index.placed.insert(item, Left { holder, field, ty });
        }
    }
}

/// Note `version` of the resource `at`, which is the resource whose history it stands in: what
/// it is a version of, else the resource it was made from and so changed.
fn note(index: &mut Versions, at: &str, mut version: Version) {
    // A removed item stood where it was last placed.
    if version.kind == VersionKind::Removed {
        version.left = index.placed.get(&version.resource).cloned();
    }
    index
        .by_resource
        .entry(at.to_owned())
        .or_default()
        .push(version);
}

/// The versions of every resource, and where each item was last placed.
#[derive(Default)]
pub(crate) struct Versions {
    pub(crate) by_resource: HashMap<String, Vec<Version>>,
    placed: HashMap<String, Left>,
}

/// The versions of no resource: what a store holds before it has any.
pub(super) static NO_VERSIONS: std::sync::LazyLock<HashMap<String, Vec<Version>>> =
    std::sync::LazyLock::new(HashMap::new);
