//! The live tree of a view: every view call a boundary of its own, re-run only when what it
//! read changed, and rows of a list rendered only when the host shows them.
//!
//! A [`Live`] holds the composed tree the host sees and, beside it, one [`Bound`] per view
//! call: which view with which arguments, what it read, and where its `fragment` sits in the
//! tree. A change names (resource, field) pairs; an index maps each to the boundaries that read
//! it, so a change to one note marks that note's row and nothing else. Re-running a boundary
//! diffs only its subtree, and a view call inside it whose view and arguments are unchanged
//! keeps its subtree without running again (memoized by key, view and arguments).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use crate::core::Core;
use crate::error::RuntimeError;
use crate::handles::Retained;
use crate::ids::Principal;
use crate::render::{Boundaries, Offer, Rendered, Scoped, Stub, ViewTarget};
use crate::space::Space;
use crate::store::{ANY_FIELD, ChangeSummary, doc_of};
use crate::value::Value;
use crate::view::{Patch, UiNode, diff_at};
use crate::view_state::StateKey;

pub(crate) type BoundId = usize;

/// One view call: the root view, or a call inside another boundary.
pub(crate) struct Bound {
    /// The key of the call's `fragment` in the tree; empty for the root.
    pub key: String,
    pub target: ViewTarget,
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
    pub space_reads: Vec<(String, f64)>,
    pub state_reads: Vec<(StateKey, u64)>,
    pub state_seen: Vec<(StateKey, Value)>,
    pub retained: Vec<Retained>,
}

/// Which boundaries read what.
#[derive(Default)]
struct Index {
    fields: HashMap<(String, String), HashSet<BoundId>>,
    resources: HashMap<String, HashSet<BoundId>>,
    docs: HashMap<String, HashSet<BoundId>>,
    states: HashMap<StateKey, HashSet<BoundId>>,
    space: HashSet<BoundId>,
}

impl Index {
    fn add(&mut self, id: BoundId, b: &Bound) {
        for (resource, field) in &b.reads {
            self.fields
                .entry((resource.clone(), field.clone()))
                .or_default()
                .insert(id);
            self.resources
                .entry(resource.clone())
                .or_default()
                .insert(id);
            self.docs
                .entry(doc_of(resource).to_owned())
                .or_default()
                .insert(id);
        }
        for (key, _) in &b.state_reads {
            self.states.entry(key.clone()).or_default().insert(id);
        }
        if !b.space_reads.is_empty() {
            self.space.insert(id);
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
        for (resource, field) in &b.reads {
            drop_from(&mut self.fields, &(resource.clone(), field.clone()), id);
            drop_from(&mut self.resources, resource, id);
            drop_from(&mut self.docs, &doc_of(resource).to_owned(), id);
        }
        for (key, _) in &b.state_reads {
            drop_from(&mut self.states, key, id);
        }
        self.space.remove(&id);
    }
}

/// What a view instance counts, for tests and the benchmark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveStats {
    /// Boundaries rendered (a view call run), since the view was opened.
    pub renders: u64,
    /// Boundaries in the tree now.
    pub boundaries: usize,
    /// Of those, rows of lists not rendered because they were not shown.
    pub unrendered: usize,
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
    /// View calls kept without running during the current re-render, by key.
    reused: HashSet<String>,
    /// Action references of trees replaced during the current re-render: released once the
    /// diff no longer compares against them.
    to_release: Vec<Retained>,
    /// The view-state clock last checked.
    state_clock: u64,
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
            target,
            args,
            depth: 0,
            children: Vec::new(),
            path: Vec::new(),
            rendered: false,
            lazy: false,
            reads: Vec::new(),
            space_reads: Vec::new(),
            state_reads: Vec::new(),
            state_seen: Vec::new(),
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
            reused: HashSet::new(),
            to_release: Vec::new(),
            state_clock: core.view_states.borrow().clock(),
        };
        let tree = live.render(core, ROOT)?;
        live.tree = tree;
        live.reused.clear();
        live.refresh_paths(ROOT);
        live.release(core);
        Ok(live)
    }

    pub fn bound(&self, id: BoundId) -> Option<&Bound> {
        self.bounds.get(id).and_then(Option::as_ref)
    }

    fn bound_mut(&mut self, id: BoundId) -> Option<&mut Bound> {
        self.bounds.get_mut(id).and_then(Option::as_mut)
    }

    /// The documents some boundary read.
    pub fn docs(&self) -> impl Iterator<Item = &String> {
        self.index.docs.keys()
    }

    pub fn stats(&self) -> LiveStats {
        let live = self.bounds.iter().flatten();
        let (mut boundaries, mut unrendered) = (0, 0);
        for b in live {
            boundaries += 1;
            if !b.rendered {
                unrendered += 1;
            }
        }
        LiveStats {
            renders: self.renders,
            boundaries,
            unrendered,
        }
    }

    // ----- invalidation ----------------------------------------------------------------------

    /// Whether a change reaches some boundary.
    pub fn reaches(&self, change: &ChangeSummary) -> bool {
        if change.fields.is_empty() {
            return self.index.docs.contains_key(&change.doc);
        }
        change.fields.iter().any(|key| {
            if key.field == ANY_FIELD {
                self.index.resources.contains_key(&key.resource)
            } else {
                self.index
                    .fields
                    .contains_key(&(key.resource.clone(), key.field.clone()))
            }
        })
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
    pub fn absorb(&mut self, change: &ChangeSummary) {
        let mut hit: Vec<BoundId> = Vec::new();
        if change.fields.is_empty() {
            if let Some(set) = self.index.docs.get(&change.doc) {
                hit.extend(set.iter().copied());
            }
        } else {
            for key in &change.fields {
                let set = if key.field == ANY_FIELD {
                    self.index.resources.get(&key.resource)
                } else {
                    self.index
                        .fields
                        .get(&(key.resource.clone(), key.field.clone()))
                };
                if let Some(set) = set {
                    hit.extend(set.iter().copied());
                }
            }
        }
        self.dirty.extend(hit);
    }

    /// Mark the boundaries whose view state moved since they read it.
    pub fn check_states(&mut self, core: &Core) {
        let states = core.view_states.borrow();
        let clock = states.clock();
        if clock == self.state_clock {
            return;
        }
        self.state_clock = clock;
        let mut hit = Vec::new();
        for (key, ids) in &self.index.states {
            let now = states.generation(key);
            for id in ids {
                let moved = self.bound(*id).is_some_and(|b| {
                    b.state_reads
                        .iter()
                        .any(|(k, seen)| k == key && *seen != now)
                });
                if moved {
                    hit.push(*id);
                }
            }
        }
        drop(states);
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

    /// Mark every boundary: what views show without reading it changed.
    pub fn mark_all(&mut self) {
        let all: Vec<BoundId> = self
            .bounds
            .iter()
            .enumerate()
            .filter_map(|(id, b)| b.as_ref().filter(|b| b.rendered).map(|_| id))
            .collect();
        self.dirty.extend(all);
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
        !self.dirty.is_empty()
    }

    // ----- rendering -------------------------------------------------------------------------

    /// Run boundary `id` and build its tree, re-running only the view calls inside it that
    /// changed. Returns the tree; the boundary's record is updated.
    fn render(&mut self, core: &Rc<Core>, id: BoundId) -> Result<UiNode, RuntimeError> {
        let (target, args, depth, key, old_children) = {
            let b = self
                .bound(id)
                .ok_or_else(|| RuntimeError::Package("a vanished boundary".into()))?;
            (
                b.target.clone(),
                b.args.clone(),
                b.depth,
                b.key.clone(),
                b.children.clone(),
            )
        };
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
            old,
            new: Vec::new(),
        };
        let prefix = (id != ROOT).then_some(key.as_str());
        let rendered = core.render_boundary(
            &target,
            &args,
            offer,
            None,
            Some(Scoped {
                prefix,
                boundaries: Some(&mut builder),
            }),
        );
        let Builder { old, new, .. } = builder;
        // Calls that are gone take their subtrees with them.
        let kept: HashSet<BoundId> = new.iter().copied().collect();
        for gone in old.values().filter(|c| !kept.contains(c)) {
            self.drop_bound(*gone);
        }
        let rendered = rendered?;
        self.renders += 1;
        let Rendered {
            root,
            reads,
            space_reads,
            retained,
            state_reads,
            state_seen,
        } = rendered;
        let mut reads = reads;
        reads.sort();
        reads.dedup();
        if let Some(mut b) = self.bounds.get_mut(id).and_then(Option::take) {
            self.index.remove(id, &b);
            self.to_release.append(&mut b.retained);
            b.children = new;
            b.reads = reads;
            b.space_reads = space_reads;
            b.state_reads = state_reads;
            b.state_seen = state_seen;
            b.retained = retained;
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
        let id = match self.free.pop() {
            Some(id) => {
                if let Some(slot) = self.bounds.get_mut(id) {
                    *slot = Some(b);
                }
                id
            }
            None => {
                self.bounds.push(Some(b));
                self.bounds.len() - 1
            }
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
        self.to_release.append(&mut b.retained);
        if self.by_key.get(&b.key) == Some(&id) {
            self.by_key.remove(&b.key);
        }
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
        if let Some(mut b) = self.bounds.get_mut(id).and_then(Option::take) {
            self.index.remove(id, &b);
            self.to_release.append(&mut b.retained);
            b.children.clear();
            b.reads.clear();
            b.space_reads.clear();
            b.state_reads.clear();
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
        for (child, path) in found {
            if let Some(b) = self.bound_mut(child) {
                b.path = path;
            }
        }
    }

    /// Re-run every boundary that is stale, outermost first, and return the patches that
    /// bring the tree up to date.
    pub fn poll(&mut self, core: &Rc<Core>) -> Result<Vec<Patch>, RuntimeError> {
        let mut patches = Vec::new();
        while let Some(id) = self.next_dirty() {
            self.rerender(core, id, &mut patches)?;
        }
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
        self.reused.clear();
        let new_content = match self.render(core, id) {
            Ok(tree) => tree,
            Err(e) => {
                self.dirty.remove(&id);
                self.release(core);
                return Err(e);
            }
        };
        let reused = std::mem::take(&mut self.reused);
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
            patches.extend(crate::view::diff(&old, &new_content, &same));
        } else {
            diff_at(&parent_keys, &old, &new_content, &same, patches);
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

    // ----- lists -----------------------------------------------------------------------------

    /// The node with `key`: searched within the boundary whose key prefixes it, so a press in
    /// one row of a long list does not walk the whole tree.
    pub fn find(&self, key: &str) -> Option<&UiNode> {
        let mut at = key.len();
        while let Some(cut) = key[..at].rfind('~') {
            if let Some(b) = self.by_key.get(&key[..cut]).and_then(|id| self.bound(*id))
                && let Some(node) = self.node_at(&b.path)
            {
                if let Some(found) = node.find(key) {
                    return Some(found);
                }
                break;
            }
            at = cut;
        }
        self.tree.find(key)
    }

    fn list_node(&self, list: &str) -> Option<&UiNode> {
        self.find(list).filter(|n| n.kind == "list")
    }

    pub fn list_len(&self, list: &str) -> Option<usize> {
        self.list_node(list).map(|n| n.children.len())
    }

    pub fn row_key(&self, list: &str, index: usize) -> Option<String> {
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
        self.reused.clear();
        let content = self.render(core, id);
        let content = match content {
            Ok(c) => c,
            Err(e) => {
                self.release(core);
                return Err(e);
            }
        };
        self.reused.clear();
        let path = self.bound(id).map(|b| b.path.clone()).unwrap_or_default();
        if let Some(fragment) = self.node_at_mut(&path) {
            fragment.children = vec![content];
        }
        self.refresh_paths(id);
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
        let Some(node) = self.list_node(list) else {
            return Vec::new();
        };
        let rows: Vec<(usize, String)> = node
            .children
            .iter()
            .enumerate()
            .filter(|(i, _)| !keep.contains(i))
            .map(|(i, row)| (i, row.key.clone()))
            .collect();
        let mut patches = Vec::new();
        for (_, key) in rows {
            let Some(&id) = self.by_key.get(&key) else {
                continue;
            };
            if !self.bound(id).is_some_and(|b| b.rendered && b.lazy) {
                continue;
            }
            let path = self.bound(id).map(|b| b.path.clone()).unwrap_or_default();
            let keys = self.keys_at(&path);
            if let Some(fragment) = self.node_at_mut(&path) {
                for child in std::mem::take(&mut fragment.children) {
                    patches.push(Patch::Remove {
                        parent: keys.clone(),
                        key: child.key,
                    });
                }
            }
            self.unrender(id);
        }
        self.release(core);
        patches
    }
}

/// Renders the view calls met while building one boundary's tree.
struct Builder<'a> {
    live: &'a mut Live,
    core: &'a Rc<Core>,
    /// The boundary's calls at its last render, by key.
    old: HashMap<String, BoundId>,
    /// Its calls now, in order.
    new: Vec<BoundId>,
}

impl Boundaries for Builder<'_> {
    fn boundary(&mut self, stub: Stub<'_>) -> Result<Vec<UiNode>, RuntimeError> {
        let lazy = stub.lazy && self.live.lazy_rows;
        if let Some(&id) = self.old.get(stub.key) {
            let same = self.live.bound(id).is_some_and(|b| {
                b.target.module == stub.target.module
                    && b.target.export == stub.target.export
                    && b.args == stub.args
            });
            if same {
                let (rendered, dirty) = self.live.bound(id).map_or((false, false), |b| {
                    (b.rendered, self.live.dirty.contains(&id))
                });
                self.new.push(id);
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
        let id = self.live.add_bound(Bound {
            key: stub.key.to_owned(),
            target: stub.target,
            args: stub.args,
            depth: stub.depth,
            children: Vec::new(),
            path: Vec::new(),
            rendered: false,
            lazy,
            reads: Vec::new(),
            space_reads: Vec::new(),
            state_reads: Vec::new(),
            state_seen: Vec::new(),
            retained: Vec::new(),
        });
        self.new.push(id);
        if lazy {
            return Ok(Vec::new());
        }
        Ok(vec![self.live.render(self.core, id)?])
    }
}

/// Find the `fragment`s of boundaries below `node` (at `at`) and where they are. The walk does
/// not stop at a boundary: the ones inside it move with it.
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
