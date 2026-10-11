//! A published resource tree whose unchanged subtrees are shared between
//! versions.
//!
//! [`tilo_actions::Resource`] owns its children inline, so two versions of a
//! tree cannot share an item and nothing reading them can tell an unchanged
//! item from a changed one without comparing it. A keystroke in a field no
//! item depends on then rebuilt, copied, filtered and re-mirrored every item
//! of a long list (TIL-766). A [`SharedResource`] holds the same tree with
//! its children behind [`Rc`]s: a publisher hands over the very nodes of
//! the last version where nothing changed, and every consumer — permission
//! filtering, control availability, the accessibility tree — skips a subtree
//! it has seen by pointer. [`SharedResource::to_resource`] gives the
//! ordinary tree back, exactly as published, for consumers of the whole
//! surface (the palette, agents, search).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tilo_actions::Resource;

/// What filtering one node or one list of children for one permission
/// context (`token`) gave, kept on the input so the same input is filtered
/// once. `Same` keeps no pointer to the input itself, so no node holds
/// itself alive.
pub(crate) enum Filtered<T> {
    /// Every action is permitted; the input stands as it is.
    Same,
    /// Nothing in it is permitted: dropped from the tree.
    Pruned,
    /// A filtered copy that shares what filtering left unchanged.
    Kept(T),
}

impl<T: Clone> Clone for Filtered<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Same => Self::Same,
            Self::Pruned => Self::Pruned,
            Self::Kept(kept) => Self::Kept(kept.clone()),
        }
    }
}

/// A published resource: its own part (identity, actions, views) and its
/// children, shared with every version that did not change them.
#[derive(Clone)]
pub struct SharedResource(Rc<Node>);

struct Node {
    /// The resource without its children.
    head: Resource,
    children: SharedChildren,
    filtered: RefCell<Option<(u64, Filtered<SharedResource>)>>,
}

/// The children of a [`SharedResource`], shared as a whole while none of
/// them changed.
#[derive(Clone)]
pub struct SharedChildren(Rc<Children>);

struct Children {
    items: Vec<SharedResource>,
    filtered: RefCell<Option<(u64, Filtered<SharedChildren>)>>,
}

impl Default for SharedChildren {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl SharedChildren {
    /// Children made of `items`.
    pub fn new(items: Vec<SharedResource>) -> Self {
        Self(Rc::new(Children {
            items,
            filtered: RefCell::new(None),
        }))
    }

    /// The children in order.
    pub fn as_slice(&self) -> &[SharedResource] {
        &self.0.items
    }

    /// The children in order.
    pub fn iter(&self) -> std::slice::Iter<'_, SharedResource> {
        self.0.items.iter()
    }

    /// How many children there are.
    pub fn len(&self) -> usize {
        self.0.items.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.0.items.is_empty()
    }

    /// Whether `self` and `other` are the very same children.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    /// What filtering these children for `token` gave last, if it was for
    /// `token`.
    pub(crate) fn filtered(&self, token: u64) -> Option<Filtered<SharedChildren>> {
        self.0
            .filtered
            .borrow()
            .as_ref()
            .filter(|(at, _)| *at == token)
            .map(|(_, filtered)| filtered.clone())
    }

    pub(crate) fn remember_filtered(&self, token: u64, filtered: Filtered<SharedChildren>) {
        *self.0.filtered.borrow_mut() = Some((token, filtered));
    }
}

impl<'a> IntoIterator for &'a SharedChildren {
    type Item = &'a SharedResource;
    type IntoIter = std::slice::Iter<'a, SharedResource>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl SharedResource {
    /// `head`, whose own children are ignored, over `children`.
    pub fn new(mut head: Resource, children: SharedChildren) -> Self {
        debug_assert!(
            head.children.is_empty(),
            "a shared resource's children are its SharedChildren"
        );
        head.children = Vec::new();
        Self(Rc::new(Node {
            head,
            children,
            filtered: RefCell::new(None),
        }))
    }

    /// `resource` as a shared tree, every node its own.
    pub fn from_resource(mut resource: Resource) -> Self {
        let children = std::mem::take(&mut resource.children)
            .into_iter()
            .map(Self::from_resource)
            .collect();
        Self::new(resource, SharedChildren::new(children))
    }

    /// The ordinary tree, exactly as published.
    pub fn to_resource(&self) -> Resource {
        let mut resource = self.0.head.clone();
        resource.children = self.0.children.iter().map(Self::to_resource).collect();
        resource
    }

    /// The resource without its children: its id, kind, label,
    /// presentation, actions and views.
    pub fn head(&self) -> &Resource {
        &self.0.head
    }

    /// The resource's id.
    pub fn id(&self) -> &str {
        self.0.head.id.as_str()
    }

    /// Its children.
    pub fn children(&self) -> &SharedChildren {
        &self.0.children
    }

    /// Whether `self` and `other` are the very same node.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    /// The node's address, for sets of nodes seen.
    pub(crate) fn addr(&self) -> *const () {
        Rc::as_ptr(&self.0).cast()
    }

    /// What filtering this node for `token` gave last, if it was for
    /// `token`.
    pub(crate) fn filtered(&self, token: u64) -> Option<Filtered<SharedResource>> {
        self.0
            .filtered
            .borrow()
            .as_ref()
            .filter(|(at, _)| *at == token)
            .map(|(_, filtered)| filtered.clone())
    }

    pub(crate) fn remember_filtered(&self, token: u64, filtered: Filtered<SharedResource>) {
        *self.0.filtered.borrow_mut() = Some((token, filtered));
    }
}

/// `forest` as ordinary trees.
pub fn to_resources(forest: &[SharedResource]) -> Vec<Resource> {
    forest.iter().map(SharedResource::to_resource).collect()
}

/// One step of [`diff`]: how the next forest differs from the last.
pub(crate) enum Change<'a> {
    /// A subtree only the last forest has.
    Removed(&'a SharedResource),
    /// A subtree only the next forest has.
    Added(&'a SharedResource),
    /// A node both have under the same id whose own part may differ; its
    /// children are diffed on their own, before this is reported.
    Head {
        last: &'a SharedResource,
        next: &'a SharedResource,
    },
    /// The list of roots differs: a root was added, removed or moved. (A
    /// node's own list of children changing is reported as its
    /// [`Change::Head`].)
    Roots,
}

/// Walk what differs from `last` to `next`, skipping every subtree both
/// share by pointer. Nodes are paired by id: a node whose own part changed
/// is reported as [`Change::Head`] (with its children diffed first), one
/// without a partner as removed or added.
pub(crate) fn diff<'a>(
    last: &'a [SharedResource],
    next: &'a [SharedResource],
    visit: &mut dyn FnMut(Change<'a>),
) {
    diff_lists(true, last, next, visit);
}

fn diff_lists<'a>(
    roots: bool,
    last: &'a [SharedResource],
    next: &'a [SharedResource],
    visit: &mut dyn FnMut(Change<'a>),
) {
    if std::ptr::eq(last, next) {
        return;
    }
    // Lists of one length are paired by position first: an edit to one item
    // of a long list is found without hashing the rest.
    let mut unpaired_last: Vec<&'a SharedResource> = Vec::new();
    let mut unpaired_next: Vec<&'a SharedResource> = Vec::new();
    let mut changed = false;
    if last.len() == next.len() {
        for (before, after) in last.iter().zip(next) {
            if before.ptr_eq(after) {
                continue;
            }
            changed = true;
            if before.id() == after.id() {
                diff_node(before, after, visit);
            } else {
                unpaired_last.push(before);
                unpaired_next.push(after);
            }
        }
    } else {
        changed = true;
        let kept: std::collections::HashSet<*const ()> =
            next.iter().map(SharedResource::addr).collect();
        let had: std::collections::HashSet<*const ()> =
            last.iter().map(SharedResource::addr).collect();
        unpaired_last.extend(last.iter().filter(|node| !kept.contains(&node.addr())));
        unpaired_next.extend(next.iter().filter(|node| !had.contains(&node.addr())));
    }
    if !changed {
        return;
    }
    let moved = last.len() != next.len() || !unpaired_last.is_empty() || !unpaired_next.is_empty();
    let mut by_id: std::collections::HashMap<&str, std::collections::VecDeque<&'a SharedResource>> =
        std::collections::HashMap::new();
    for node in unpaired_last {
        by_id.entry(node.id()).or_default().push_back(node);
    }
    for node in unpaired_next {
        match by_id
            .get_mut(node.id())
            .and_then(std::collections::VecDeque::pop_front)
        {
            Some(before) => diff_node(before, node, visit),
            None => visit(Change::Added(node)),
        }
    }
    let gone = by_id
        .into_values()
        .flatten()
        .map(SharedResource::addr)
        .collect::<std::collections::HashSet<_>>();
    // Report removals in the last forest's order.
    if !gone.is_empty() {
        for node in last.iter().filter(|node| gone.contains(&node.addr())) {
            visit(Change::Removed(node));
        }
    }
    if roots && moved {
        visit(Change::Roots);
    }
}

fn diff_node<'a>(
    last: &'a SharedResource,
    next: &'a SharedResource,
    visit: &mut dyn FnMut(Change<'a>),
) {
    if last.ptr_eq(next) {
        return;
    }
    if !last.children().ptr_eq(next.children()) {
        diff_lists(
            false,
            last.children().as_slice(),
            next.children().as_slice(),
            visit,
        );
    }
    visit(Change::Head { last, next });
}

thread_local! {
    static NEXT_TOKEN: Cell<u64> = const { Cell::new(1) };
}

/// A permission context's token no other context had: the key a filtered
/// result is kept under on its input (see [`SharedResource::filtered`]).
pub(crate) fn fresh_token() -> u64 {
    NEXT_TOKEN.with(|next| {
        let token = next.get();
        next.set(token.wrapping_add(1).max(1));
        token
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(id: &str) -> SharedResource {
        SharedResource::new(Resource::new(id, "kind", id), SharedChildren::default())
    }

    fn changes(last: &[SharedResource], next: &[SharedResource]) -> Vec<String> {
        let mut out = Vec::new();
        diff(last, next, &mut |change| {
            out.push(match change {
                Change::Removed(node) => format!("-{}", node.id()),
                Change::Added(node) => format!("+{}", node.id()),
                Change::Head { next, .. } => format!("~{}", next.id()),
                Change::Roots => "roots".to_owned(),
            });
        });
        out
    }

    #[test]
    fn a_shared_subtree_is_skipped_and_an_edit_found_by_position() {
        let items = (0..5).map(|i| leaf(&format!("i{i}"))).collect::<Vec<_>>();
        let list = SharedChildren::new(items.clone());
        let root = SharedResource::new(Resource::new("root", "k", "Root"), list.clone());
        let relabelled = SharedResource::new(Resource::new("root", "k", "Root 2"), list);
        assert_eq!(
            changes(
                std::slice::from_ref(&root),
                std::slice::from_ref(&relabelled)
            ),
            vec!["~root"],
            "a new head over the same children diffs no child"
        );
        let mut edited = items.clone();
        edited[3] = leaf("i3");
        let after = SharedResource::new(
            Resource::new("root", "k", "Root"),
            SharedChildren::new(edited),
        );
        assert_eq!(
            changes(std::slice::from_ref(&root), std::slice::from_ref(&after)),
            vec!["~i3", "~root"]
        );
        let mut grown = items;
        grown.remove(1);
        grown.push(leaf("i9"));
        grown.push(leaf("i8"));
        let after = SharedResource::new(
            Resource::new("root", "k", "Root"),
            SharedChildren::new(grown),
        );
        assert_eq!(
            changes(std::slice::from_ref(&root), std::slice::from_ref(&after)),
            vec!["+i9", "+i8", "-i1", "~root"]
        );
    }

    #[test]
    fn the_ordinary_tree_round_trips() {
        let mut resource = Resource::new("root", "k", "Root");
        let mut child = Resource::new("child", "k", "Child");
        child
            .children
            .push(Resource::new("grandchild", "k", "Grandchild"));
        resource.children.push(child);
        let shared = SharedResource::from_resource(resource);
        let back = shared.to_resource();
        assert_eq!(back.children.len(), 1);
        assert_eq!(back.children[0].children[0].id.as_str(), "grandchild");
        assert!(shared.head().children.is_empty());
    }
}
