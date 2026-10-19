//! The published tree as a [`SharedResource`]: each version shares with the
//! last every item child the change did not touch, so a keystroke in a field
//! no item depends on neither builds nor copies an item, and what reads the
//! tree (bound controls, accessibility, the door) skips what it has seen.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use std::any::Any;

use tilo_resource::items::{ItemAction, ItemCollection};
use ui_core::automerge::AutoCommit;
use ui_core::published::{SharedChildren, SharedResource};

use super::super::rows::{LiveRow, join, outside_document};
use super::typed::ResourceType;
use super::{
    Decoration, ResourceRoot, mark_document_jobs, offer_document_reads, offer_item_open, open_spec,
};

/// The document-owned tree at one live revision, locale and
/// [`Resource::publication_cache_epoch`].
pub(super) struct SharedTreeCache {
    pub(super) revision: (u64, u64, String),
    pub(super) tree: SharedResource,
}

/// The item children last published: the collections they were built from,
/// at one locale (their actions' labels are localised).
pub(super) struct ItemChildren {
    i18n: u64,
    collections: Vec<ItemCollection>,
    children: SharedChildren,
}

/// The view operation types of the nested resources at one live revision,
/// and the children they were collected from.
pub(super) struct NestedViews {
    revision: u64,
    children: SharedChildren,
    views: Rc<HashSet<String>>,
}

/// The children of the publication with the records kept beside the
/// resource, at the records' revisions, one locale and whether `open` is
/// offered on items.
pub(super) struct RecordChildren {
    document: SharedChildren,
    records: Vec<(String, u64)>,
    i18n: u64,
    opens: bool,
    children: SharedChildren,
}

/// The document children as last published, after what the publication adds
/// to each child: `open` on items a view opens, and document actions queued
/// as jobs. Kept so the next publication adds it only to the children that
/// changed.
pub(super) struct PublishedChildren {
    /// What was added: (`open` offered, document actions queued).
    adds: (bool, bool),
    document: SharedChildren,
    published: SharedChildren,
}

/// The children of the publication with the live rows the surfaces show
/// merged in: each group of rows under one id (merged into the document
/// child of that id, or appended), with what it published. A group whose
/// rows are the very ones the surfaces handed out last time, over the same
/// document child, is that very node again.
pub(super) struct RowChildren {
    /// What was added: (`open` offered, document actions queued).
    adds: (bool, bool),
    groups: HashMap<String, RowGroup>,
    children: SharedChildren,
}

/// One id's rows and the child they published.
pub(super) struct RowGroup {
    /// The document child they were merged into, if any.
    base: Option<SharedResource>,
    rows: Vec<LiveRow>,
    published: SharedResource,
}

impl RowGroup {
    fn holds(&self, base: Option<&SharedResource>, rows: &[LiveRow]) -> bool {
        let same_base = match (&self.base, base) {
            (Some(was), Some(now)) => was.ptr_eq(now),
            (None, None) => true,
            _ => false,
        };
        same_base
            && self.rows.len() == rows.len()
            && self
                .rows
                .iter()
                .zip(rows)
                .all(|(was, now)| was.same_row(now))
    }
}

impl ResourceRoot {
    /// The document-owned tree of `state`: [`tilo_resource::publish`],
    /// sharing every item child whose collection, id and label are the ones
    /// it was built with. Cached per live revision when `state` is the live
    /// state and the resource opts in ([`Resource::publication_cache_epoch`]);
    /// a version the view has not synced yet is built afresh (sharing the
    /// items all the same).
    pub(super) fn shared_document_tree(&self, state: &dyn Any) -> SharedResource {
        let live = std::ptr::addr_eq(Rc::as_ptr(&self.typed.state()), state);
        let Some(epoch) = self.typed.cache_epoch(state).filter(|_| live) else {
            return self.build_document_tree(state, live);
        };
        let revision = (
            self.typed.revision(),
            ui_core::i18n::I18nManager::revision(),
            epoch,
        );
        if let Some(cached) = self
            .publication_tree
            .borrow()
            .as_ref()
            .filter(|cached| cached.revision == revision)
        {
            return cached.tree.clone();
        }
        let tree = self.build_document_tree(state, true);
        *self.publication_tree.borrow_mut() = Some(SharedTreeCache {
            revision,
            tree: tree.clone(),
        });
        tree
    }

    /// The document-owned tree of `state`; `live` when it is the live
    /// state, whose items the next version shares.
    fn build_document_tree(&self, state: &dyn Any, live: bool) -> SharedResource {
        let _profile = tilo_resource::profiling::Phase::start("framework.resource_tree");
        let parts = self.typed.children_parts(state);
        let items = self.item_children(parts.items, &parts.item_actions, live, state);
        let children = if parts.others.is_empty() {
            items
        } else {
            SharedChildren::new(
                parts
                    .others
                    .into_iter()
                    .map(|mut child| {
                        if self.resource.readable {
                            offer_document_reads(&mut child, &|id| {
                                self.typed.document_child(state, id)
                            });
                        }
                        SharedResource::from_resource(child)
                    })
                    .chain(items.iter().cloned())
                    .collect(),
            )
        };
        let head = self
            .typed
            .head(self.dispatch.resource_id(), &self.label, state);
        SharedResource::new(head, children)
    }

    /// One child per item of `collections` carrying `actions`
    /// ([`tilo_resource::items::item_children`]), reusing each child built
    /// for the last kept collections whose label is unchanged. `keep`
    /// remembers these for the next version: the live state's do, a version
    /// the view has not synced yet does not displace them.
    fn item_children(
        &self,
        collections: Vec<ItemCollection>,
        actions: &[ItemAction],
        keep: bool,
        state: &dyn Any,
    ) -> SharedChildren {
        let i18n = ui_core::i18n::I18nManager::revision();
        let mut memo = self.item_children.borrow_mut();
        if memo.as_ref().is_some_and(|last| last.i18n != i18n) {
            *memo = None;
        }
        if let Some(last) = memo.as_ref().filter(|last| last.collections == collections) {
            return last.children.clone();
        }
        let children = build_items(
            &self.resource,
            memo.as_ref(),
            &collections,
            actions,
            &|id| self.typed.document_child(state, id),
        );
        if keep {
            *memo = Some(ItemChildren {
                i18n,
                collections,
                children: children.clone(),
            });
        }
        children
    }

    /// The view operation types of the nested resources of the live state.
    pub(super) fn nested_views(&self) -> Rc<HashSet<String>> {
        let revision = self.typed.revision();
        if let Some(pinned) = self
            .nested_views
            .borrow()
            .as_ref()
            .filter(|pinned| pinned.revision == revision)
        {
            return Rc::clone(&pinned.views);
        }
        let tree = self.shared_document_tree(&*self.typed.state());
        self.keep_nested_views(revision, tree.children())
    }

    /// Answer the nested views of `state`, a version the view has not
    /// synced yet, until the live revision moves.
    pub(super) fn pin_nested_views(&self, state: &dyn Any) {
        let tree = self.shared_document_tree(state);
        self.keep_nested_views(self.typed.revision(), tree.children());
    }

    fn keep_nested_views(&self, revision: u64, children: &SharedChildren) -> Rc<HashSet<String>> {
        let mut kept = self.nested_views.borrow_mut();
        let views = match kept.as_ref() {
            Some(last) if last.children.ptr_eq(children) => Rc::clone(&last.views),
            _ => Rc::new(nested_view_types(children.as_slice())),
        };
        *kept = Some(NestedViews {
            revision,
            children: children.clone(),
            views: Rc::clone(&views),
        });
        views
    }

    /// [`ui_core::resources::PublishesResources::resources_for`] as a shared
    /// forest, when it is one: nothing the view adds touches the tree's
    /// children beyond what [`Self::published_children`] keeps per child (no
    /// surface, which alone publishes rows or binds edits to them, and no
    /// content resource naming the root), and the record reads as the
    /// resource. `None` otherwise, and for a participant's view, which is
    /// never kept.
    pub(super) fn shared_resources(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
    ) -> Option<Vec<SharedResource>> {
        if participant.is_some() || self.typed.unreadable().is_some() {
            return None;
        }
        let surface = self.has_surface();
        let current = if self.typed.follows(doc) {
            None
        } else {
            Some(self.typed.read(doc).ok()?)
        };
        let live = self.typed.state();
        let state: &dyn Any = &**current.as_ref().unwrap_or(&live);
        let opens = self.stages.opens();
        let root = {
            let _profile = tilo_resource::profiling::Phase::start("framework.publication");
            let tree = self.shared_document_tree(state);
            if current.is_some() {
                // As `current_publication`: nested views follow this state
                // until the view syncs to it.
                self.keep_nested_views(self.typed.revision(), tree.children());
            }
            let mut head =
                self.decorate_publication(state, tree.head().clone(), Decoration::Actions);
            let document = self.published_children(tree.children(), opens);
            let document = if surface {
                self.with_live_rows(tree.children(), document, opens)
            } else {
                document
            };
            let children = self.with_record_children(document, opens);
            if opens && !head.actions.iter().any(|existing| existing.id == "open") {
                head = head.with_action(open_spec());
            }
            (head, children)
        };
        let mut extras = Vec::new();
        for extra in ui_core::resources::resources_for(self.content.as_ref(), None, doc) {
            if extra.id.as_str() == root.0.id.as_str() {
                return None;
            }
            // A hosted surface has already admitted what it adds.
            let extra = if surface {
                extra
            } else {
                outside_document(extra)
            };
            extras.push(SharedResource::from_resource(extra));
        }
        let mut resources = vec![SharedResource::new(root.0, root.1)];
        resources.append(&mut extras);
        Some(resources)
    }

    /// `document`, the document-owned children, as the publication carries
    /// them: with `open` offered on the items when a view `opens` them, and
    /// document actions queued as jobs when the app drains effects causally,
    /// as [`Self::decorate_publication`] and `resources_for` add them to the
    /// whole tree. A child the last publication carried is that very node.
    fn published_children(&self, document: &SharedChildren, opens: bool) -> SharedChildren {
        let adds = (opens, self.typed.has_causal_drains());
        if adds == (false, false) {
            return document.clone();
        }
        let mut kept = self.published_children.borrow_mut();
        let last = kept.as_ref().filter(|last| last.adds == adds);
        if let Some(last) = last.filter(|last| last.document.ptr_eq(document)) {
            return last.published.clone();
        }
        let before: HashMap<&str, (&SharedResource, &SharedResource)> = last
            .map(|last| {
                last.document
                    .iter()
                    .zip(last.published.iter())
                    .map(|(document, published)| (document.id(), (document, published)))
                    .collect()
            })
            .unwrap_or_default();
        let published = SharedChildren::new(
            document
                .iter()
                .map(|child| {
                    before
                        .get(child.id())
                        .filter(|(document, _)| document.ptr_eq(child))
                        .map_or_else(
                            || {
                                let mut resource = child.to_resource();
                                if adds.1 {
                                    mark_document_jobs(&mut resource);
                                }
                                if adds.0 {
                                    offer_item_open(self.resource.item_of, &mut resource);
                                }
                                SharedResource::from_resource(resource)
                            },
                            |(_, published)| (*published).clone(),
                        )
                })
                .collect(),
        );
        *kept = Some(PublishedChildren {
            adds,
            document: document.clone(),
            published: published.clone(),
        });
        published
    }

    /// `document` followed by the records kept beside the resource, as the
    /// publication carries them (with `open` on their items when a view
    /// `opens` them). The same children while neither moved, so what reads
    /// the forest skips them as seen.
    fn with_record_children(&self, document: SharedChildren, opens: bool) -> SharedChildren {
        let records = self.storage.records_key();
        if records.is_empty() {
            return document;
        }
        let i18n = ui_core::i18n::I18nManager::revision();
        let mut kept = self.record_children.borrow_mut();
        if let Some(kept) = kept.as_ref().filter(|kept| {
            kept.document.ptr_eq(&document)
                && kept.records == records
                && kept.i18n == i18n
                && kept.opens == opens
        }) {
            return kept.children.clone();
        }
        let children = SharedChildren::new(
            document
                .iter()
                .cloned()
                .chain(
                    self.storage
                        .record_resources()
                        .into_iter()
                        .map(|mut record| {
                            if opens {
                                offer_item_open(self.resource.item_of, &mut record);
                            }
                            SharedResource::from_resource(record)
                        }),
                )
                .collect(),
        );
        *kept = Some(RecordChildren {
            document,
            records,
            i18n,
            opens,
            children: children.clone(),
        });
        children
    }

    /// Whether the view holds a hosted surface of this resource.
    fn has_surface(&self) -> bool {
        let mut found = false;
        self.surfaces(&mut |_| found = true);
        found
    }

    /// `document`, the published document children, with the live rows the
    /// surfaces show merged in by id, as [`super::super::rows::merge`] merges
    /// them into the whole tree, and the edits surfaces bind to rows bound
    /// on them. A row joins `raw`, the document child of its id as the
    /// document has it, which is then decorated as `document`'s children
    /// are: decorating first would give an item the framework's own view
    /// beside the row's, where the row's views replace it (`open` on items
    /// keeps an app's explicit views). A row a surface hands out again (the same [`LiveRow`], kept
    /// while what it shows holds) over the same document child publishes
    /// the node it published last time, so what reads the forest skips it
    /// as seen; only a surface that binds edits to rows
    /// ([`super::super::ResourceSurface::ROW_DECLARATIONS`]) has its rows
    /// bound afresh.
    fn with_live_rows(
        &self,
        raw: &SharedChildren,
        document: SharedChildren,
        opens: bool,
    ) -> SharedChildren {
        let _profile = tilo_resource::profiling::Phase::start("framework.live_rows");
        let mut rows = Vec::new();
        let mut binds = false;
        self.surfaces(&mut |surface| {
            rows.extend(surface.live_rows());
            binds |= surface.binds_rows();
        });
        if rows.is_empty() && !binds {
            *self.row_children.borrow_mut() = None;
            return document;
        }
        let adds = (opens, self.typed.has_causal_drains());
        let last = self
            .row_children
            .borrow_mut()
            .take()
            .filter(|last| last.adds == adds);
        // Rows by id, in the order merging meets them: those naming a
        // document child join it, the rest are appended in order, a later
        // row of an appended id joining the first.
        let at: HashMap<&str, usize> = document
            .iter()
            .enumerate()
            .map(|(index, child)| (child.id(), index))
            .collect();
        let mut order: Vec<String> = Vec::new();
        let mut by_id: HashMap<String, Vec<LiveRow>> = HashMap::new();
        for row in rows {
            let id = row.id().to_owned();
            by_id
                .entry(id.clone())
                .or_insert_with(|| {
                    order.push(id);
                    Vec::new()
                })
                .push(row);
        }
        let mut children: Vec<SharedResource> = document.iter().cloned().collect();
        let mut groups = HashMap::with_capacity(order.len());
        for id in order {
            let Some(rows) = by_id.remove(&id) else {
                continue;
            };
            let target = at.get(id.as_str()).copied();
            // `document` holds `raw`'s children one for one, decorated.
            let base = target.and_then(|index| raw.as_slice().get(index));
            let published = last
                .as_ref()
                .and_then(|last| last.groups.get(&id))
                .filter(|group| group.holds(base, &rows))
                .map_or_else(
                    || self.publish_rows(base, &rows, adds),
                    |group| group.published.clone(),
                );
            match target.and_then(|index| children.get_mut(index)) {
                Some(child) => *child = published.clone(),
                None => children.push(published.clone()),
            }
            groups.insert(
                id,
                RowGroup {
                    base: base.cloned(),
                    rows,
                    published,
                },
            );
        }
        if binds {
            self.bind_rows_shared(&mut children, adds);
        }
        #[cfg(debug_assertions)]
        if !binds {
            self.surfaces(&mut |surface| {
                for child in &children {
                    debug_assert!(
                        !surface.offers_on_row(child.id()),
                        "a surface binds edits on `{}` without `ResourceSurface::ROW_DECLARATIONS`",
                        child.id()
                    );
                }
            });
        }
        let children = match last {
            Some(last)
                if last.children.len() == children.len()
                    && last
                        .children
                        .iter()
                        .zip(&children)
                        .all(|(was, now)| was.ptr_eq(now)) =>
            {
                last.children
            }
            _ => SharedChildren::new(children),
        };
        *self.row_children.borrow_mut() = Some(RowChildren {
            adds,
            groups,
            children: children.clone(),
        });
        children
    }

    /// The child `rows`, all of one id, publish: joined into `base`, the
    /// document child of that id, or the first row with the rest joined
    /// into it; with `open` offered on items and document actions queued
    /// as jobs, as `adds` says.
    fn publish_rows(
        &self,
        base: Option<&SharedResource>,
        rows: &[LiveRow],
        adds: (bool, bool),
    ) -> SharedResource {
        let mut rows = rows.iter().cloned().map(LiveRow::into_resource);
        let mut resource = match base {
            Some(base) => base.to_resource(),
            None => match rows.next() {
                Some(first) => first,
                None => {
                    return SharedResource::from_resource(tilo_actions::Resource::new("", "", ""));
                }
            },
        };
        for row in rows {
            join(&mut resource, row);
        }
        self.decorate_child(&mut resource, adds);
        SharedResource::from_resource(resource)
    }

    /// What the publication adds to each child, as `adds` says: `open` on
    /// items a view opens, and document actions queued as jobs.
    fn decorate_child(&self, resource: &mut tilo_actions::Resource, adds: (bool, bool)) {
        if adds.1 {
            mark_document_jobs(resource);
        }
        if adds.0 {
            offer_item_open(self.resource.item_of, resource);
        }
    }

    /// Bind the edits surfaces offer on rows to each of `children` holding a
    /// row any surface offers on, anywhere in it. The others stay as they
    /// are.
    fn bind_rows_shared(&self, children: &mut [SharedResource], adds: (bool, bool)) {
        fn offered(resource: &SharedResource, offers: &dyn Fn(&str) -> bool) -> bool {
            offers(resource.id())
                || resource
                    .children()
                    .iter()
                    .any(|child| offered(child, offers))
        }
        let _profile = tilo_resource::profiling::Phase::start("framework.row_declarations");
        let offers = |id: &str| {
            let mut any = false;
            self.surfaces(&mut |surface| any = any || surface.offers_on_row(id));
            any
        };
        for child in children {
            if !offered(child, &offers) {
                continue;
            }
            let mut bound = [child.to_resource()];
            self.bind_row_declarations(&mut bound);
            let [mut resource] = bound;
            self.decorate_child(&mut resource, adds);
            *child = SharedResource::from_resource(resource);
        }
    }
}

/// The item children of `collections`, each the one `last` holds for its
/// item while its label is unchanged: by position when the items are the
/// same ones in the same order, else by id.
fn build_items(
    resource: &ResourceType,
    last: Option<&ItemChildren>,
    collections: &[ItemCollection],
    actions: &[ItemAction],
    eligible: &dyn Fn(&str) -> bool,
) -> SharedChildren {
    let last_children = last.map_or(&[][..], |last| last.children.as_slice());
    let in_place = last.is_some_and(|last| same_members(&last.collections, collections));
    let by_id: HashMap<&str, &SharedResource> = if in_place {
        HashMap::new()
    } else {
        last_children
            .iter()
            .map(|child| (child.id(), child))
            .collect()
    };
    let mut children = Vec::with_capacity(last_children.len());
    (resource.items)(collections, actions, &mut |collection, id, label, build| {
        let kept = if in_place {
            last_children.get(children.len())
        } else {
            by_id
                .get((resource.item_id)(collection, id).as_str())
                .copied()
        };
        children.push(
            kept.filter(|child| child.head().label == label)
                .cloned()
                .unwrap_or_else(|| {
                    let mut child = build();
                    if resource.readable {
                        offer_document_reads(&mut child, eligible);
                    }
                    SharedResource::from_resource(child)
                }),
        );
    });
    SharedChildren::new(children)
}

/// Whether `last` and `next` hold the same items in the same order, in
/// collections published alike.
fn same_members(last: &[ItemCollection], next: &[ItemCollection]) -> bool {
    last.len() == next.len()
        && last.iter().zip(next).all(|(last, next)| {
            last.field == next.field
                && last.within == next.within
                && last.labelled == next.labelled
                && last.items.len() == next.items.len()
                && last
                    .items
                    .iter()
                    .zip(&next.items)
                    .all(|((last, _), (next, _))| last == next)
        })
}

fn nested_view_types(resources: &[SharedResource]) -> HashSet<String> {
    fn collect(resources: &[SharedResource], views: &mut HashSet<String>) {
        for resource in resources {
            views.extend(
                resource
                    .head()
                    .actions
                    .iter()
                    .filter(|spec| spec.kind == tilo_actions::OperationKind::View)
                    .filter_map(|spec| spec.operation_type.clone()),
            );
            collect(resource.children().as_slice(), views);
        }
    }
    let mut views = HashSet::new();
    collect(resources, &mut views);
    views
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use serde_json::json;

    use super::super::ResourceRoot;
    use super::super::RootPublishes;
    use crate::prelude::*;
    use ui_core::published::SharedResource;

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    /// One task.
    pub struct Task {
        pub title: String,
        pub done: bool,
    }

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    /// One note.
    pub struct Note {
        pub text: String,
    }

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    /// A draft, tasks and notes.
    pub struct Board {
        pub draft: String,
        pub tasks: Collection<Task>,
        pub notes: Collection<Note>,
    }

    #[resource(kind = "test.shared-board", label = "Board")]
    impl Board {
        /// Set the draft.
        #[action]
        pub fn draft(&mut self, title: String) {
            self.draft = title;
        }

        /// Add a task at `index`.
        #[action]
        pub fn add(&mut self, cx: &mut ActionCx, title: String, index: u32) {
            let index = usize::try_from(index).unwrap_or(usize::MAX);
            self.tasks
                .insert(index, cx.id(), Task { title, done: false });
        }

        /// Mark a task done or open.
        ///
        /// # Errors
        /// Refuses a task that is gone.
        #[action(item = tasks)]
        pub fn set_done(&mut self, task: &Id, done: bool) -> Result<(), &'static str> {
            self.tasks.get_mut(task).ok_or("gone")?.done = done;
            Ok(())
        }

        /// Retitle a task.
        ///
        /// # Errors
        /// Refuses a task that is gone.
        #[action(item = tasks)]
        pub fn rename(&mut self, task: &Id, title: String) -> Result<(), &'static str> {
            self.tasks.get_mut(task).ok_or("gone")?.title = title;
            Ok(())
        }

        /// Remove a task.
        #[action(item = tasks)]
        pub fn remove(&mut self, task: &Id) {
            self.tasks.remove(task);
        }

        /// Move a task to `index`.
        #[action(item = tasks)]
        pub fn move_task(&mut self, task: &Id, index: u32) {
            self.tasks
                .move_to(task, usize::try_from(index).unwrap_or(usize::MAX));
        }

        /// Add a note.
        #[action]
        pub fn jot(&mut self, cx: &mut ActionCx, text: String) {
            self.notes.push(cx.id(), Note { text });
        }

        /// Drop a note.
        #[action(item = notes)]
        pub fn drop_note(&mut self, note: &Id) {
            self.notes.remove(note);
        }
    }

    struct BoardApp;

    impl ResourceApp for BoardApp {
        type Resource = Board;
        const ID: &'static str = "so.tilo.test-shared-board";
        const TITLE: &'static str = "Board";

        fn view(cx: &mut ViewCx<Board>) -> Element {
            Box::new(Column::new().push(Box::new(cx.control_with(
                |shown| TextInput::from_state(shown).with_id("draft"),
                |board| board.draft.clone(),
                Board::DRAFT,
            ))))
        }
    }

    /// `resource` as inert data, children included: what agents, the
    /// palette and accessibility read.
    fn outline(resource: &tilo_actions::Resource) -> serde_json::Value {
        json!({
            "id": resource.id.as_str(),
            "kind": resource.kind,
            "label": resource.label,
            "presentation": format!("{:?}", resource.presentation),
            "identity": format!("{:?}", resource.semantic_identity),
            "actions": tilo_actions::door::survey(&[super::super::without_children(resource)]),
            "views": format!("{:?}", resource.views),
            "children": resource.children.iter().map(outline).collect::<Vec<_>>(),
        })
    }

    /// The view operation types anywhere beneath `resources`.
    fn views_beneath(resources: &[tilo_actions::Resource]) -> std::collections::BTreeSet<String> {
        let mut views = std::collections::BTreeSet::new();
        for resource in resources {
            views.extend(
                resource
                    .actions
                    .iter()
                    .filter(|spec| spec.kind == tilo_actions::OperationKind::View)
                    .filter_map(|spec| spec.operation_type.clone()),
            );
            views.extend(views_beneath(&resource.children));
        }
        views
    }

    /// The board's tree as kept between versions, checked against the one
    /// built from scratch, and what the root publishes from it against what
    /// it publishes as ordinary trees.
    fn kept_tree(app: &mut TestApp<BoardApp>) -> SharedResource {
        let doc = app.read_document(Clone::clone);
        app.harness()
            .with_widget_mut(|root: &mut ResourceRoot| {
                let state = root.typed::<Board>().expect("a board").live.get();
                let kept = root.shared_document_tree(&*state);
                let fresh = tilo_resource::publish::<Board>(
                    root.dispatch.resource_id(),
                    &root.label,
                    &*state,
                );
                assert_eq!(
                    outline(&kept.to_resource()),
                    outline(&fresh),
                    "the document tree"
                );
                let shared = root
                    .shared_resources(None, &doc)
                    .expect("nothing in the view decorates the board's children");
                let plain = root.resources_for(None, &doc);
                assert_eq!(
                    ui_core::published::to_resources(&shared)
                        .iter()
                        .map(outline)
                        .collect::<Vec<_>>(),
                    plain.iter().map(outline).collect::<Vec<_>>(),
                    "the published resources"
                );
                assert_eq!(
                    root.nested_views()
                        .iter()
                        .cloned()
                        .collect::<std::collections::BTreeSet<_>>(),
                    views_beneath(&fresh.children),
                    "the nested views"
                );
                // A version the view has not synced (here, the host's empty
                // document) publishes its own tree, both ways alike, and
                // leaves the live tree as it was.
                let unsynced = ui_core::automerge::AutoCommit::new();
                let shared = root
                    .shared_resources(None, &unsynced)
                    .expect("an unsynced version is published too");
                assert_eq!(
                    ui_core::published::to_resources(&shared)
                        .iter()
                        .map(outline)
                        .collect::<Vec<_>>(),
                    root.resources_for(None, &unsynced)
                        .iter()
                        .map(outline)
                        .collect::<Vec<_>>(),
                    "an unsynced version's published resources"
                );
                let empty = <Board as tilo_resource::Resource>::read(&unsynced)
                    .expect("an empty document reads as an empty board");
                assert_eq!(
                    shared[0].children().len(),
                    0,
                    "the unsynced board has no items"
                );
                assert_eq!(
                    outline(&root.shared_document_tree(&empty).to_resource()),
                    outline(&tilo_resource::publish::<Board>(
                        root.dispatch.resource_id(),
                        &root.label,
                        &empty
                    )),
                    "an unsynced version's document tree"
                );
                assert!(
                    root.shared_document_tree(&*root.typed.state())
                        .ptr_eq(&kept),
                    "the live tree is the one kept for this revision"
                );
                kept
            })
            .expect("the root is the board's")
    }

    fn call(app: &mut TestApp<BoardApp>, action: &str, arguments: serde_json::Value) {
        app.call(action, arguments)
            .unwrap_or_else(|error| panic!("{action}: {error:?}"));
    }

    #[test]
    fn the_kept_tree_is_the_one_built_from_scratch_after_any_edit() {
        let mut app = TestApp::<BoardApp>::new().expect("the board opens");
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut random = move |below: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            usize::try_from(seed % u64::try_from(below.max(1)).unwrap_or(1)).unwrap_or(0)
        };
        for task in 0..12 {
            call(
                &mut app,
                "add",
                json!({ "title": format!("Task {task}"), "index": task }),
            );
        }
        let mut last = kept_tree(&mut app);
        for step in 0..160 {
            let tasks = app.state().tasks.ids().cloned().collect::<Vec<_>>();
            let notes = app.state().notes.ids().cloned().collect::<Vec<_>>();
            let task = tasks
                .get(random(tasks.len()))
                .map(|id| id.as_str().to_owned());
            match (random(8), task) {
                (1, Some(task)) => call(
                    &mut app,
                    "set-done",
                    json!({ "task": task, "done": step % 2 == 0 }),
                ),
                (2, Some(task)) => call(
                    &mut app,
                    "rename",
                    json!({ "task": task, "title": format!("Renamed {step}") }),
                ),
                (3, Some(task)) if tasks.len() > 4 => {
                    call(&mut app, "remove", json!({ "task": task }));
                }
                (4, Some(task)) => call(
                    &mut app,
                    "move-task",
                    json!({ "task": task, "index": random(tasks.len()) }),
                ),
                (5, _) => call(
                    &mut app,
                    "add",
                    json!({ "title": format!("New {step}"), "index": random(tasks.len() + 1) }),
                ),
                (6, _) => call(&mut app, "jot", json!({ "text": format!("Note {step}") })),
                (7, _) if !notes.is_empty() => call(
                    &mut app,
                    "drop-note",
                    json!({ "note": notes[random(notes.len())].as_str() }),
                ),
                _ => call(
                    &mut app,
                    "draft",
                    json!({ "title": format!("Draft {step}") }),
                ),
            }
            let next = kept_tree(&mut app);
            // Every item child the edit left as it was is the very node of
            // the last tree.
            let unchanged = |child: &&SharedResource| {
                next.children()
                    .iter()
                    .any(|now| now.id() == child.id() && now.head().label == child.head().label)
            };
            let shared =
                |child: &&SharedResource| next.children().iter().any(|now| now.ptr_eq(child));
            assert_eq!(
                last.children().iter().filter(shared).count(),
                last.children().iter().filter(unchanged).count(),
                "step {step}: every unchanged item is shared"
            );
            last = next;
        }
    }

    #[test]
    fn an_edit_no_item_depends_on_shares_every_item() {
        let mut app = TestApp::<BoardApp>::new().expect("the board opens");
        for task in 0..50 {
            call(
                &mut app,
                "add",
                json!({ "title": format!("Task {task}"), "index": task }),
            );
        }
        let before = kept_tree(&mut app);
        call(&mut app, "draft", json!({ "title": "typed" }));
        let after = kept_tree(&mut app);
        assert!(!before.ptr_eq(&after), "the root's own state changed");
        assert!(
            before.children().ptr_eq(after.children()),
            "a keystroke in the draft builds no item"
        );
    }

    /// The board with each task shown as a row offering its own view, as
    /// Chat's channel rows offer their conversation, in a view that opens
    /// items.
    struct ViewedBoardApp;

    impl ResourceApp for ViewedBoardApp {
        type Resource = Board;
        const ID: &'static str = "so.tilo.test-viewed-board";
        const TITLE: &'static str = "Board";

        fn view(cx: &mut ViewCx<Board>) -> Element {
            cx.on_open_item(|_| Ok(()));
            cx.surface(TaskRows::default())
        }
    }

    #[derive(Default)]
    struct TaskRows {
        rect: Rect,
        tasks: Vec<Id>,
    }

    impl Widget for TaskRows {
        fn build_layout(
            &mut self,
            taffy: &mut crate::advanced::TaffyTree<()>,
            _: &Theme,
            _: &mut std::collections::HashMap<crate::advanced::NodeId, *mut dyn Widget>,
        ) -> crate::advanced::NodeId {
            taffy
                .new_leaf(ui_core::taffy::Style::default())
                .unwrap_or_else(|_| crate::advanced::NodeId::from(0_u64))
        }

        fn apply_layout(
            &mut self,
            _: &crate::advanced::TaffyTree<()>,
            _: crate::advanced::NodeId,
            origin: Point,
        ) {
            self.rect = Rect::from_origin_size(origin, Size::ZERO);
        }

        fn name(&self) -> &'static str {
            "TaskRows"
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    impl ResourceSurface<Board> for TaskRows {
        fn present(&mut self, board: &std::rc::Rc<Board>) {
            self.tasks = board.tasks.ids().cloned().collect();
        }

        fn live_rows(&self) -> Vec<LiveRow> {
            self.tasks
                .iter()
                .map(|task| {
                    LiveRow::new(
                        tilo_resource::items::item_resource_id::<Board>("tasks", task),
                        "test.shared-board.task",
                        "Task",
                    )
                    .view(
                        tilo_actions::ResourceViewSpec::new("look", "Look", "open")
                            .with_primary(true),
                    )
                })
                .collect()
        }
    }

    /// An item a row offers a view of shows that view, as the whole tree
    /// publishes it, and not also the item view the framework offers items
    /// without one: two primary views opened by one action break the action
    /// contract, and the host then published nothing of the app (Chat in a
    /// site's workspace listed no conversation to agents or the palette).
    #[test]
    fn a_rows_view_replaces_the_frameworks_item_view() {
        let mut app = TestApp::<ViewedBoardApp>::new().expect("the board opens");
        app.call("add", json!({ "title": "Read", "index": 0 }))
            .expect("a task is added");
        let app_task = app.state().tasks.ids().next().cloned().expect("the task");
        let doc = app.read_document(Clone::clone);
        app.harness()
            .with_widget_mut(|root: &mut ResourceRoot| {
                let shared = root
                    .shared_resources(None, &doc)
                    .expect("the board publishes a shared forest");
                let shared = ui_core::published::to_resources(&shared);
                let item = tilo_resource::items::item_resource_id::<Board>("tasks", &app_task);
                let task = shared[0]
                    .children
                    .iter()
                    .find(|child| child.id.as_str() == item)
                    .expect("the task")
                    .clone();
                let views: Vec<(&str, bool)> = task
                    .views
                    .iter()
                    .map(|view| (view.id.as_str(), view.primary))
                    .collect();
                assert_eq!(
                    views,
                    [("look", true)],
                    "the row's view is the task's one view"
                );
                assert!(
                    task.actions.iter().any(|action| action.id == "open"),
                    "the task's `open` shows it"
                );
                assert_eq!(
                    shared.iter().map(outline).collect::<Vec<_>>(),
                    root.resources_for(None, &doc)
                        .iter()
                        .map(outline)
                        .collect::<Vec<_>>(),
                    "the shared forest is the whole tree"
                );
            })
            .expect("the root is the board's");
    }
}
