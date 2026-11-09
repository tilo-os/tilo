//! The versions of every resource, read from the workspace record's log (reflection, and what a
//! run did: task-legibility.md §2): each package action's change is a version of each
//! resource it made, changed or removed; each other step on the record (an undo, a revert) a
//! version of each resource it wrote.

use std::collections::{BTreeMap, HashMap};

use tilolang_runtime::ids::{AppId, DocId, TypeId};
use tilolang_runtime::store::{Left, Store as _, StoreError, Version, VersionKind};
use tilolang_runtime::value::ResourceRef;

use crate::index as idx;
use crate::index::derive::{LeftRow, Removed};
use crate::index::reflect::Stamp;

use super::{Inner, TiloStore, backend};

type Doc = tilo_resource::Doc;

/// A page of a document's resources with their newest versions, and whether it holds more: what
/// a window's mark counts ([`Inner::newest_page`]).
pub(super) type NewestPage = (Vec<(String, Version)>, bool);

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
) -> Removed {
    let undone = idx::reflect::undone_changes(mirror);
    for change in changes {
        let stamp = idx::reflect::change_stamp(change);
        // A change a step took back is no version of what the record holds.
        let counts = !undone.contains(&change.hash().to_string());
        let placed: BTreeMap<String, Left> = idx::reflect::placed_of(change)
            .into_iter()
            .map(|(item, holder, field, ty)| (item, Left { holder, field, ty }))
            .collect();
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
            note(index, &at, made, stamp.as_ref(), &placed, counts);
            // What was made from a resource is a change to it too: a comment under a task
            // changed that task, for whoever made or read it, though not one of its fields.
            if let Some(subject) = subject {
                let mut under = stood;
                under.kind = VersionKind::Changed;
                under.fields = Vec::new();
                under.placed = Vec::new();
                under.left = None;
                note(index, &subject, under, stamp.as_ref(), &placed, counts);
            }
        }
        for (item, left) in placed {
            let ty = left
                .ty
                .or_else(|| index.placed.get(&item).and_then(|held| held.ty.clone()));
            index.placed.insert(
                item,
                Left {
                    holder: left.holder,
                    field: left.field,
                    ty,
                },
            );
        }
    }
    removed(index)
}

/// The items the log removed from each collection, by the resource holding the collection and
/// its field, each with the version that removed it: what a list shows of them struck until the
/// person has seen them go ([`idx::REMOVED`]).
fn removed(index: &Versions) -> Removed {
    let mut removed: Removed = BTreeMap::new();
    for (resource, (_, version)) in &index.removed {
        let Some(left) = version.left.as_ref() else {
            continue;
        };
        removed
            .entry((left.holder.clone(), left.field.clone()))
            .or_default()
            .push(LeftRow {
                path: resource.clone(),
                version: version.clone(),
            });
    }
    removed
}

/// Note `version` of the resource `at`, which is the resource whose history it stands in: what
/// it is a version of, else the resource it was made from and so changed.
fn note(
    index: &mut Versions,
    at: &str,
    mut version: Version,
    stamp: Option<&Stamp>,
    placed: &BTreeMap<String, Left>,
    counts: bool,
) {
    // A removed item stood where it was last placed.
    let removed = version.kind == VersionKind::Removed;
    if removed {
        version.left = index.placed.get(&version.resource).cloned();
    } else if let Some(left) = placed.get(&version.resource) {
        version.left = Some(left.clone());
    }
    index
        .by_resource
        .entry(at.to_owned())
        .or_default()
        .push(version.clone());
    // The removal the resource ends at: of two, the later by the order versions keep, which
    // the index keeps too, so both pick the same one however the record reached them. A
    // removal of an item the index already had out leaves the earlier one standing.
    let Some(stamp) = stamp.filter(|_| removed && counts && version.left.is_some()) else {
        return;
    };
    match index.removed.get(&version.resource) {
        Some((held, _)) if !held.before(stamp) => {}
        _ => drop(
            index
                .removed
                .insert(version.resource.clone(), (stamp.clone(), version)),
        ),
    }
}

/// The versions of every resource, the last removal of each, and where each item was last
/// placed.
#[derive(Default)]
pub(crate) struct Versions {
    pub(crate) by_resource: HashMap<String, Vec<Version>>,
    removed: HashMap<String, (Stamp, Version)>,
    placed: HashMap<String, Left>,
}

/// The versions of no resource: what a store holds before it has any.
pub(super) static NO_VERSIONS: std::sync::LazyLock<HashMap<String, Vec<Version>>> =
    std::sync::LazyLock::new(HashMap::new);

impl Inner {
    /// The items that left the collection `field` of `at` (of items of the type `item`), in the
    /// order they left, each with the version that removed it ([`idx::REMOVED`]): what a list
    /// shows of them struck until the person has seen them go; `None` when no index of its
    /// package is read from yet.
    pub(super) fn removed_in(
        &self,
        at: &ResourceRef,
        field: &str,
        item: &TypeId,
    ) -> Option<Result<Vec<(String, Version)>, StoreError>> {
        let resource = at.id_string();
        let (app, _) = self.locate(at).ok()?;
        let view = self.view(&app)?;
        let read = || {
            let rows = view.removed(&resource, field).map_err(backend)?;
            Ok(rows
                .into_iter()
                .filter(|(_, version)| {
                    version
                        .left
                        .as_ref()
                        .is_some_and(|left| left.ty.as_ref().is_none_or(|ty| ty == item))
                })
                .collect())
        };
        Some(read())
    }

    /// The newest version of `at`, as the index keeps it ([`idx::Snapshot::newest`]); `None`
    /// when no index of its package is read from yet.
    pub(super) fn newest(&self, at: &ResourceRef) -> Option<Result<Option<Version>, StoreError>> {
        let (app, _) = self.index.get(&at.doc)?;
        let view = self.view(app)?;
        Some(view.newest(&at.id_string()).map_err(backend))
    }

    /// The version that last changed the field `field` of the resource `at`, as the index keeps
    /// it; `None` when no index of its package is read from yet. A field no version changed
    /// since the resource was made was last changed by the one that made it, which set it.
    pub(super) fn field_changed(
        &self,
        at: &ResourceRef,
        field: &str,
    ) -> Option<Result<Option<Version>, StoreError>> {
        let (app, _) = self.index.get(&at.doc)?;
        let view = self.view(app)?;
        let read = |field: &str| {
            view.field(&at.id_string(), field)
                .map_err(backend)
                .map(|held| {
                    held.as_ref()
                        .and_then(idx::reflect::Stamp::of_value)
                        .map(|stamp| stamp.version)
                })
        };
        Some(match read(&idx::reflect::changed_field(field)) {
            Ok(None) => read(idx::reflect::CREATED),
            held => held,
        })
    }

    /// The newest version of each resource of the document `doc` (it and its items), a page of
    /// them at most, and whether it holds more, as the index keeps them
    /// ([`idx::Snapshot::newest_page`]); `None` when no index of its package is read from yet.
    pub(super) fn newest_page(
        &self,
        doc: &DocId,
        page: usize,
    ) -> Option<Result<NewestPage, StoreError>> {
        let (app, _) = self.index.get(doc)?;
        let view = self.view(app)?;
        Some(view.newest_page(doc, page).map_err(backend))
    }
}

/// The newest version of `at` ([`Inner::newest`]), else the record's log answers it: what a
/// mark of a resource reads without the resource's log once its package's index is open.
pub(super) fn newest(store: &TiloStore, at: &ResourceRef) -> Result<Option<Version>, StoreError> {
    if let Some(answered) = store.held().newest(at) {
        return answered;
    }
    // No index answers yet: the log does.
    Ok(store.versions(at)?.pop())
}

/// The newest version of each resource of the document `doc` ([`Inner::newest_page`]), else the
/// record's log answers it: a page of them, and whether there are more.
pub(super) fn newest_page(
    store: &TiloStore,
    doc: &DocId,
    page: usize,
) -> Result<NewestPage, StoreError> {
    // The store is let go before the log answers: answering from it reads the store's versions,
    // which no read of the store may be held across.
    let answered = store.held().newest_page(doc, page);
    match answered {
        Some(answered) => answered,
        // No index answers yet: the log does.
        None => tilolang_runtime::store::newest_page_of_log(store, doc, page),
    }
}

/// The items that left the collection `field` of `holder` ([`Inner::removed_in`]), else the
/// record's log answers it.
pub(super) fn removed_in(
    store: &TiloStore,
    holder: &ResourceRef,
    field: &str,
    item: &TypeId,
) -> Result<Vec<(String, Version)>, StoreError> {
    // The store is let go before the log answers: answering from it reads the store's versions,
    // which no read of the store may be held across.
    let answered = store.held().removed_in(holder, field, item);
    match answered {
        Some(answered) => answered,
        // No index answers yet: the log does.
        None => tilolang_runtime::store::removed_of_log(store, holder, field, item),
    }
}
