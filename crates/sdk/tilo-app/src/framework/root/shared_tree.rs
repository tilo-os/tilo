//! The published tree as a [`SharedResource`]: each version shares with the
//! last every item child the change did not touch, so a keystroke in a field
//! no item depends on neither builds nor copies an item, and what reads the
//! tree (bound controls, accessibility, the door) skips what it has seen.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use tilo_resource::Resource;
use tilo_resource::items::{ItemAction, ItemCollection};
use ui_core::automerge::AutoCommit;
use ui_core::published::{SharedChildren, SharedResource};

use super::super::rows::outside_document;
use super::{Decoration, ResourceRoot, mark_document_jobs, offer_item_open, open_spec};

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

impl<R: Resource> ResourceRoot<R> {
    /// The document-owned tree of `state`: [`tilo_resource::publish`],
    /// sharing every item child whose collection, id and label are the ones
    /// it was built with. Cached per live revision when `state` is the live
    /// state and the resource opts in ([`Resource::publication_cache_epoch`]);
    /// a version the view has not synced yet is built afresh (sharing the
    /// items all the same).
    pub(super) fn shared_document_tree(&self, state: &R::State) -> SharedResource {
        let live = std::ptr::eq(Rc::as_ptr(&self.live.get()), state);
        let Some(epoch) = R::publication_cache_epoch(state).filter(|_| live) else {
            return self.build_document_tree(state, live);
        };
        let revision = (
            self.live.revision(),
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
    fn build_document_tree(&self, state: &R::State, live: bool) -> SharedResource {
        let _profile = tilo_resource::profiling::Phase::start("framework.resource_tree");
        let parts = R::children_parts(state);
        let items = self.item_children(parts.items, &parts.item_actions, live);
        let children = if parts.others.is_empty() {
            items
        } else {
            SharedChildren::new(
                parts
                    .others
                    .into_iter()
                    .map(SharedResource::from_resource)
                    .chain(items.iter().cloned())
                    .collect(),
            )
        };
        let head =
            tilo_resource::publish_head::<R>(self.dispatch.resource_id(), &self.label, state);
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
    ) -> SharedChildren {
        let i18n = ui_core::i18n::I18nManager::revision();
        let mut memo = self.item_children.borrow_mut();
        if memo.as_ref().is_some_and(|last| last.i18n != i18n) {
            *memo = None;
        }
        if let Some(last) = memo.as_ref().filter(|last| last.collections == collections) {
            return last.children.clone();
        }
        let children = build_items::<R>(memo.as_ref(), &collections, actions);
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
        let revision = self.live.revision();
        if let Some(pinned) = self
            .nested_views
            .borrow()
            .as_ref()
            .filter(|pinned| pinned.revision == revision)
        {
            return Rc::clone(&pinned.views);
        }
        let tree = self.shared_document_tree(&self.live.get());
        self.keep_nested_views(revision, tree.children())
    }

    /// Answer the nested views of `state`, a version the view has not
    /// synced yet, until the live revision moves.
    pub(super) fn pin_nested_views(&self, state: &R::State) {
        let tree = self.shared_document_tree(state);
        self.keep_nested_views(self.live.revision(), tree.children());
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
        if participant.is_some() || self.live.unreadable().is_some() {
            return None;
        }
        let surface = self.has_surface();
        let current = if self.live.follows(doc) {
            None
        } else {
            Some(R::read(doc).ok()?)
        };
        let live = self.live.get();
        let state = current.as_ref().unwrap_or(&*live);
        let opens = self.stages.opens();
        let root = {
            let _profile = tilo_resource::profiling::Phase::start("framework.publication");
            let tree = self.shared_document_tree(state);
            // A surface that adds rows, or edits bound to rows, changes the
            // children: that publication is built whole.
            if surface && !self.surfaces_publish_on_head_only(tree.children()) {
                return None;
            }
            if current.is_some() {
                // As `current_publication`: nested views follow this state
                // until the view syncs to it.
                self.keep_nested_views(self.live.revision(), tree.children());
            }
            let mut head =
                self.decorate_publication(state, tree.head().clone(), Decoration::Actions);
            let document = self.published_children(tree.children(), opens);
            let records = self.storage.record_resources();
            let children = if records.is_empty() {
                document
            } else {
                SharedChildren::new(
                    document
                        .iter()
                        .cloned()
                        .chain(records.into_iter().map(|mut record| {
                            if opens {
                                offer_item_open::<R>(&mut record);
                            }
                            SharedResource::from_resource(record)
                        }))
                        .collect(),
                )
            };
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
        let adds = (opens, self.effects.has_causal_drains());
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
                                    offer_item_open::<R>(&mut resource);
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

    /// Whether the view holds a hosted surface of this resource.
    fn has_surface(&self) -> bool {
        let mut found = false;
        self.surfaces(&mut |_| found = true);
        found
    }

    /// Whether every surface of the view publishes on the resource itself
    /// only, as its head's bound edits and availability: no live rows, and
    /// no edit bound to one of `children` or their children. Then the
    /// publication is the shared document tree under the decorated head.
    fn surfaces_publish_on_head_only(&self, children: &SharedChildren) -> bool {
        fn ids<'a>(children: &'a SharedChildren, out: &mut Vec<&'a str>) {
            for child in children {
                out.push(child.id());
                ids(child.children(), out);
            }
        }
        let mut all = Vec::new();
        ids(children, &mut all);
        let mut head_only = true;
        self.surfaces(&mut |surface| {
            head_only = head_only
                && surface.live_rows().is_empty()
                && all.iter().all(|id| surface.row_declarations(id).is_empty());
        });
        head_only
    }
}

/// The item children of `collections`, each the one `last` holds for its
/// item while its label is unchanged: by position when the items are the
/// same ones in the same order, else by id.
fn build_items<R: Resource>(
    last: Option<&ItemChildren>,
    collections: &[ItemCollection],
    actions: &[ItemAction],
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
    tilo_resource::items::for_each_item_child::<R>(
        collections,
        actions,
        &mut |collection, id, label, build| {
            let kept = if in_place {
                last_children.get(children.len())
            } else {
                by_id
                    .get(tilo_resource::items::item_child_id::<R>(collection, id).as_str())
                    .copied()
            };
            children.push(
                kept.filter(|child| child.head().label == label)
                    .cloned()
                    .unwrap_or_else(|| SharedResource::from_resource(build())),
            );
        },
    );
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
