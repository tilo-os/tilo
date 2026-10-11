//! The published tree as a [`SharedResource`]: each version shares with the
//! last every item child the change did not touch, so a keystroke in a field
//! no item depends on neither builds nor copies an item (TIL-766).
//!
//! The caches and their bookkeeping live in [`SharedTree`], which does not
//! depend on the resource's type, so they are compiled once in the shared
//! runtime. `ResourceRoot<R>` is compiled once per resource type in every
//! app, so its methods here only gather what needs `R`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use tilo_resource::Resource;
use tilo_resource::items::{ChildrenParts, ItemAction, ItemChildVisitor, ItemCollection};
use ui_core::automerge::AutoCommit;
use ui_core::published::{SharedChildren, SharedResource};
use ui_core::widget::Widget;

use super::super::rows::outside_document;
use super::super::surface::SurfacePublication;
use super::{Decoration, ResourceRoot, open_spec};

/// The live revision, locale revision and
/// [`Resource::publication_cache_epoch`] a document-owned tree was built at.
type TreeRevision = (u64, u64, String);

/// The document-owned tree at one [`TreeRevision`].
struct SharedTreeCache {
    revision: TreeRevision,
    tree: SharedResource,
}

/// The item children last published: the collections they were built from,
/// at one locale (their actions' labels are localised).
struct ItemChildren {
    i18n: u64,
    collections: Vec<ItemCollection>,
    children: SharedChildren,
}

/// The view operation types of the nested resources at one live revision,
/// and the children they were collected from.
struct NestedViews {
    revision: u64,
    children: SharedChildren,
    views: Rc<HashSet<String>>,
}

/// [`tilo_resource::items::for_each_item_child`] of one resource type.
type ItemWalk = fn(&[ItemCollection], &[ItemAction], &mut ItemChildVisitor<'_>);

/// How a resource type publishes its items: its [`Resource::KIND`], which
/// their resource ids start with, and its
/// [`tilo_resource::items::for_each_item_child`].
struct ItemSource {
    kind: &'static str,
    walk: ItemWalk,
}

/// What one root keeps of its publication between versions.
#[derive(Default)]
pub(super) struct SharedTree {
    /// The document-owned tree at one live revision, locale and epoch.
    /// Surface bindings, local rows and effect availability are decorated
    /// afresh for every caller.
    cached: RefCell<Option<SharedTreeCache>>,
    /// The item children last published, kept so the next tree builds only
    /// the items whose label changed.
    items: RefCell<Option<ItemChildren>>,
    /// The view operation types of the nested resources at one live revision.
    nested: RefCell<Option<NestedViews>>,
}

impl SharedTree {
    /// The tree kept at `revision`, else the one `build` makes, kept at
    /// `revision`. Without a revision (a version that is not the live one,
    /// or a resource that does not opt in to reuse) it is built afresh.
    fn tree(
        &self,
        revision: Option<TreeRevision>,
        build: &mut dyn FnMut() -> SharedResource,
    ) -> SharedResource {
        let Some(revision) = revision else {
            return build();
        };
        if let Some(tree) = self.kept_at(&revision) {
            return tree;
        }
        let tree = build();
        *self.cached.borrow_mut() = Some(SharedTreeCache {
            revision,
            tree: tree.clone(),
        });
        tree
    }

    /// The tree kept at `revision`, if it is the one kept.
    fn kept_at(&self, revision: &TreeRevision) -> Option<SharedResource> {
        self.cached
            .borrow()
            .as_ref()
            .filter(|cached| cached.revision == *revision)
            .map(|cached| cached.tree.clone())
    }

    /// The head of the tree kept at `revision`, if it is the one kept.
    pub(super) fn head_at(&self, revision: &TreeRevision) -> Option<tilo_actions::Resource> {
        self.kept_at(revision).map(|tree| tree.head().clone())
    }

    /// The document-owned tree of `head` and `parts`; `keep` when it is the
    /// live state's, whose items the next version shares.
    fn document_tree(
        &self,
        head: tilo_actions::Resource,
        parts: ChildrenParts,
        items: &ItemSource,
        keep: bool,
    ) -> SharedResource {
        let item_children = self.item_children(parts.items, &parts.item_actions, items, keep);
        let children = if parts.others.is_empty() {
            item_children
        } else {
            SharedChildren::new(
                parts
                    .others
                    .into_iter()
                    .map(SharedResource::from_resource)
                    .chain(item_children.iter().cloned())
                    .collect(),
            )
        };
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
        items: &ItemSource,
        keep: bool,
    ) -> SharedChildren {
        let i18n = ui_core::i18n::I18nManager::revision();
        let mut memo = self.items.borrow_mut();
        if memo.as_ref().is_some_and(|last| last.i18n != i18n) {
            *memo = None;
        }
        if let Some(last) = memo.as_ref().filter(|last| last.collections == collections) {
            return last.children.clone();
        }
        let children = build_items(memo.as_ref(), &collections, actions, items);
        if keep {
            *memo = Some(ItemChildren {
                i18n,
                collections,
                children: children.clone(),
            });
        }
        children
    }

    /// The nested views pinned at `revision`, if they are.
    fn nested_views_at(&self, revision: u64) -> Option<Rc<HashSet<String>>> {
        self.nested
            .borrow()
            .as_ref()
            .filter(|pinned| pinned.revision == revision)
            .map(|pinned| Rc::clone(&pinned.views))
    }

    /// Pin the nested views of `children` at `revision`, collecting them
    /// afresh only when the children are not the ones last pinned.
    fn keep_nested_views(&self, revision: u64, children: &SharedChildren) -> Rc<HashSet<String>> {
        let mut kept = self.nested.borrow_mut();
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
}

impl<R: Resource> ResourceRoot<R> {
    /// The document-owned tree of `state`: [`tilo_resource::publish`],
    /// sharing every item child whose collection field, id and label are
    /// the ones it was built with. Cached per live revision when `state` is
    /// the live state and the resource opts in
    /// ([`Resource::publication_cache_epoch`]); a version the view has not
    /// synced yet is built afresh (sharing the items all the same).
    pub(super) fn shared_document_tree(&self, state: &R::State) -> SharedResource {
        let live = std::ptr::eq(Rc::as_ptr(&self.live.get()), state);
        let revision = R::publication_cache_epoch(state)
            .filter(|_| live)
            .map(|epoch| {
                (
                    self.live.revision(),
                    ui_core::i18n::I18nManager::revision(),
                    epoch,
                )
            });
        self.tree
            .tree(revision, &mut || self.build_document_tree(state, live))
    }

    /// The document-owned tree of `state`; `live` when it is the live
    /// state, whose items the next version shares.
    fn build_document_tree(&self, state: &R::State, live: bool) -> SharedResource {
        #[cfg(feature = "test-support")]
        let _profile = super::super::profiling::Scope::start("framework.resource_tree");
        let head =
            tilo_resource::publish_head::<R>(self.dispatch.resource_id(), &self.label, state);
        let items = ItemSource {
            kind: R::KIND,
            walk: tilo_resource::items::for_each_item_child::<R>,
        };
        self.tree
            .document_tree(head, R::children_parts(state), &items, live)
    }

    /// The view operation types of the nested resources of the live state.
    pub(super) fn nested_views(&self) -> Rc<HashSet<String>> {
        let revision = self.live.revision();
        if let Some(views) = self.tree.nested_views_at(revision) {
            return views;
        }
        let tree = self.shared_document_tree(&self.live.get());
        self.tree.keep_nested_views(revision, tree.children())
    }

    /// Answer the nested views of `state`, a version the view has not
    /// synced yet, until the live revision moves.
    pub(super) fn pin_nested_views(&self, state: &R::State) {
        let tree = self.shared_document_tree(state);
        self.tree
            .keep_nested_views(self.live.revision(), tree.children());
    }

    /// [`ui_core::resources::PublishesResources::resources_for`] as a shared
    /// forest, when it is one: nothing the view adds touches the tree's
    /// children (no surface, which alone publishes rows or binds edits to
    /// them, and no content resource naming the root). `None` otherwise, and
    /// for a participant's view, which is never kept.
    pub(super) fn shared_resources(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
    ) -> Option<Vec<SharedResource>> {
        if participant.is_some() || self.has_surface() {
            return None;
        }
        let current = if self.live.follows(doc) {
            None
        } else {
            Some(R::read(doc).ok()?)
        };
        let live = self.live.get();
        let state = current.as_ref().unwrap_or(&*live);
        let root = {
            #[cfg(feature = "test-support")]
            let _profile = super::super::profiling::Scope::start("framework.publication");
            let tree = self.shared_document_tree(state);
            if current.is_some() {
                // As `current_publication`: nested views follow this state
                // until the view syncs to it.
                self.tree
                    .keep_nested_views(self.live.revision(), tree.children());
            }
            let head = self.decorate_publication(state, tree.head().clone(), Decoration::Actions);
            forest_root(
                head,
                tree.children(),
                self.storage.person_resources(),
                self.stages.opens(),
            )
        };
        forest(root, self.content.as_ref(), doc)
    }

    /// Whether the view holds a hosted surface of this resource.
    fn has_surface(&self) -> bool {
        let mut found = false;
        self.surfaces(&mut |_: &dyn SurfacePublication<R>| found = true);
        found
    }
}

/// The published root over `head`, the decorated head of a document tree
/// whose children are `children`: those children, then `persons`, and the
/// framework's `open` view when the view `opens` items.
fn forest_root(
    mut head: tilo_actions::Resource,
    children: &SharedChildren,
    persons: Vec<tilo_actions::Resource>,
    opens: bool,
) -> SharedResource {
    let children = if persons.is_empty() {
        children.clone()
    } else {
        SharedChildren::new(
            children
                .iter()
                .cloned()
                .chain(persons.into_iter().map(SharedResource::from_resource))
                .collect(),
        )
    };
    if opens && !head.actions.iter().any(|existing| existing.id == "open") {
        head = head.with_action(open_spec());
    }
    SharedResource::new(head, children)
}

/// `root`, then the resources `content` publishes beside it, outside the
/// document; `None` when one of them is the root itself.
fn forest(
    root: SharedResource,
    content: &dyn Widget,
    doc: &AutoCommit,
) -> Option<Vec<SharedResource>> {
    let extras = ui_core::resources::resources_for(content, None, doc);
    if extras.iter().any(|extra| extra.id.as_str() == root.id()) {
        return None;
    }
    let mut resources = vec![root];
    resources.extend(
        extras
            .into_iter()
            .map(|extra| SharedResource::from_resource(outside_document(extra))),
    );
    Some(resources)
}

/// The item children of `collections`, each the one `last` holds for its
/// item while its label is unchanged: by position when the items are the
/// same ones in the same order, else by id.
fn build_items(
    last: Option<&ItemChildren>,
    collections: &[ItemCollection],
    actions: &[ItemAction],
    items: &ItemSource,
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
    (items.walk)(collections, actions, &mut |field, id, label, build| {
        let kept = if in_place {
            last_children.get(children.len())
        } else {
            by_id
                .get(tilo_resource::items::item_resource_id_in(items.kind, field, id).as_str())
                .copied()
        };
        children.push(
            kept.filter(|child| child.head().label == label)
                .cloned()
                .unwrap_or_else(|| SharedResource::from_resource(build())),
        );
    });
    SharedChildren::new(children)
}

/// Whether `last` and `next` hold the same items in the same order.
fn same_members(last: &[ItemCollection], next: &[ItemCollection]) -> bool {
    last.len() == next.len()
        && last.iter().zip(next).all(|(last, next)| {
            last.field == next.field
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
    use crate::prelude::*;
    use ui_core::published::SharedResource;
    use ui_core::resources::PublishesResources;

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
        #[action]
        pub fn set_done(&mut self, task: &Id, done: bool) -> Result<(), &'static str> {
            self.tasks.get_mut(task).ok_or("gone")?.done = done;
            Ok(())
        }

        /// Retitle a task.
        ///
        /// # Errors
        /// Refuses a task that is gone.
        #[action]
        pub fn rename(&mut self, task: &Id, title: String) -> Result<(), &'static str> {
            self.tasks.get_mut(task).ok_or("gone")?.title = title;
            Ok(())
        }

        /// Remove a task.
        #[action]
        pub fn remove(&mut self, task: &Id) {
            self.tasks.remove(task);
        }

        /// Move a task to `index`.
        #[action]
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
        #[action]
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
            Box::new(Column::new().push(Box::new(
                cx.input(|board| board.draft.clone(), Board::DRAFT),
            )))
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
            .with_widget_mut(|root: &mut ResourceRoot<Board>| {
                let state = root.live.get();
                let kept = root.shared_document_tree(&state);
                let fresh = tilo_resource::publish::<Board>(
                    root.dispatch.resource_id(),
                    &root.label,
                    &state,
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
                    root.shared_document_tree(&root.live.get()).ptr_eq(&kept),
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
}
