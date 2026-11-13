//! The live tree of a view: every view call a boundary of its own, re-run only when what it
//! read changed, and rows of a list rendered only when the host shows them.
//!
//! A [`Live`] holds the composed tree the host sees and, beside it, one [`Bound`] per view
//! call: which view with which arguments, what it read, and where its `fragment` sits in the
//! tree. A change names (resource, field) pairs; an index maps each to the boundaries that read
//! it, so a change to one note marks that note's row and nothing else. Re-running a boundary
//! diffs only its subtree, and a view call inside it whose view and arguments are unchanged
//! keeps its subtree without running again (memoized by key, view and arguments).

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use crate::colls::CollSeen;
use crate::core::Core;
use crate::error::RuntimeError;
use crate::handles::Retained;
use crate::ids::Principal;
use crate::render::{Boundaries, LoopRows, Offer, Rendered, Scoped, Stub, ViewTarget, VirtualRows};
use crate::space::Space;
use crate::store::{ANY_FIELD, ChangeSummary, Splice, doc_of};
use crate::value::Value;
use crate::view::{Patch, UiNode, VirtualLists, diff_at};

mod virtual_rows;

use crate::view_state::StateKey;
use virtual_rows::Virtual;

pub(crate) type BoundId = usize;

/// One view call: the root view, or a call inside another boundary.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent facet of the call: rendered, a row, a row that stands \
              open, and a row that says of itself whether it is new"
)]
pub(crate) struct Bound {
    /// The key of the call's `fragment` in the tree; empty for the root.
    pub key: String,
    pub target: Rc<ViewTarget>,
    pub args: Vec<Value>,
    pub depth: usize,
    pub children: Vec<BoundId>,
    /// Child indices from the tree's root to the call's `fragment` (to the root node for the
    /// root boundary).
    pub path: Vec<usize>,
    /// Whether its tree is in the composed tree. A row of a list is not until it is shown.
    pub rendered: bool,
    /// A row of a list: rendered on demand.
    pub lazy: bool,
    pub reads: Vec<(String, String)>,
    /// What it saw of the collections it read item by item.
    pub colls: Vec<CollSeen>,
    /// The loops over collections whose rows it holds: they follow their collections, and it
    /// does not depend on which items those have.
    pub loops: Vec<Rc<LoopRows>>,
    pub space_reads: Vec<(String, f64)>,
    /// Whether it showed something only the window knows: a participant's name, a day or a time
    /// of day in the person's formats, a word in the language their window speaks.
    pub person_reads: bool,
    pub state_reads: Vec<(StateKey, u64)>,
    pub state_seen: Vec<(StateKey, Value)>,
    /// When the time it read next reads differently (seconds since 1970, UTC): it runs again
    /// then. `None` when it read no time.
    pub changes_at: Option<i64>,
    /// What the row was drawn knowing: whether the list it stands in stands a divider where
    /// its unseen part starts, which its own marks answer to. A row is built again when that
    /// changes under it, so a mark it drew for being new goes when the divider arrives.
    pub says_new: bool,
    /// Whether the row stands open into its item where it stands: its document is built here,
    /// and in no closed row. A row is built again when it opens or closes.
    pub open: bool,
    pub retained: Vec<Retained>,
}

impl Bound {
    /// The boundary of a view call, not rendered yet.
    fn new(stub: Stub<'_>, lazy: bool) -> Self {
        Self {
            key: stub.key.to_owned(),
            target: stub.target,
            args: stub.args,
            depth: stub.depth,
            children: Vec::new(),
            path: Vec::new(),
            rendered: false,
            lazy,
            reads: Vec::new(),
            colls: Vec::new(),
            loops: Vec::new(),
            space_reads: Vec::new(),
            person_reads: false,
            state_reads: Vec::new(),
            state_seen: Vec::new(),
            changes_at: None,
            says_new: false,
            open: stub.open,
            retained: Vec::new(),
        }
    }
}

/// A map keyed by resource ids, of which a view over a list reads tens of thousands: hashed
/// fast.
type FastMap<K, V> = HashMap<K, V, foldhash::fast::RandomState>;

/// `reads` without repeats, in the order first read.
fn distinct(mut reads: Vec<(String, String)>) -> Vec<(String, String)> {
    let first: Vec<bool> = {
        let mut seen =
            HashSet::with_capacity_and_hasher(reads.len(), foldhash::fast::RandomState::default());
        reads.iter().map(|read| seen.insert(read)).collect()
    };
    let mut first = first.into_iter();
    reads.retain(|_| first.next().unwrap_or(false));
    reads
}

/// The collection an item resource (written as [`crate::value::ResourceRef::id_string`]) is
/// in, as the store has it: its holder (written the same way) and the collection's field, and
/// the item's id.
fn item_in(core: &Core, resource: &str) -> Option<(String, String, String)> {
    let at = crate::value::ResourceRef::parse_id(resource, String::new())?;
    let item = at.item.clone()?;
    let (holder, field) = core.store.borrow().container(&at).ok()??;
    Some((holder.id_string(), field, item))
}

/// Which boundaries read what.
///
/// A view over a list reads a field of every item, so a boundary can hold tens of thousands of
/// reads: they are indexed per resource, each resource with the fields read on it and by whom,
/// which costs a key per resource read rather than per field and boundary.
#[derive(Default)]
struct Index {
    resources: FastMap<String, Vec<(String, BoundId)>>,
    /// The collections read item by item, per resource: a change to one reaches a boundary only
    /// if what it saw of it no longer holds.
    colls: FastMap<String, Vec<(String, BoundId)>>,
    /// The collections whose rows a boundary holds, per resource.
    loops: FastMap<String, Vec<(String, BoundId)>>,
    docs: FastMap<String, HashSet<BoundId>>,
    states: HashMap<StateKey, HashSet<BoundId>>,
    space: HashSet<BoundId>,
    /// The boundaries that show something only the window knows: a name, a format, a word.
    people: HashSet<BoundId>,
    /// The boundaries that read the time, by when it next reads differently for them: the
    /// first is when the view next needs to run without anything else changing.
    times: BTreeSet<(i64, BoundId)>,
}

impl Index {
    fn add(&mut self, id: BoundId, b: &Bound) {
        let mut docs: Vec<&str> = Vec::new();
        let reads = b
            .reads
            .iter()
            .map(|(resource, field)| (resource, field, Read::Field));
        let colls = b
            .colls
            .iter()
            .map(|seen| (&seen.resource, &seen.coll.field, Read::Items));
        let loops = b
            .loops
            .iter()
            .map(|rows| (&rows.resource, &rows.coll.field, Read::Rows));
        for (resource, field, read) in reads.chain(colls).chain(loops) {
            let index = self.of(read);
            let read = (field.clone(), id);
            match index.get_mut(resource) {
                Some(readers) => readers.push(read),
                None => {
                    index.insert(resource.clone(), vec![read]);
                }
            }
            let doc = doc_of(resource);
            if docs.last() != Some(&doc) && !docs.contains(&doc) {
                docs.push(doc);
            }
        }
        for doc in docs {
            match self.docs.get_mut(doc) {
                Some(ids) => {
                    ids.insert(id);
                }
                None => {
                    self.docs.insert(doc.to_owned(), HashSet::from([id]));
                }
            }
        }
        for (key, _) in &b.state_reads {
            self.states.entry(key.clone()).or_default().insert(id);
        }
        if !b.space_reads.is_empty() {
            self.space.insert(id);
        }
        if b.person_reads {
            self.people.insert(id);
        }
        if let Some(at) = b.changes_at {
            self.times.insert((at, id));
        }
    }

    fn remove(&mut self, id: BoundId, b: &Bound) {
        fn drop_from<K: std::hash::Hash + Eq>(
            map: &mut HashMap<K, HashSet<BoundId>>,
            key: &K,
            id: BoundId,
        ) {
            if let Some(set) = map.get_mut(key) {
                set.remove(&id);
                if set.is_empty() {
                    map.remove(key);
                }
            }
        }
        let reads = b.reads.iter().map(|(resource, _)| (resource, Read::Field));
        let colls = b.colls.iter().map(|seen| (&seen.resource, Read::Items));
        let loops = b.loops.iter().map(|rows| (&rows.resource, Read::Rows));
        for (resource, read) in reads.chain(colls).chain(loops) {
            let index = self.of(read);
            if let Some(readers) = index.get_mut(resource) {
                readers.retain(|(_, reader)| *reader != id);
                if readers.is_empty() {
                    index.remove(resource);
                }
            }
            if let Some(ids) = self.docs.get_mut(doc_of(resource)) {
                ids.remove(&id);
                if ids.is_empty() {
                    self.docs.remove(doc_of(resource));
                }
            }
        }
        for (key, _) in &b.state_reads {
            drop_from(&mut self.states, key, id);
        }
        self.space.remove(&id);
        self.people.remove(&id);
        if let Some(at) = b.changes_at {
            self.times.remove(&(at, id));
        }
    }

    fn of(&mut self, read: Read) -> &mut FastMap<String, Vec<(String, BoundId)>> {
        match read {
            Read::Field => &mut self.resources,
            Read::Items => &mut self.colls,
            Read::Rows => &mut self.loops,
        }
    }

    /// The boundaries that read `field` of `resource`, or any field of it for [`ANY_FIELD`].
    fn readers<'a>(&'a self, resource: &str, field: &'a str) -> impl Iterator<Item = BoundId> + 'a {
        self.resources
            .get(resource)
            .into_iter()
            .flatten()
            .filter(move |(read, _)| field == ANY_FIELD || read == field)
            .map(|(_, id)| *id)
    }

    /// The boundaries that read the collection `field` of `resource` item by item, or any
    /// collection of it for [`ANY_FIELD`].
    fn coll_readers<'a>(
        &'a self,
        resource: &str,
        field: &'a str,
    ) -> impl Iterator<Item = (BoundId, &'a str)> + 'a {
        Self::in_map(&self.colls, resource, field)
    }

    /// The boundaries that hold the rows of the collection `field` of `resource`, or of any
    /// collection of it for [`ANY_FIELD`].
    fn loop_holders<'a>(
        &'a self,
        resource: &str,
        field: &'a str,
    ) -> impl Iterator<Item = (BoundId, &'a str)> + 'a {
        Self::in_map(&self.loops, resource, field)
    }

    fn in_map<'a>(
        map: &'a FastMap<String, Vec<(String, BoundId)>>,
        resource: &str,
        field: &'a str,
    ) -> impl Iterator<Item = (BoundId, &'a str)> + 'a {
        map.get(resource)
            .into_iter()
            .flatten()
            .filter(move |(read, _)| field == ANY_FIELD || read == field)
            .map(|(read, id)| (*id, read.as_str()))
    }
}

/// How a boundary depends on a field.
#[derive(Clone, Copy)]
enum Read {
    /// It read the field.
    Field,
    /// It read the collection item by item ([`CollSeen`]).
    Items,
    /// It holds the collection's rows ([`LoopRows`]).
    Rows,
}

/// Steps of a collection's listing for the rows of a loop to take, from one change.
struct Pending {
    holder: BoundId,
    rows: Rc<LoopRows>,
    steps: Steps,
    /// The change is the viewer's own: a row of theirs that its steps take out of a query
    /// lingers where it is (see [`crate::live::virtual_rows`]).
    mine: bool,
}

/// `pending`, the items several changes moved in one query's rows taken as one step: where each
/// was is read from the listing the rows stand for, and where it is from the listing now, so
/// the changes between are one.
fn coalesce_moved(pending: Vec<Pending>) -> Vec<Pending> {
    let mut out: Vec<Pending> = Vec::with_capacity(pending.len());
    for step in pending {
        if let Steps::Moved(ids) = &step.steps
            && let Some(Steps::Moved(held)) = out
                .iter_mut()
                .find(|held| {
                    Rc::ptr_eq(&held.rows, &step.rows)
                        && held.mine == step.mine
                        && matches!(held.steps, Steps::Moved(_))
                })
                .map(|held| &mut held.steps)
        {
            held.extend(ids.iter().cloned());
            continue;
        }
        out.push(step);
    }
    out
}

/// What one change did to the listing a loop's rows are made of.
enum Steps {
    /// The collection's steps, the rows being its items.
    Splices(Vec<Splice>),
    /// The items that came, went or moved in a query of the collection (filtered or sorted),
    /// whose rows are a virtual list's: the store says where each was and where it is
    /// ([`crate::store::moved_steps`]), and the others keep their order.
    Moved(BTreeSet<String>),
}

/// What a change reaches: the boundaries to re-run, and the steps the rows of loops take.
#[derive(Default)]
struct Reach {
    hit: Vec<BoundId>,
    steps: Vec<Pending>,
    /// The items the person looking changed, when the change is theirs: a list of values keeps
    /// the rows of those items where they stand ([`crate::live::virtual_rows`]).
    mine: std::collections::BTreeSet<String>,
}

/// What a view instance counts, for tests and the benchmark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveStats {
    /// Boundaries rendered (a view call run), since the view was opened.
    pub renders: u64,
    /// Of those, the runs of the view's root: how many times a change reached the window.
    pub root_runs: u64,
    /// Boundaries in the tree now.
    pub boundaries: usize,
    /// Of those, rows of lists not rendered because they were not shown.
    pub unrendered: usize,
    /// What the boundaries in the tree depend on: resources and fields, collections, spaces
    /// and state, as their views read them. The cost of a change reaching them, whatever the
    /// workspace holds: a header's mark reads the same whether the window lists ten items or
    /// ten thousand.
    pub reads: usize,
}

/// The tree, its boundaries and what they depend on.
pub(crate) struct Live {
    pub tree: UiNode,
    bounds: Vec<Option<Bound>>,
    free: Vec<BoundId>,
    by_key: HashMap<String, BoundId>,
    index: Index,
    dirty: BTreeSet<BoundId>,
    pub space: Space,
    pub viewer: Principal,
    pub window: String,
    /// Whether rows of lists wait to be shown before they render.
    pub lazy_rows: bool,
    pub renders: u64,
    /// Of those, the runs of the root: how many times a change reached the window.
    pub root_runs: u64,
    /// View calls kept without running during the current re-render, by key.
    reused: HashSet<String>,
    /// Action references of trees replaced during the current re-render: released once the
    /// diff no longer compares against them.
    to_release: Vec<Retained>,
    /// The rows of each list (by its key) rendered because the host showed them.
    shown: HashMap<String, HashSet<BoundId>>,
    /// Where each list the host asked about was last found: checked before use, searched for
    /// again when the tree moved it.
    list_paths: RefCell<HashMap<String, Vec<usize>>>,
    /// Where each other node [`Self::find`] walked the tree for was last found, checked the same
    /// way: a host edits and presses the same few nodes again and again.
    found_paths: RefCell<HashMap<String, Vec<usize>>>,
    /// The view-state clock last checked.
    state_clock: u64,
    /// Steps heard for the rows of loops, applied at the next poll.
    pending: Vec<Pending>,
    /// The items the person looking changed in this window since the last poll: the rows of a
    /// list of values their own change took out of it stand where they are until the view is
    /// left ([`crate::live::virtual_rows`]).
    mine_items: std::collections::BTreeSet<String>,
    /// The virtual lists, by key: lists whose rows are built when the host shows them.
    lists: HashMap<String, Virtual>,
    /// What became of the rows of the virtual lists met by the render under way, for its diff.
    row_changes: VirtualLists,
}

pub(crate) const ROOT: BoundId = 0;

impl Live {
    pub fn open(
        core: &Rc<Core>,
        (viewer, window): (Principal, String),
        target: ViewTarget,
        args: Vec<Value>,
    ) -> Result<Self, RuntimeError> {
        let root = Bound {
            key: String::new(),
            target: Rc::new(target),
            args,
            depth: 0,
            children: Vec::new(),
            path: Vec::new(),
            rendered: false,
            lazy: false,
            reads: Vec::new(),
            colls: Vec::new(),
            loops: Vec::new(),
            space_reads: Vec::new(),
            person_reads: false,
            state_reads: Vec::new(),
            state_seen: Vec::new(),
            changes_at: None,
            says_new: false,
            open: false,
            retained: Vec::new(),
        };
        let mut live = Self {
            tree: UiNode::empty("fragment", ""),
            bounds: vec![Some(root)],
            free: Vec::new(),
            by_key: HashMap::new(),
            index: Index::default(),
            dirty: BTreeSet::new(),
            space: Space::default(),
            viewer,
            window,
            lazy_rows: true,
            renders: 0,
            root_runs: 0,
            reused: HashSet::new(),
            to_release: Vec::new(),
            shown: HashMap::new(),
            list_paths: RefCell::default(),
            found_paths: RefCell::default(),
            state_clock: core.view_states.borrow().clock(),
            pending: Vec::new(),
            mine_items: std::collections::BTreeSet::new(),
            lists: HashMap::new(),
            row_changes: VirtualLists::new(),
        };
        let tree = live.render(core, ROOT)?;
        live.tree = tree;
        live.reused.clear();
        live.row_changes.clear();
        live.refresh_paths(ROOT);
        live.release(core);
        Ok(live)
    }

    pub fn bound(&self, id: BoundId) -> Option<&Bound> {
        self.bounds.get(id).and_then(Option::as_ref)
    }

    /// Take `reads` into what the root boundary read, after it ran: a host converting what the
    /// view shows reads more of the workspace than the view did (a record whose content a card
    /// of it shows), and tells the view, so a change to it runs the root again and the host
    /// converts again. The reads are the root's until it runs again, which replaces them with
    /// what it read then; the host registers its own again after converting.
    pub fn shows(&mut self, reads: impl IntoIterator<Item = (String, String)>) {
        let Some(mut b) = self.bounds.get_mut(ROOT).and_then(Option::take) else {
            return;
        };
        let mut added = false;
        for read in reads {
            if !b.reads.contains(&read) {
                b.reads.push(read);
                added = true;
            }
        }
        if added {
            self.index.add(ROOT, &b);
        }
        if let Some(slot) = self.bounds.get_mut(ROOT) {
            *slot = Some(b);
        }
    }

    /// Plant a boundary by hand, as a boundary whose branch has left the tree stands: rendered,
    /// with the cached trail `path` of a call that is no longer placed. `target` is a call that
    /// renders, so a run of it has content to write: without the guard that refuses a boundary
    /// the tree does not hold, that content lands at `path`.
    #[cfg(test)]
    pub(crate) fn plant_orphan(
        &mut self,
        key: &str,
        target: crate::render::ViewTarget,
        args: Vec<Value>,
        path: Vec<usize>,
    ) -> BoundId {
        let mut bound = Bound::new(
            crate::render::Stub {
                key,
                target: Rc::new(target),
                args,
                lazy: false,
                open: false,
                depth: 1,
            },
            false,
        );
        bound.path = path;
        bound.rendered = true;
        let id = self.add_bound(bound);
        self.dirty.insert(id);
        id
    }

    /// The keys of the tree's children, to say what a tree holds.
    #[cfg(test)]
    pub(crate) fn child_keys(&self) -> Vec<String> {
        self.tree.children.iter().map(|n| n.key.clone()).collect()
    }

    fn bound_mut(&mut self, id: BoundId) -> Option<&mut Bound> {
        self.bounds.get_mut(id).and_then(Option::as_mut)
    }

    /// The documents some boundary read.
    pub fn docs(&self) -> impl Iterator<Item = &String> {
        self.index.docs.keys()
    }

    pub fn stats(&self) -> LiveStats {
        let boundaries = self.bounds.iter().flatten().count();
        let unrendered = self.bounds.iter().flatten().filter(|b| !b.rendered).count();
        let reads = self
            .bounds
            .iter()
            .flatten()
            .map(|b| {
                [
                    b.reads.len(),
                    b.colls.len(),
                    b.loops.len(),
                    b.space_reads.len(),
                    usize::from(b.person_reads),
                    b.state_reads.len(),
                ]
                .into_iter()
                .fold(0usize, usize::saturating_add)
            })
            .fold(0usize, usize::saturating_add);
        LiveStats {
            renders: self.renders,
            root_runs: self.root_runs,
            boundaries,
            unrendered,
            reads,
        }
    }

    /// The resources the window shows now, by [`ResourceRef::id_string`]: the views' own
    /// subjects, the items of the rows rendered, and the items the virtual lists built (the
    /// visible rows and the overscan). What an app publishes for the window to act on: a
    /// surface bounded by what a person can reach, whatever the workspace holds.
    pub fn shown_refs(&self, core: &Core) -> Vec<String> {
        let mut out = BTreeSet::new();
        let collect = |out: &mut BTreeSet<String>, args: &[Value]| {
            for arg in args {
                if let Value::Ref(held) = arg {
                    out.insert(held.target.id_string());
                }
            }
        };
        if let Some(root) = self.bound(ROOT) {
            collect(&mut out, &root.args);
        }
        for id in self.shown.values().flatten() {
            if let Some(bound) = self.bound(*id) {
                collect(&mut out, &bound.args);
            }
        }
        for list in self.lists.values() {
            for item in list.shown_items(core) {
                if let Value::Ref(held) = &item {
                    out.insert(held.target.id_string());
                }
            }
        }
        out.into_iter().collect()
    }

    // ----- invalidation ----------------------------------------------------------------------

    /// What a change reaches: the boundaries that read a field it changed, those that saw
    /// something of a collection it changed that is no longer so, and the steps the rows of
    /// loops over a collection it changed take. Rows whose steps the store could not tell are
    /// built again by their boundary.
    fn reached(&self, core: &Core, change: &ChangeSummary) -> Reach {
        let mut reach = Reach::default();
        // What the person looking did in this window, as opposed to anyone else's change:
        // the rows of a list they changed themselves linger ([`crate::live::virtual_rows`]).
        let mine = change.by.as_deref() == Some(self.viewer.0.as_str());
        if mine {
            for key in &change.fields {
                if let Some((_, item)) = key.resource.split_once('/') {
                    reach.mine.insert(item.to_owned());
                }
            }
        }
        // A window's change mark counts every version in its document: any change there is a
        // new one. A change that names its items' versions and nothing else names versions the
        // window was told of before, provisional, that the record holds now: whoever reads an
        // item's versions reads them again, and the count stands.
        let renamed = !change.fields.is_empty()
            && change
                .fields
                .iter()
                .all(|key| key.resource != change.doc && key.field == crate::marks::DOC_VERSIONS);
        if !renamed {
            reach
                .hit
                .extend(self.index.readers(&change.doc, crate::marks::DOC_VERSIONS));
        }
        if change.fields.is_empty() {
            if let Some(set) = self.index.docs.get(&change.doc) {
                reach.hit.extend(set.iter().copied());
            }
            return reach;
        }
        for key in &change.fields {
            reach
                .hit
                .extend(self.index.readers(&key.resource, &key.field));
            self.reach_colls(core, &key.resource, &key.field, false, &mut reach);
            for (id, field) in self.index.loop_holders(&key.resource, &key.field) {
                let held = self.bound(id).into_iter().flat_map(|b| &b.loops);
                for rows in held.filter(|rows| rows.coll.field == field) {
                    match change.listing(key) {
                        // Nothing moved for a reader that had read the collection since the
                        // write: the store measured the steps from the mirror the write left
                        // open, which every read of it holds. A reader that read the collection
                        // before the write has not seen the items it placed, and the change
                        // names them: each goes where the listing has it now, found by what
                        // orders the listing rather than by reading it item by item.
                        Some([]) if self.lists.contains_key(&rows.parent) => {
                            let named: BTreeSet<String> = change
                                .fields
                                .iter()
                                .filter_map(|named| {
                                    let (holder, field, item) = item_in(core, &named.resource)?;
                                    (holder == key.resource && field == key.field).then_some(item)
                                })
                                .collect();
                            if !named.is_empty() {
                                reach.steps.push(Pending {
                                    holder: id,
                                    rows: Rc::clone(rows),
                                    steps: Steps::Moved(named),
                                    mine,
                                });
                            }
                        }
                        Some([]) => {}
                        Some(splices) if key.field != ANY_FIELD && !rows.coll.derived() => {
                            reach.steps.push(Pending {
                                holder: id,
                                rows: Rc::clone(rows),
                                steps: Steps::Splices(splices.to_vec()),
                                mine,
                            });
                        }
                        // The steps are the collection's; a filtered or sorted one's rows are
                        // not its items. A virtual list's find where the items the steps name
                        // are in the query; others are built again by their boundary.
                        Some(splices)
                            if key.field != ANY_FIELD && self.lists.contains_key(&rows.parent) =>
                        {
                            let moved = splices.iter().map(|s| s.item().to_owned()).collect();
                            reach.steps.push(Pending {
                                holder: id,
                                rows: Rc::clone(rows),
                                steps: Steps::Moved(moved),
                                mine,
                            });
                        }
                        _ => reach.hit.push(id),
                    }
                }
            }
            // A field of an item of a collection decides whether the item is in a filtered
            // view of it, and where in a sorted one: the readers of those views look again,
            // and the rows of a virtual list over one find where the item is now.
            if let Some((parent, coll, item)) = item_in(core, &key.resource) {
                let (parent, coll) = (parent.as_str(), coll.as_str());
                self.reach_colls(core, parent, coll, true, &mut reach);
                for (id, field) in self.index.loop_holders(parent, coll) {
                    let held = self.bound(id).into_iter().flat_map(|b| &b.loops);
                    for rows in held
                        .filter(|rows| rows.coll.field == field)
                        .filter(|rows| rows.coll.reads_field(&key.field))
                    {
                        if rows.coll.derived() && self.lists.contains_key(&rows.parent) {
                            reach.steps.push(Pending {
                                holder: id,
                                rows: Rc::clone(rows),
                                steps: Steps::Moved(BTreeSet::from([item.clone()])),
                                mine,
                            });
                        } else {
                            reach.hit.push(id);
                        }
                    }
                }
            }
        }
        reach
    }

    /// The boundaries that saw something of the collection `field` of `resource` (or of any
    /// collection of it, for [`ANY_FIELD`]) that no longer holds: of its filtered views alone
    /// when `filtered`.
    fn reach_colls(
        &self,
        core: &Core,
        resource: &str,
        field: &str,
        filtered: bool,
        reach: &mut Reach,
    ) {
        // Every row of a list reads the same listing: it is read once for all of them, not once
        // a row.
        let mut listings: Vec<(Rc<crate::value::CollRef>, Option<Rc<crate::store::Listing>>)> =
            Vec::new();
        for (id, coll) in self.index.coll_readers(resource, field) {
            let Some(b) = self.bound(id) else {
                continue;
            };
            let stale = b
                .colls
                .iter()
                .filter(|seen| seen.resource == resource && seen.coll.field == coll)
                .filter(|seen| !filtered || seen.coll.derived())
                .any(|seen| {
                    let at = listings
                        .iter()
                        .position(|(held, _)| seen.is_of(held))
                        .unwrap_or_else(|| {
                            let listing =
                                crate::colls::listing_of(&**core.store.borrow(), &seen.coll);
                            listings.push((Rc::clone(&seen.coll), listing.ok()));
                            listings.len().saturating_sub(1)
                        });
                    !listings
                        .get(at)
                        .and_then(|(_, listing)| listing.as_ref())
                        .is_some_and(|listing| seen.holds(listing))
                });
            if stale {
                reach.hit.push(id);
            }
        }
    }

    /// Whether a change reaches some boundary or the rows of some loop.
    pub fn reaches(&self, core: &Core, change: &ChangeSummary) -> bool {
        let reach = self.reached(core, change);
        !reach.hit.is_empty() || !reach.steps.is_empty()
    }

    /// Whether a view state slot some boundary read moved since.
    pub fn states_moved(&self, core: &Core) -> bool {
        let states = core.view_states.borrow();
        if states.clock() == self.state_clock {
            return false;
        }
        self.index.states.iter().any(|(key, ids)| {
            let now = states.generation(key);
            ids.iter().any(|id| {
                self.bound(*id).is_some_and(|b| {
                    b.state_reads
                        .iter()
                        .any(|(k, seen)| k == key && *seen != now)
                })
            })
        })
    }

    /// Mark the boundaries a change reaches.
    pub fn absorb(&mut self, core: &Core, change: &ChangeSummary) {
        let reach = self.reached(core, change);

        self.dirty.extend(reach.hit);
        self.pending.extend(reach.steps);
        self.mine_items.extend(reach.mine);
    }

    /// Mark the boundaries whose view state moved since they read it.
    pub fn check_states(&mut self, core: &Core) {
        let states = core.view_states.borrow();
        let clock = states.clock();
        if clock == self.state_clock {
            return;
        }
        self.state_clock = clock;
        let mut left: Vec<(BoundId, &StateKey)> = Vec::new();
        for (key, ids) in &self.index.states {
            let now = states.generation(key);
            for id in ids {
                let moved = self.bound(*id).is_some_and(|b| {
                    b.state_reads
                        .iter()
                        .any(|(k, seen)| k == key && *seen != now)
                });
                if moved {
                    left.push((*id, key));
                }
            }
        }
        drop(states);
        // The state a view declares is what its rows stand in: moving it leaves that view, and
        // the rows the person kept for looking at it settle
        // ([`crate::live::virtual_rows`]).
        let settling = self.left_lists(core, &left);
        let hit: Vec<BoundId> = left.into_iter().map(|(id, _)| id).collect();
        self.settle_kept(&settling);
        self.dirty.extend(hit);
    }

    /// Mark the boundaries that read a `space` value that differs in `space`.
    pub fn check_space(&mut self, space: Space) {
        self.space = space;
        let hit: Vec<BoundId> = self
            .index
            .space
            .iter()
            .copied()
            .filter(|id| {
                self.bound(*id).is_some_and(|b| {
                    b.space_reads
                        .iter()
                        .any(|(name, seen)| space.field(name) != Some(*seen))
                })
            })
            .collect();
        self.dirty.extend(hit);
    }

    /// When the time some boundary read next reads differently, in seconds since 1970 (UTC):
    /// the view runs again then ([`Self::pass_time`]). `None` when no boundary read the time,
    /// and then nothing ever wakes the view for it.
    pub fn wakes_at(&self) -> Option<i64> {
        self.index.times.first().map(|(at, _)| *at)
    }

    /// The clock reads `now` (seconds since 1970, UTC): mark the boundaries for which the time
    /// they read reads differently by now, and only those. Each is taken off the times it waits
    /// for: it waits again for what its next run reads, and a run that fails leaves it waiting
    /// for nothing rather than due forever.
    pub fn pass_time(&mut self, now: i64) {
        while let Some(&(at, id)) = self.index.times.first() {
            if at > now {
                break;
            }
            self.index.times.remove(&(at, id));
            if let Some(b) = self.bound_mut(id) {
                b.changes_at = None;
            }
            self.dirty.insert(id);
        }
    }

    /// Mark the boundaries that show something only the window knows: the person chose another
    /// language, another clock or another date style, or the window calls a participant
    /// something else, and what shows one shows it again. A view that shows none is not run.
    pub fn person_moved(&mut self) {
        let hit: Vec<BoundId> = self.index.people.iter().copied().collect();
        self.dirty.extend(hit);
    }

    /// The value a view state slot had when a boundary last read it, its default included.
    pub fn state_seen(&self, key: &StateKey) -> Option<Value> {
        self.bounds.iter().flatten().find_map(|b| {
            b.state_seen
                .iter()
                .rev()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        })
    }

    pub fn is_dirty(&self) -> bool {
        !self.dirty.is_empty() || !self.pending.is_empty()
    }

    // ----- rendering -------------------------------------------------------------------------

    /// Run boundary `id` and build its tree, re-running only the view calls inside it that
    /// changed. Returns the tree; the boundary's record is updated.
    fn render(&mut self, core: &Rc<Core>, id: BoundId) -> Result<UiNode, RuntimeError> {
        let (target, args, depth, key, old_children, row) = {
            let b = self
                .bound(id)
                .ok_or_else(|| RuntimeError::Package("a vanished boundary".into()))?;
            (
                b.target.clone(),
                b.args.clone(),
                b.depth,
                b.key.clone(),
                b.children.clone(),
                b.lazy.then(|| b.path.clone()),
            )
        };
        // A row of a `list` that stands a divider where its unseen part starts: the divider
        // already says which rows are new, so a row there says it no further.
        let says_new = row.as_deref().is_some_and(|path| self.row_says_new(path));
        let open = self.bound(id).is_some_and(|b| b.open);
        let old: HashMap<String, BoundId> = old_children
            .iter()
            .filter_map(|c| self.bound(*c).map(|b| (b.key.clone(), *c)))
            .collect();
        let viewer = self.viewer.clone();
        let window = self.window.clone();
        let offer = Offer {
            space: self.space,
            viewer: &viewer,
            window: &window,
            depth,
        };
        let mut builder = Builder {
            live: self,
            core,
            id,
            old,
            new: Vec::new(),
            loops: Vec::new(),
        };
        let prefix = (id != ROOT).then_some(key.as_str());
        // A call that fails for want of a reading that has not arrived draws its waiting state,
        // and one that cannot be shown at all draws a failed state naming it; either way the
        // rest of the tree stands, so one place's failure causes no other place to fail.
        let rendered = core.render_boundary_waiting(
            &key,
            &target,
            &args,
            offer,
            Some(Scoped {
                prefix,
                says_new,
                open,
                boundaries: Some(&mut builder),
            }),
        );
        let Builder {
            old, new, loops, ..
        } = builder;
        // Calls that are gone take their subtrees with them.
        let kept: HashSet<BoundId> = new.iter().copied().collect();
        for gone in old.values().filter(|c| !kept.contains(c)) {
            self.drop_bound(*gone);
        }
        let rendered = rendered?;
        self.renders = self.renders.saturating_add(1);
        if id == ROOT {
            self.root_runs = self.root_runs.saturating_add(1);
        }
        let Rendered {
            root,
            reads,
            colls,
            space_reads,
            person_reads,
            retained,
            state_reads,
            state_seen,
            changes_at,
        } = rendered;
        // A view over a list reads each item's fields once per expression that uses them (a
        // count and a filter), tens of thousands of reads: duplicates go by hash, not a sort.
        let reads = distinct(reads);
        if let Some(mut b) = self.bounds.get_mut(id).and_then(Option::take) {
            self.index.remove(id, &b);
            self.to_release.append(&mut b.retained);
            b.children = new;
            b.reads = reads;
            b.colls = colls;
            b.loops = loops;
            b.space_reads = space_reads;
            b.person_reads = person_reads;
            b.state_reads = state_reads;
            b.state_seen = state_seen;
            b.changes_at = changes_at;
            b.retained = retained;
            b.says_new = says_new;
            b.rendered = true;
            self.index.add(id, &b);
            if let Some(slot) = self.bounds.get_mut(id) {
                *slot = Some(b);
            }
        }
        self.dirty.remove(&id);
        Ok(root)
    }

    fn add_bound(&mut self, b: Bound) -> BoundId {
        let key = b.key.clone();
        let id = if let Some(id) = self.free.pop() {
            if let Some(slot) = self.bounds.get_mut(id) {
                *slot = Some(b);
            }
            id
        } else {
            self.bounds.push(Some(b));
            self.bounds.len().saturating_sub(1)
        };
        self.by_key.insert(key, id);
        id
    }

    /// Forget a boundary and every boundary inside it.
    fn drop_bound(&mut self, id: BoundId) {
        let Some(mut b) = self.bounds.get_mut(id).and_then(Option::take) else {
            return;
        };
        self.index.remove(id, &b);
        self.dirty.remove(&id);
        for rows in self.shown.values_mut() {
            rows.remove(&id);
        }
        self.to_release.append(&mut b.retained);
        if self.by_key.get(&b.key) == Some(&id) {
            self.by_key.remove(&b.key);
        }
        self.forget_rows_of(id);
        self.free.push(id);
        for child in b.children {
            self.drop_bound(child);
        }
    }

    /// Forget what a boundary rendered: it renders again when it is shown.
    fn unrender(&mut self, id: BoundId) {
        let children = match self.bound(id) {
            Some(b) => b.children.clone(),
            None => return,
        };
        for child in children {
            self.drop_bound(child);
        }
        self.lists.retain(|_, list| list.holder != id);
        if let Some(mut b) = self.bounds.get_mut(id).and_then(Option::take) {
            self.index.remove(id, &b);
            self.to_release.append(&mut b.retained);
            b.children.clear();
            b.reads.clear();
            b.colls.clear();
            b.loops.clear();
            b.space_reads.clear();
            b.person_reads = false;
            b.state_reads.clear();
            b.changes_at = None;
            b.rendered = false;
            if let Some(slot) = self.bounds.get_mut(id) {
                *slot = Some(b);
            }
        }
        self.dirty.remove(&id);
    }

    pub fn release(&mut self, core: &Core) {
        let mut frames = core.frames.borrow_mut();
        for r in self.to_release.drain(..) {
            frames.release(r);
        }
    }

    /// Release everything the view holds.
    pub fn release_all(&mut self, core: &Core) {
        for b in self.bounds.iter_mut().flatten() {
            self.to_release.append(&mut b.retained);
        }
        self.release(core);
    }

    // ----- the tree --------------------------------------------------------------------------

    fn node_at(&self, path: &[usize]) -> Option<&UiNode> {
        path.iter()
            .try_fold(&self.tree, |node, i| node.children.get(*i))
    }

    fn node_at_mut(&mut self, path: &[usize]) -> Option<&mut UiNode> {
        path.iter()
            .try_fold(&mut self.tree, |node, i| node.children.get_mut(*i))
    }

    /// The keys from the root down to the node at `path`, the node included.
    fn keys_at(&self, path: &[usize]) -> Vec<String> {
        let mut out = vec![self.tree.key.clone()];
        let mut node = &self.tree;
        for i in path {
            let Some(next) = node.children.get(*i) else {
                break;
            };
            out.push(next.key.clone());
            node = next;
        }
        out
    }

    /// Record where each boundary inside `id` (itself included) sits now.
    fn refresh_paths(&mut self, id: BoundId) {
        let Some(path) = self.bound(id).map(|b| b.path.clone()) else {
            return;
        };
        let mut found: Vec<(BoundId, Vec<usize>)> = Vec::new();
        if let Some(node) = self.node_at(&path) {
            let mut at = path.clone();
            collect_paths(&self.by_key, node, &mut at, &mut found, id == ROOT);
        }
        let mut ahead: Vec<BoundId> = Vec::new();
        for (child, path) in found {
            // Only a row answers to the divider over it: it is built again when one comes or
            // goes, so a mark it drew for being new goes with it.
            let says_new = self.row_says_new(&path);
            let Some(b) = self.bound_mut(child) else {
                continue;
            };
            if b.lazy {
                if b.says_new != says_new {
                    ahead.push(child);
                }
                b.says_new = says_new;
            }
            b.path = path;
        }
        self.dirty.extend(ahead);
    }

    /// Whether the row at `path` stands in the unseen part of its list, under the divider where
    /// that part starts: the divider says which rows are new, so a row there says it no further.
    ///
    /// A list holds only the rows built, so a row's node stands at its place among them: what
    /// it is a row of is the index the list gave it ([`crate::live::virtual_rows`]).
    fn row_says_new(&self, path: &[usize]) -> bool {
        let Some((&place, above)) = path.split_last() else {
            return false;
        };
        let Some(list) = self.node_at(above) else {
            return false;
        };
        if !crate::view::is_virtual(&list.kind) {
            return false;
        }
        let Some(Value::Ref(unseen)) = list.props.get(crate::render::UNSEEN_FROM) else {
            return false;
        };
        let Some(item) = unseen.target.item.as_deref() else {
            return false;
        };
        let Some(rows) = self.lists.get(&list.key) else {
            return false;
        };
        let (Some(start), Some(index)) = (rows.place(item), rows.row(place)) else {
            return false;
        };
        index >= start
    }

    /// Whether the `fragment` of boundary `id`, at `path`, is still the node the tree holds
    /// there for it: a `fragment` keyed by the boundary's own key, and whose key this boundary
    /// is the one that stands for. False for a boundary whose branch of the view has since
    /// left the tree, which has nowhere to write.
    ///
    /// The key alone is not enough. Two calls of one view can stand at once (a view drawn in
    /// two branches, a row called twice), and a boundary freed and handed to another call can
    /// keep a key the tree now holds elsewhere. Asking [`Self::by_key`] whose the fragment is,
    /// and refusing when it is not this boundary's, keeps a stale boundary from writing over
    /// the content the boundary standing there now owns.
    fn placed(&self, id: BoundId, path: &[usize]) -> bool {
        let Some(b) = self.bound(id) else {
            return false;
        };
        self.by_key.get(&b.key) == Some(&id)
            && self
                .node_at(path)
                .is_some_and(|node| node.kind == "fragment" && node.key == b.key)
    }

    /// Re-run every boundary that is stale, outermost first, and return the patches that
    /// bring the tree up to date.
    pub fn poll(&mut self, core: &Rc<Core>) -> Result<Vec<Patch>, RuntimeError> {
        let mut patches = Vec::new();
        self.follow_collections(core, &mut patches)?;
        while let Some(id) = self.next_dirty() {
            self.rerender(core, id, &mut patches)?;
        }
        self.mine_items.clear();
        Ok(patches)
    }

    /// The stale boundary to re-run next: the shallowest rendered one. A stale row that is not
    /// shown just waits.
    fn next_dirty(&mut self) -> Option<BoundId> {
        let mut best: Option<(usize, BoundId)> = None;
        let mut stale = Vec::new();
        for id in &self.dirty {
            match self.bound(*id) {
                Some(b) if b.rendered => {
                    if best.is_none_or(|(depth, _)| b.depth < depth) {
                        best = Some((b.depth, *id));
                    }
                }
                _ => stale.push(*id),
            }
        }
        for id in stale {
            self.dirty.remove(&id);
        }
        best.map(|(_, id)| id)
    }

    fn rerender(
        &mut self,
        core: &Rc<Core>,
        id: BoundId,
        patches: &mut Vec<Patch>,
    ) -> Result<(), RuntimeError> {
        let Some(path) = self.bound(id).map(|b| b.path.clone()) else {
            return Ok(());
        };
        // A boundary writes into its own `fragment`, and nowhere else. Its path is a cached
        // trail of child indices, so it can outlive the node it named: when a branch of the
        // view swaps (a preset that lays the desk another way), the `fragment` the boundary
        // was made for leaves the tree while the boundary itself stays behind, rendered and
        // reading what changed. Its path is then the empty trail of a boundary that was never
        // placed, and an empty path names the root: writing there would put this one call's
        // content where the whole view's belongs, and the screen would show that call alone.
        // A boundary whose fragment the tree no longer holds has nothing to update; what is
        // drawn is drawn by whichever boundary holds the branch now, and the call makes a
        // boundary anew when the branch comes back.
        if id != ROOT && !self.placed(id, &path) {
            self.drop_bound(id);
            return Ok(());
        }
        self.reused.clear();
        self.row_changes.clear();
        let new_content = match self.render(core, id) {
            Ok(tree) => tree,
            Err(e) => {
                self.dirty.remove(&id);
                self.row_changes.clear();
                self.release(core);
                return Err(e);
            }
        };
        let reused = std::mem::take(&mut self.reused);
        let row_changes = std::mem::take(&mut self.row_changes);
        // The node under the boundary: the whole tree for the root, else the fragment's child.
        let (parent_keys, old) = if id == ROOT {
            (
                Vec::new(),
                std::mem::replace(&mut self.tree, UiNode::empty("fragment", "")),
            )
        } else {
            let keys = self.keys_at(&path);
            let Some(fragment) = self.node_at_mut(&path) else {
                return Ok(());
            };
            let old = std::mem::take(&mut fragment.children);
            (
                keys,
                old.into_iter()
                    .next()
                    .unwrap_or_else(|| UiNode::empty("fragment", "")),
            )
        };
        let mut old = old;
        let mut kept: HashMap<String, Vec<UiNode>> = HashMap::new();
        if !reused.is_empty() {
            take_reused(&mut old, &reused, &mut kept);
        }
        let mut new_content = new_content;
        let same = |a: Retained, b: Retained| {
            let frames = core.frames.borrow();
            frames.retained(a) == frames.retained(b)
        };
        if id == ROOT {
            patches.extend(crate::view::diff_with(
                &old,
                &new_content,
                &same,
                &row_changes,
            ));
        } else {
            diff_at(
                &parent_keys,
                &old,
                &new_content,
                (&same, &row_changes),
                patches,
            );
        }
        if !kept.is_empty() {
            put_reused(&mut new_content, &mut kept);
        }
        if id == ROOT {
            self.tree = new_content;
        } else if let Some(fragment) = self.node_at_mut(&path) {
            fragment.children = vec![new_content];
        }
        self.refresh_paths(id);
        self.release(core);
        Ok(())
    }

    // ----- rows that follow their collection ------------------------------------------------

    /// Apply the steps heard for the rows of loops: each row inserted or removed is a patch, and
    /// the boundary that holds the loop does not run. Rows whose steps do not fit what they
    /// hold, or whose count then differs from the collection's, are built again by their
    /// boundary instead.
    fn follow_collections(
        &mut self,
        core: &Rc<Core>,
        patches: &mut Vec<Patch>,
    ) -> Result<(), RuntimeError> {
        let pending = coalesce_moved(std::mem::take(&mut self.pending));
        let mut followed: Vec<(BoundId, Rc<LoopRows>)> = Vec::new();
        for step in pending {
            let held = self
                .bound(step.holder)
                .is_some_and(|b| b.loops.iter().any(|rows| Rc::ptr_eq(rows, &step.rows)));
            // The rows of a virtual list take their steps even when their holder runs again:
            // it then finds them as the listing is, and keeps them.
            let virtual_rows = self.lists.contains_key(&step.rows.parent);
            if !held || (self.dirty.contains(&step.holder) && !virtual_rows) {
                continue;
            }
            if !self.splice_rows(core, &step, patches)? {
                self.dirty.insert(step.holder);
                self.lose_rows(&step.rows.parent);
                continue;
            }
            if !followed
                .iter()
                .any(|(_, rows)| Rc::ptr_eq(rows, &step.rows))
            {
                followed.push((step.holder, step.rows));
            }
        }
        for (holder, rows) in followed {
            // A virtual list's rows stand for the listing as it is once they fit it, so they
            // are checked even when their holder runs again.
            let check = !self.dirty.contains(&holder) || self.lists.contains_key(&rows.parent);
            if check && !self.rows_fit(core, holder, &rows) {
                // The rows the host holds are the ones the list built, unchanged: the holder
                // runs again and gives the host the steps between them and the listing as it
                // is now, so its rows move rather than being read anew.
                self.dirty.insert(holder);
            }
        }
        self.release(core);
        Ok(())
    }

    /// Apply one change's steps to the rows of a loop. False when they do not fit the rows.
    fn splice_rows(
        &mut self,
        core: &Rc<Core>,
        step: &Pending,
        patches: &mut Vec<Patch>,
    ) -> Result<bool, RuntimeError> {
        let rows = &step.rows;
        if self.lists.contains_key(&rows.parent) {
            return self.splice_virtual(core, step, patches);
        }
        let Some(at) = self.node_path_in(step.holder, &rows.parent) else {
            return Ok(false);
        };
        let Steps::Splices(step_splices) = &step.steps else {
            return Ok(false);
        };
        let parent = self.keys_at(&at);
        let mut from = usize::MAX;
        let mut splices = step_splices.iter().peekable();
        while let Some(splice) = splices.next() {
            let patch = match splice {
                // A removal and an insertion of the same item: the row moves, kept as it is.
                Splice::Remove { at: index, item }
                    if splices.peek().is_some_and(
                        |next| matches!(next, Splice::Insert { item: to, .. } if to == item),
                    ) =>
                {
                    let Some(Splice::Insert { at: to, .. }) = splices.next() else {
                        return Ok(false);
                    };
                    let key = rows.row_key(item);
                    let Some(row) = self.take_row(&at, *index, &key) else {
                        return Ok(false);
                    };
                    match self.node_at_mut(&at) {
                        Some(node) if *to <= node.children.len() => node.children.insert(*to, row),
                        _ => return Ok(false),
                    }
                    from = from.min(*index).min(*to);
                    Patch::Move {
                        parent: parent.clone(),
                        key,
                        from: *index,
                        to: *to,
                    }
                }
                Splice::Remove { at: index, item } => {
                    let key = rows.row_key(item);
                    if self.take_row(&at, *index, &key).is_none() {
                        return Ok(false);
                    }
                    if let Some(&id) = self.by_key.get(&key) {
                        self.drop_bound(id);
                        if let Some(holder) = self.bound_mut(step.holder) {
                            holder.children.retain(|child| *child != id);
                        }
                    }
                    from = from.min(*index);
                    Patch::Remove {
                        parent: parent.clone(),
                        key,
                        at: None,
                    }
                }
                Splice::Insert { at: index, item } => {
                    let Some(node) = self.insert_row(core, step, (&at, *index), item)? else {
                        return Ok(false);
                    };
                    from = from.min(*index);
                    Patch::Insert {
                        parent: parent.clone(),
                        index: *index,
                        node,
                    }
                }
            };
            patches.push(patch);
        }
        if from != usize::MAX {
            self.renumber_rows(&at, rows, from, step_splices.len());
        }
        Ok(true)
    }

    /// Take the row keyed `key` out of the node at `at`, where it is the `index`th child.
    /// `None` when it is not there.
    fn take_row(&mut self, at: &[usize], index: usize, key: &str) -> Option<UiNode> {
        let node = self.node_at_mut(at)?;
        if node.children.get(index)?.key != key {
            return None;
        }
        Some(node.children.remove(index))
    }

    /// Insert the row of `item` as the `index`th child of the node at `at`, a boundary of
    /// `step`'s holder, rendered unless it waits to be shown. Returns its node, or `None`
    /// when it does not fit: there is no such place, or the item has a row already.
    fn insert_row(
        &mut self,
        core: &Rc<Core>,
        step: &Pending,
        (at, index): (&[usize], usize),
        item: &str,
    ) -> Result<Option<UiNode>, RuntimeError> {
        let rows = &step.rows;
        let (key, args) = rows.row(core, item);
        let len = self.node_at(at).map_or(0, |node| node.children.len());
        if index > len || self.by_key.contains_key(&key) {
            return Ok(None);
        }
        let lazy = rows.rows.lazy && self.lazy_rows;
        let path = [at, &[index]].concat();
        let stub = rows.rows.stub(&key, args);
        let mut bound = Bound::new(stub, lazy);
        bound.path.clone_from(&path);
        let id = self.add_bound(bound);
        if let Some(holder) = self.bound_mut(step.holder) {
            holder.children.push(id);
        }
        if let Some(node) = self.node_at_mut(at) {
            node.children
                .insert(index, rows.rows.fragment(key, Vec::new()));
        }
        if !lazy {
            let place = Some((Rc::clone(&rows.coll), index));
            self.render_row(core, id, place)?;
            if rows.rows.lazy {
                self.shown
                    .entry(rows.parent.clone())
                    .or_default()
                    .insert(id);
            }
        }
        Ok(self.node_at(&path).cloned())
    }

    /// Render the row `id` into its `fragment`, at its path, telling the view where it stands
    /// among the rows of `place`: the list's rows and the row's place in them.
    fn render_row(
        &mut self,
        core: &Rc<Core>,
        id: BoundId,
        place: Option<(Rc<crate::value::CollRef>, usize)>,
    ) -> Result<(), RuntimeError> {
        self.reused.clear();
        let outer = core.row_place.replace(place);
        let content = self.render(core, id);
        core.row_place.replace(outer);
        self.reused.clear();
        // A row's first render has nothing to diff: lists inside it start as they are.
        self.row_changes.clear();
        let content = content?;
        let path = self.bound(id).map(|b| b.path.clone()).unwrap_or_default();
        if let Some(fragment) = self.node_at_mut(&path) {
            fragment.children = vec![content];
        }
        self.refresh_paths(id);
        Ok(())
    }

    /// Bring the paths of the rendered rows of a loop up to date after steps at `from` and
    /// after moved each by at most `moved` places. Of a `list`'s rows only those shown are
    /// rendered: the work is in the rows on screen, not in the length of the list.
    fn renumber_rows(&mut self, at: &[usize], rows: &LoopRows, from: usize, moved: usize) {
        let depth = at.len();
        let Some(node) = self.node_at(at) else {
            return;
        };
        let place = |key: &str, hint: usize| {
            let near = hint.saturating_sub(moved)..=hint.saturating_add(moved);
            near.into_iter()
                .find(|i| node.children.get(*i).is_some_and(|row| row.key == key))
                .or_else(|| node.children.iter().position(|row| row.key == key))
        };
        let mut found: Vec<(BoundId, usize)> = Vec::new();
        if rows.rows.lazy && self.lazy_rows {
            for &id in self.shown.get(&rows.parent).into_iter().flatten() {
                let Some(b) = self.bound(id) else {
                    continue;
                };
                let hint = b.path.get(depth).copied().unwrap_or(from);
                if let Some(index) = place(&b.key, hint) {
                    found.push((id, index));
                }
            }
        } else {
            for (index, row) in node.children.iter().enumerate().skip(from) {
                if let Some(&id) = self.by_key.get(&row.key) {
                    found.push((id, index));
                }
            }
        }
        for (id, index) in found {
            self.set_index(id, depth, index);
        }
    }

    /// Set the step at `depth` of the paths of boundary `id` and those inside it to `index`.
    fn set_index(&mut self, id: BoundId, depth: usize, index: usize) {
        let children = match self.bound_mut(id) {
            Some(b) => {
                if let Some(step) = b.path.get_mut(depth) {
                    *step = index;
                }
                b.children.clone()
            }
            None => return,
        };
        for child in children {
            self.set_index(child, depth, index);
        }
    }

    /// Whether the rows of a loop are as many as its collection's items.
    fn rows_fit(&mut self, core: &Core, holder: BoundId, rows: &LoopRows) -> bool {
        if self.lists.contains_key(&rows.parent) {
            return self.virtual_fits(core, rows);
        }
        let Ok(listing) = crate::colls::listing_of(&**core.store.borrow(), &rows.coll) else {
            return false;
        };
        self.node_path_in(holder, &rows.parent)
            .and_then(|at| self.node_at(&at))
            .is_some_and(|node| node.children.len() == listing.len())
    }

    /// The path of the node keyed `key` in boundary `holder`'s tree: where it was last found if
    /// it is still there, else searched for.
    fn node_path_in(&self, holder: BoundId, key: &str) -> Option<Vec<usize>> {
        let cached = self.list_paths.borrow().get(key).cloned();
        if let Some(path) = cached.filter(|path| self.node_at(path).is_some_and(|n| n.key == key)) {
            return Some(path);
        }
        let b = self.bound(holder)?;
        let base = self.node_at(&b.path)?;
        let node = base.find(key)?;
        let path = [b.path.as_slice(), &path_to(base, node)?].concat();
        self.list_paths
            .borrow_mut()
            .insert(key.to_owned(), path.clone());
        Some(path)
    }

    // ----- lists -----------------------------------------------------------------------------

    /// The node with `key`: searched within the boundary whose key prefixes it, so a press in
    /// one row of a long list does not walk the whole tree.
    pub fn find(&self, key: &str) -> Option<&UiNode> {
        // A view call's own node is where its boundary is.
        if let Some(node) = self
            .by_key
            .get(key)
            .and_then(|id| self.bound(*id))
            .and_then(|b| self.node_at(&b.path))
            .filter(|n| n.key == key)
        {
            return Some(node);
        }
        let mut rest = key;
        while let Some((prefix, _)) = rest.rsplit_once('~') {
            if let Some(b) = self.by_key.get(prefix).and_then(|id| self.bound(*id))
                && let Some(node) = self.node_at(&b.path)
            {
                if let Some(found) = node.find(key) {
                    return Some(found);
                }
                break;
            }
            rest = prefix;
        }
        // Anything else (the root view's own nodes, an anchored node in any boundary) is found
        // by walking the tree once; where it was is kept, and checked before it is used again.
        if let Some(node) = self
            .found_paths
            .borrow()
            .get(key)
            .and_then(|path| self.node_at(path))
            .filter(|n| n.key == key)
        {
            return Some(node);
        }
        let node = self.tree.find(key)?;
        if let Some(path) = path_to(&self.tree, node) {
            self.found_paths.borrow_mut().insert(key.to_owned(), path);
        }
        Some(node)
    }

    /// The list with key `list`. A host asks for its rows one by one as they scroll into
    /// view, so the list's place is kept rather than searched for in the whole tree each time.
    fn list_node(&self, list: &str) -> Option<&UiNode> {
        let is_it = |n: &&UiNode| n.key == list && crate::view::is_virtual(&n.kind);
        let cached = self.list_paths.borrow().get(list).cloned();
        if let Some(node) = cached.and_then(|path| self.node_at(&path)).filter(is_it) {
            return Some(node);
        }
        let node = self
            .find(list)
            .filter(|n| crate::view::is_virtual(&n.kind))?;
        if let Some(path) = path_to(&self.tree, node) {
            self.list_paths.borrow_mut().insert(list.to_owned(), path);
        }
        Some(node)
    }

    pub fn list_len(&self, list: &str) -> Option<usize> {
        if let Some(rows) = self.lists.get(list) {
            return Some(rows.len);
        }
        self.list_node(list).map(|n| n.children.len())
    }

    /// The list's length, as reading a row of it left it ([`Virtual::settle_len`]).
    pub fn settle_len(&mut self, list: &str) {
        if let Some(rows) = self.lists.get_mut(list) {
            rows.settle_len();
        }
    }

    pub fn row_key(&self, list: &str, index: usize) -> Option<String> {
        if let Some(rows) = self.lists.get(list) {
            return rows.key(index);
        }
        self.list_node(list)?
            .children
            .get(index)
            .map(|row| row.key.clone())
    }

    /// Render row `index` of `list` if it is a view call that was not rendered yet.
    pub fn show_row(
        &mut self,
        core: &Rc<Core>,
        list: &str,
        index: usize,
    ) -> Result<Option<String>, RuntimeError> {
        if self.lists.contains_key(list) {
            return self.show_virtual(core, list, index);
        }
        let Some(key) = self.row_key(list, index) else {
            return Ok(None);
        };
        let Some(&id) = self.by_key.get(&key) else {
            // An ordinary node: it was rendered with the list.
            return Ok(Some(key));
        };
        if self.bound(id).is_some_and(|b| b.rendered) {
            return Ok(Some(key));
        }
        // The row's place: the list's and its index. Rows inserted or removed before it since
        // it was laid out moved it.
        let list_at = self.list_paths.borrow().get(list).cloned();
        if let (Some(mut path), Some(b)) = (list_at, self.bound_mut(id)) {
            path.push(index);
            b.path = path;
        }
        if let Err(e) = self.render_row(core, id, None) {
            self.release(core);
            return Err(e);
        }
        self.shown.entry(list.to_owned()).or_default().insert(id);
        self.release(core);
        Ok(Some(key))
    }

    /// Forget the rendered rows of `list` outside `keep`: they render again when shown.
    /// Returns the patches that empty them.
    pub fn hide_rows(
        &mut self,
        core: &Core,
        list: &str,
        keep: std::ops::Range<usize>,
    ) -> Vec<Patch> {
        if self.lists.contains_key(list) {
            return self.hide_virtual(core, list, &keep);
        }
        let Some(node) = self.list_node(list) else {
            return Vec::new();
        };
        let kept: HashSet<&str> = node
            .children
            .get(keep.start.min(node.children.len())..keep.end.min(node.children.len()))
            .unwrap_or_default()
            .iter()
            .map(|row| row.key.as_str())
            .collect();
        // Only the rows the host showed can be hidden: the work is in the rows on screen, not in
        // the length of the list.
        let rows: Vec<String> = self
            .shown
            .get(list)
            .into_iter()
            .flatten()
            .filter_map(|&id| self.bound(id))
            .filter(|b| !kept.contains(b.key.as_str()))
            .map(|b| b.key.clone())
            .collect();
        let mut patches = Vec::new();
        for key in rows {
            let Some(&id) = self.by_key.get(&key) else {
                continue;
            };
            if !self.bound(id).is_some_and(|b| b.rendered && b.lazy) {
                continue;
            }
            let path = self.bound(id).map(|b| b.path.clone()).unwrap_or_default();
            let keys = self.keys_at(&path);
            // Only a row whose own `fragment` the tree still holds has rows to hide: a row kept
            // behind by a list the view no longer draws has nowhere to write, and hiding it would
            // empty whatever stands at its stale path.
            if self.placed(id, &path)
                && let Some(fragment) = self.node_at_mut(&path)
            {
                for child in std::mem::take(&mut fragment.children) {
                    patches.push(Patch::Remove {
                        parent: keys.clone(),
                        key: child.key,
                        at: None,
                    });
                }
            }
            self.unrender(id);
            if let Some(rows) = self.shown.get_mut(list) {
                rows.remove(&id);
            }
        }
        self.release(core);
        patches
    }
}

/// Renders the view calls met while building one boundary's tree.
struct Builder<'a> {
    live: &'a mut Live,
    core: &'a Rc<Core>,
    /// The boundary being built.
    id: BoundId,
    /// The boundary's calls at its last render, by key.
    old: HashMap<String, BoundId>,
    /// Its calls now, in order.
    new: Vec<BoundId>,
    /// The loops over collections whose rows it holds.
    loops: Vec<Rc<LoopRows>>,
}

impl Boundaries for Builder<'_> {
    fn rows(&mut self, rows: LoopRows) {
        self.loops.push(Rc::new(rows));
    }

    fn virtualizes(&self) -> bool {
        self.live.lazy_rows
    }

    fn virtual_rows(&mut self, rows: VirtualRows) -> Result<Vec<UiNode>, RuntimeError> {
        self.build_virtual(rows)
    }

    fn boundary(&mut self, stub: Stub<'_>) -> Result<Vec<UiNode>, RuntimeError> {
        let lazy = stub.lazy && self.live.lazy_rows;
        if let Some(&id) = self.old.get(stub.key) {
            let same = self.live.bound(id).is_some_and(|b| {
                b.target.module == stub.target.module
                    && b.target.export == stub.target.export
                    && b.args == stub.args
            });
            if same {
                let (rendered, dirty, opened) =
                    self.live.bound(id).map_or((false, false, false), |b| {
                        (b.rendered, self.live.dirty.contains(&id), b.open)
                    });
                self.new.push(id);
                if opened != stub.open {
                    // The row it stands in opened or closed into its item: the call runs again,
                    // its tree holding the item's document or not.
                    if let Some(b) = self.live.bound_mut(id) {
                        b.open = stub.open;
                    }
                    if !rendered && lazy {
                        return Ok(Vec::new());
                    }
                    return Ok(vec![self.live.render(self.core, id)?]);
                }
                if rendered && !dirty {
                    // Unchanged: its subtree is kept as it is.
                    self.live.reused.insert(stub.key.to_owned());
                    return Ok(Vec::new());
                }
                if !rendered && lazy {
                    return Ok(Vec::new());
                }
                return Ok(vec![self.live.render(self.core, id)?]);
            }
            // Another view or other arguments: a call of its own.
            self.old.remove(stub.key);
            self.live.drop_bound(id);
        }
        let id = self.live.add_bound(Bound::new(stub, lazy));
        self.new.push(id);
        if lazy {
            return Ok(Vec::new());
        }
        Ok(vec![self.live.render(self.core, id)?])
    }
}

/// Find the `fragment`s of boundaries below `node` (at `at`) and where they are. The walk does
/// not stop at a boundary: the ones inside it move with it.
/// The child indexes from `root` down to `node` (found by address).
fn path_to(root: &UiNode, node: &UiNode) -> Option<Vec<usize>> {
    if std::ptr::eq(root, node) {
        return Some(Vec::new());
    }
    for (i, child) in root.children.iter().enumerate() {
        if let Some(mut path) = path_to(child, node) {
            path.insert(0, i);
            return Some(path);
        }
    }
    None
}

fn collect_paths(
    by_key: &HashMap<String, BoundId>,
    node: &UiNode,
    at: &mut Vec<usize>,
    out: &mut Vec<(BoundId, Vec<usize>)>,
    is_root: bool,
) {
    if !is_root
        && node.kind == "fragment"
        && let Some(id) = by_key.get(&node.key)
    {
        out.push((*id, at.clone()));
    }
    for (i, child) in node.children.iter().enumerate() {
        at.push(i);
        collect_paths(by_key, child, at, out, false);
        at.pop();
    }
}

/// Take the subtrees of the kept view calls out of `node`, by key.
fn take_reused(
    node: &mut UiNode,
    reused: &HashSet<String>,
    out: &mut HashMap<String, Vec<UiNode>>,
) {
    if reused.contains(&node.key) {
        out.insert(node.key.clone(), std::mem::take(&mut node.children));
        return;
    }
    for child in &mut node.children {
        take_reused(child, reused, out);
    }
}

/// Put the kept subtrees back into the new tree.
fn put_reused(node: &mut UiNode, kept: &mut HashMap<String, Vec<UiNode>>) {
    if let Some(children) = kept.remove(&node.key) {
        node.children = children;
        return;
    }
    if kept.is_empty() {
        return;
    }
    for child in &mut node.children {
        put_reused(child, kept);
    }
}
