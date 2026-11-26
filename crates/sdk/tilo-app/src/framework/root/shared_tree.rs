//! The published tree as a [`SharedResource`]: each version shares with the
//! last every item child the change did not touch, so a keystroke in a field
//! no item depends on neither builds nor copies an item, and what reads the
//! tree (bound controls, accessibility, the API) skips what it has seen.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use std::any::Any;

use tilo_resource::items::{ItemAction, ItemCollection};
use ui_core::automerge::AutoCommit;
use ui_core::published::{SharedChildren, SharedResource};

use super::super::chain_of_resource_id;
use super::super::rows::{LiveRow, join, outside_document};
use super::typed::ResourceType;
use super::{
    Decoration, ResourceRoot, offer_document_reads, offer_item_open, offer_item_reads, open_spec,
};

/// The document-owned tree at one live version, locale and
/// [`Resource::publication_cache_epoch`].
pub(super) struct SharedTreeCache {
    pub(super) revision: ((u64, u64), u64, String),
    pub(super) tree: SharedResource,
}

/// The document-owned tree of the live state as last built, and what it
/// was built from: the next one is this very tree while its head, its kept
/// children ([`ChildrenParts::kept`](tilo_resource::items::ChildrenParts))
/// and its items are the very ones it was built from.
pub(super) struct DocumentChildren {
    kept: Rc<[Rc<tilo_actions::ResourceSpec>]>,
    /// `kept` as published.
    published: Vec<SharedResource>,
    items: SharedChildren,
    /// Whether the tree's children are `published` and `items` alone.
    whole: bool,
    head: Rc<tilo_actions::ResourceSpec>,
    tree: SharedResource,
}

/// The item children last published: the collections they were built from,
/// at one locale (their actions' labels are localised).
pub(crate) struct ItemChildren {
    i18n: u64,
    collections: Vec<ItemCollection>,
    children: SharedChildren,
}

impl ItemChildren {
    /// One child per item of `collections` carrying `actions`
    /// ([`tilo_resource::items::item_children`]) of `resource`, each made
    /// into the node it is published as by `finish`: the very children of
    /// `memo` when it was built from these collections, otherwise each child
    /// `memo` holds whose label is unchanged, and the rest built. So a change
    /// to one item builds that item, whatever the collections hold. `keep`
    /// remembers these in `memo` for the next version.
    pub(crate) fn build(
        memo: &mut Option<Self>,
        resource: &ResourceType,
        collections: Vec<ItemCollection>,
        actions: &[ItemAction],
        keep: bool,
        finish: &dyn Fn(tilo_actions::ResourceSpec) -> SharedResource,
    ) -> SharedChildren {
        let i18n = ui_core::i18n::I18nManager::revision();
        if memo.as_ref().is_some_and(|last| last.i18n != i18n) {
            *memo = None;
        }
        if let Some(last) = memo.as_ref().filter(|last| last.collections == collections) {
            return last.children.clone();
        }
        let children = build_items(resource, memo.as_ref(), &collections, actions, finish);
        if keep {
            *memo = Some(Self {
                i18n,
                collections,
                children: children.clone(),
            });
        }
        children
    }
}

/// The view operation types beneath the resource and its records, at one live
/// version and records' revisions, and the children they were collected from.
pub(super) struct NestedViews {
    at: ((u64, u64), Vec<(String, u64)>),
    children: SharedChildren,
    views: Rc<HashSet<String>>,
}

/// The children of the publication with the resources kept beside the
/// resource, and the chains among them, at the kept resources' revisions,
/// one locale and whether `open` is offered on items.
pub(super) struct ResourceChildren {
    document: SharedChildren,
    records: Vec<(String, u64)>,
    i18n: u64,
    opens: bool,
    children: SharedChildren,
    chains: Vec<SharedResource>,
}

/// What a publication adds to each child of the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Adds {
    /// `open` on the items a view opens.
    open: bool,
    /// Document actions queued as jobs, for an app that drains effects
    /// causally.
    jobs: bool,
    /// The document read of each item that has none yet (one the app's own
    /// projection names already has its read): an item is document state,
    /// disclosed by its read.
    reads: bool,
}

impl Adds {
    const fn none(self) -> bool {
        !self.open && !self.jobs && !self.reads
    }
}

/// The document children as last published, after what the publication adds
/// to each child: `open` on items a view opens, and document actions queued
/// as jobs. Kept so the next publication adds it only to the children that
/// changed.
pub(super) struct PublishedChildren {
    /// What was added.
    adds: Adds,
    document: SharedChildren,
    published: SharedChildren,
}

/// The children of the publication with the live rows the surfaces show
/// merged in: each group of rows under one id (merged into the document
/// child of that id, or appended), with what it published. A group whose
/// rows are the very ones the surfaces handed out last time, over the same
/// document child, is that very node again.
pub(super) struct RowChildren {
    /// What was added.
    adds: Adds,
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
    /// it was built with. Cached per live version when `state` is the live
    /// state and the resource opts in ([`Resource::publication_cache_epoch`]),
    /// and then the very tree of the version before while nothing it shows
    /// changed; a version the view has not synced yet is built afresh
    /// (sharing the items all the same).
    pub(super) fn shared_document_tree(&self, state: &dyn Any) -> SharedResource {
        let live = std::ptr::addr_eq(Rc::as_ptr(&self.typed.state()), state);
        let Some(epoch) = self.typed.cache_epoch(state).filter(|_| live) else {
            return self.build_document_tree(state, live, None);
        };
        let revision = (
            self.version(),
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
        let tree = self.build_document_tree(state, true, Some(&revision.2));
        *self.publication_tree.borrow_mut() = Some(SharedTreeCache {
            revision,
            tree: tree.clone(),
        });
        tree
    }

    /// The document-owned tree of `state`; `live` when it is the live
    /// state, whose items the next version shares. With the `epoch` of a
    /// live state that may reuse its publication, the very tree built last
    /// while its head and children are the very ones it was built from.
    fn build_document_tree(
        &self,
        state: &dyn Any,
        live: bool,
        epoch: Option<&str>,
    ) -> SharedResource {
        let parts = self.typed.children_parts(state);
        let items = self.item_children(parts.items, &parts.item_actions, live, state);
        let head = match epoch {
            Some(epoch) => self.live_head(state, epoch, &self.name_of(state)),
            None => Rc::new(self.typed.head(
                self.dispatch.resource_id(),
                &self.name_of(state),
                state,
            )),
        };
        let mut kept = self.document_children.borrow_mut();
        let last = kept
            .as_ref()
            .filter(|last| epoch.is_some() && Rc::ptr_eq(&last.kept, &parts.kept));
        if let Some(last) = last.filter(|last| {
            last.whole
                && parts.others.is_empty()
                && last.items.ptr_eq(&items)
                && Rc::ptr_eq(&last.head, &head)
        }) {
            return last.tree.clone();
        }
        let _profile = tilo_resource::profiling::Phase::start("framework.resource_tree");
        let published: Vec<SharedResource> = if let Some(last) = last {
            last.published.clone()
        } else {
            // A child published as it was is the very node it was.
            let before: HashMap<*const tilo_actions::ResourceSpec, &SharedResource> = kept
                .as_ref()
                .filter(|_| epoch.is_some())
                .map(|last| {
                    last.kept
                        .iter()
                        .map(Rc::as_ptr)
                        .zip(&last.published)
                        .collect()
                })
                .unwrap_or_default();
            parts
                .kept
                .iter()
                .map(|child| match before.get(&Rc::as_ptr(child)) {
                    Some(&shared) => shared.clone(),
                    None => self.document_child_resource((**child).clone(), state),
                })
                .collect()
        };
        let whole = parts.others.is_empty();
        let children = if whole && published.is_empty() {
            items.clone()
        } else {
            SharedChildren::new(
                parts
                    .others
                    .into_iter()
                    .map(|child| self.document_child_resource(child, state))
                    .chain(published.iter().cloned())
                    .chain(items.iter().cloned())
                    .collect(),
            )
        };
        let tree = SharedResource::new((*head).clone(), children);
        if epoch.is_some() {
            *kept = Some(DocumentChildren {
                kept: parts.kept,
                published,
                items,
                whole,
                head,
                tree: tree.clone(),
            });
        }
        tree
    }

    /// `child`, a document child of `state`, as the tree carries it.
    fn document_child_resource(
        &self,
        mut child: tilo_actions::ResourceSpec,
        state: &dyn Any,
    ) -> SharedResource {
        if self.resource.readable {
            offer_document_reads(&mut child, &|id| self.typed.document_child(state, id));
        }
        SharedResource::from_resource(child)
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
        let readable = self.resource.readable;
        ItemChildren::build(
            &mut self.item_children.borrow_mut(),
            &self.resource,
            collections,
            actions,
            keep,
            &|mut child| {
                if readable {
                    offer_document_reads(&mut child, &|id| self.typed.document_child(state, id));
                }
                SharedResource::from_resource(child)
            },
        )
    }

    /// The view operation types beneath the live state and the records kept
    /// beside it, such as a chain's items' rich-text bodies.
    pub(super) fn nested_views(&self) -> Rc<HashSet<String>> {
        let at = (self.version(), self.storage.resources_key());
        if let Some(pinned) = self
            .nested_views
            .borrow()
            .as_ref()
            .filter(|pinned| pinned.at == at)
        {
            return Rc::clone(&pinned.views);
        }
        let tree = self.shared_document_tree(&*self.typed.state());
        self.keep_nested_views(tree.children())
    }

    /// Answer the nested views of `document` and the chains kept beside it,
    /// until the live revision moves.
    pub(super) fn keep_nested_views(&self, document: &SharedChildren) -> Rc<HashSet<String>> {
        let (children, chains) = self.with_resource_children(document.clone(), self.stages.opens());
        let at = (self.version(), self.storage.resources_key());
        let mut kept = self.nested_views.borrow_mut();
        let views = match kept.as_ref() {
            Some(last) if last.children.ptr_eq(&children) => Rc::clone(&last.views),
            _ => Rc::new(nested_view_types(children.as_slice(), &chains)),
        };
        *kept = Some(NestedViews {
            at,
            children,
            views: Rc::clone(&views),
        });
        views
    }

    /// [`ui_core::widget::resources::PublishesResources::resources_for`] as a shared
    /// forest, when it is one: nothing the view adds touches the tree's
    /// children beyond what [`Self::published_children`] keeps per child (no
    /// surface, which alone publishes rows or binds edits to them, and no
    /// content resource naming the root), and the record reads as the
    /// resource. `None` otherwise, and for a presence's view, which is
    /// never kept.
    pub(super) fn shared_resources(
        &self,
        presence: Option<&tilo_actions::PresenceId>,
        doc: &AutoCommit,
    ) -> Option<Vec<SharedResource>> {
        if presence.is_some() || self.typed.unreadable().is_some() || self.needs_whole_tree() {
            return None;
        }
        let surface = self.has_surface();
        let current = if self.follow_document(doc) {
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
                self.keep_nested_views(tree.children());
            }
            let mut head =
                self.decorate_publication(state, tree.head().clone(), Decoration::Actions);
            let document = self.published_children(tree.children(), opens);
            let document = if surface {
                self.with_live_rows(tree.children(), document, opens)
            } else {
                document
            };
            let (children, chains) = self.with_resource_children(document, opens);
            if opens && !head.actions.iter().any(|existing| existing.id == "open") {
                head = head.with_action(open_spec());
            }
            (head, children, chains)
        };
        let mut extras = Vec::new();
        for extra in ui_core::widget::resources::resources_for(self.content.as_ref(), None, doc) {
            if extra.id.as_str() == root.0.id.as_str() {
                return None;
            }
            // Never a document action: those are the resource's own.
            extras.push(SharedResource::from_resource(outside_document(extra)));
        }
        let mut resources = vec![SharedResource::new(root.0, root.1)];
        resources.extend(root.2);
        resources.append(&mut extras);
        Some(resources)
    }

    /// Whether the publication carries what only the whole tree builds
    /// ([`Self::decorate_publication`]): what the views say of their rows and
    /// items, the nested editors they opened, and the records they asked for
    /// and could not open. The shared tree builds none of them, so it would
    /// publish the rows without their statuses and leave the rest out.
    fn needs_whole_tree(&self) -> bool {
        let mut rows = false;
        self.surfaces(&mut |surface| rows = rows || surface.says_of_rows());
        rows || self.dispatch.has_nested() || !self.storage.unavailable_resources().is_empty()
    }

    /// `document`, the document-owned children, as the publication carries
    /// them: with `open` offered on the items when a view `opens` them, and
    /// document actions queued as jobs when the app drains effects causally,
    /// as [`Self::decorate_publication`] and `resources_for` add them to the
    /// whole tree. A child the last publication carried is that very node.
    fn published_children(&self, document: &SharedChildren, opens: bool) -> SharedChildren {
        let adds = self.adds(opens);
        if adds.none() {
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
                                self.decorate_child(&mut resource, adds);
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

    /// `document` followed by the resources kept beside the resource, as the
    /// publication carries them (with `open` on their items when a view
    /// `opens` them), and the chains kept beside it: each chain's items are
    /// the app's own, so a chain stands at the top of the app's tree and not
    /// under the resource it hangs off. The same children while neither
    /// moved, so what reads the forest skips them as seen.
    fn with_resource_children(
        &self,
        document: SharedChildren,
        opens: bool,
    ) -> (SharedChildren, Vec<SharedResource>) {
        let records = self.storage.resources_key();
        if records.is_empty() {
            return (document, Vec::new());
        }
        let i18n = ui_core::i18n::I18nManager::revision();
        let mut kept = self.resource_children.borrow_mut();
        if let Some(kept) = kept.as_ref().filter(|kept| {
            kept.document.ptr_eq(&document)
                && kept.records == records
                && kept.i18n == i18n
                && kept.opens == opens
        }) {
            return (kept.children.clone(), kept.chains.clone());
        }
        let opened = |beside: SharedResource| {
            if !opens {
                return beside;
            }
            // The opener is the resource's own: its items stay the nodes they were.
            let mut head = beside.head().clone();
            offer_item_open(self.resource.opens_as_item, &mut head);
            // It only ever adds: an opener, and the view it shows.
            let before = beside.head();
            if head.actions.len() == before.actions.len() && head.views.len() == before.views.len()
            {
                beside
            } else {
                SharedResource::new(head, beside.children().clone())
            }
        };
        let mut children = document.iter().cloned().collect::<Vec<_>>();
        let mut chains = Vec::new();
        for beside in self.storage.resource_resources_shared() {
            // The record the window holds, kept for the workspace too, is the window's own
            // resource: published once, as it.
            if beside.id() == self.dispatch.resource_id() {
                continue;
            }
            if chain_of_resource_id(beside.id()).is_some() {
                chains.push(opened(beside));
            } else {
                children.push(opened(beside));
            }
        }
        let children = SharedChildren::new(children);
        *kept = Some(ResourceChildren {
            document,
            records,
            i18n,
            opens,
            children: children.clone(),
            chains: chains.clone(),
        });
        (children, chains)
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
    /// as seen; only a view that binds edits to rows
    /// ([`Publishes::row_edits`](super::super::Publishes::row_edits)) has its
    /// rows bound afresh.
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
        let adds = self.adds(opens);
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
        adds: Adds,
    ) -> SharedResource {
        let mut rows = rows.iter().cloned().map(LiveRow::into_resource);
        let mut resource = match base {
            Some(base) => base.to_resource(),
            None => match rows.next() {
                Some(first) => first,
                None => {
                    return SharedResource::from_resource(tilo_actions::ResourceSpec::new(
                        "", "", "",
                    ));
                }
            },
        };
        for row in rows {
            join(&mut resource, row);
        }
        self.decorate_child(&mut resource, adds);
        SharedResource::from_resource(resource)
    }

    /// What the publication adds to the document's children when a view
    /// `opens` items.
    fn adds(&self, opens: bool) -> Adds {
        Adds {
            open: opens,
            jobs: self.typed.has_causal_drains(),
            reads: self.resource.resource_table.is_some(),
        }
    }

    /// What the publication adds to each child, as `adds` says: `open` on
    /// items a view opens, document actions queued as jobs, and the
    /// document read of each item.
    fn decorate_child(&self, resource: &mut tilo_actions::ResourceSpec, adds: Adds) {
        if adds.jobs {
            mark_document_jobs(resource);
        }
        if adds.open {
            offer_item_open(self.resource.opens_as_item, resource);
        }
        if adds.reads
            && let Some(table) = self.resource.resource_table
        {
            offer_item_reads(resource, table, self.resource.kind);
        }
    }

    /// Bind the edits surfaces offer on rows to each of `children` holding a
    /// row any surface offers on, anywhere in it. The others stay as they
    /// are.
    fn bind_rows_shared(&self, children: &mut [SharedResource], adds: Adds) {
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
    finish: &dyn Fn(tilo_actions::ResourceSpec) -> SharedResource,
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
                    let _profile = tilo_resource::profiling::Phase::start("framework.item_child");
                    finish(build())
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

fn nested_view_types(children: &[SharedResource], chains: &[SharedResource]) -> HashSet<String> {
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
    collect(children, &mut views);
    collect(chains, &mut views);
    views
}

/// The document actions of `resource` and its children that can queue an
/// outside consequence (a verb taking an outbox, every action of a record
/// with a `queue`, undo, redo and restore, which queue a compensation, and a
/// field edit, which names a path the host resolves), published as jobs: the
/// invocation answers when what it queued has settled. Every other document
/// action applies where it is invoked, at once.
pub(super) fn mark_document_jobs(resource: &mut tilo_actions::ResourceSpec) {
    const OWN_JOB: [&str; 4] = ["field-edit", "undo", "redo", "restore"];
    for spec in &mut resource.actions {
        let outside = spec.contract.kind == tilo_actions::ActionKind::Outside;
        let queues = outside || OWN_JOB.contains(&spec.id.as_str());
        if spec.kind == tilo_actions::OperationKind::Document && queues {
            spec.type_mut().contract.execution =
                tilo_actions::ExecutionMode::Background { cancellable: true };
            if matches!(spec.contract.result, tilo_actions::ResultSpec::None) {
                spec.type_mut().contract.result = tilo_actions::ResultSpec::Job;
            }
        }
    }
    for child in &mut resource.children {
        mark_document_jobs(child);
    }
}
