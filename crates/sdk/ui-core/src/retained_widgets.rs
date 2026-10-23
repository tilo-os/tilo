//! Whether a retained layout's widget pointers still name live widgets.
//!
//! A layout map (each taffy node's `*mut dyn Widget`) is built from the
//! widget tree and kept across frames while nothing reports a structure
//! change ([`Widget::take_structure_dirty`]). A widget that replaces its
//! children without reporting it would leave the map pointing at freed
//! widgets, and the next frame would call through them: once the allocator
//! reused that memory the call read a garbage vtable (the web desktop trapped
//! in an indirect call). Safe code must not be able to cause that, so a
//! retained map is checked against the live tree before it is trusted again:
//! every widget it names must still be in the tree, met through
//! [`Widget::for_each_layout_child`], at the same address and of the same
//! concrete type. A map that fails is built again, so a widget that forgets
//! to report gets a fresh layout, never a call into freed memory.
//!
//! A widget is identified by its data address and concrete type
//! ([`Widget::concrete_type_name`]), not by trait-object pointer equality:
//! the same type's vtable can sit at different addresses in different
//! codegen units, and a wrapper shares its address with its first field. A
//! live widget of the registered type at the registered address is all the
//! registered pointer needs to be safe to call through.
//!
//! A widget that only forwards to a field (a typed facade over its core, an
//! app over its shell) lays the field out without the walk meeting it. Such
//! a field is checked through the walked widget whose memory holds it: while
//! that widget is the same type at the same place, so is its field. Only a
//! field the walk never meets inside an enum its owner switches without
//! reporting escapes that, which is why containers list what they lay out.
//!
//! A check compares the walk with the one recorded when the map was built,
//! widget by widget in walk order: the same sequence means every widget the
//! map can name is still there. That costs one walk and no lookups, which
//! keeps it small in the browser bundle. Debug builds also prove, at each
//! build, that every node of the map is covered by the recorded walk, the
//! invariant the check relies on, and name any widget that is not.

#[cfg(debug_assertions)]
use std::collections::{HashMap, HashSet};

use taffy::NodeId;

use crate::widget::Widget;

/// A widget's data address and concrete type.
type Identity = (usize, &'static str);

fn identity(widget: &dyn Widget) -> Identity {
    (
        std::ptr::from_ref(widget).cast::<()>() as usize,
        widget.concrete_type_name(),
    )
}

/// Hand `visitor` every child `widget` lays out: its
/// [`Widget::for_each_layout_child`] when it lists them, else its routed
/// children.
pub(crate) fn layout_children(widget: &dyn Widget, visitor: &mut dyn FnMut(&dyn Widget)) {
    if !widget.for_each_layout_child(visitor) {
        widget.for_each_child(visitor);
    }
}

/// The trees a host lays out, handed to a visitor one root at a time.
pub(crate) type Roots<'a> = &'a dyn Fn(&mut dyn FnMut(&dyn Widget));

/// Every widget of the trees `roots` hands over, parents first.
#[inline(never)]
fn walk(roots: Roots<'_>, visit: &mut dyn FnMut(&dyn Widget)) {
    fn descend(widget: &dyn Widget, visit: &mut dyn FnMut(&dyn Widget)) {
        visit(widget);
        layout_children(widget, &mut |child| descend(child, visit));
    }
    roots(&mut |root| descend(root, visit));
}

/// The walk a layout map was admitted against.
#[derive(Default)]
pub(crate) struct Adopted {
    /// Each widget the walk met, in walk order.
    walked: Vec<Identity>,
    /// Each node's widget as first admitted: its registered pointer,
    /// identity and size, kept so a later build's check never reads a
    /// pointer that may have gone stale.
    #[cfg(debug_assertions)]
    nodes: HashMap<NodeId, (*mut (dyn Widget + 'static), Identity, usize)>,
}

impl Adopted {
    /// Forget everything admitted (the map was cleared).
    pub(crate) fn clear(&mut self) {
        self.walked.clear();
        #[cfg(debug_assertions)]
        self.nodes.clear();
    }

    /// Record the walk of the trees `roots` hands over, right after a build
    /// registered widgets from them in `nodes`. A debug build stops on a
    /// node the walk does not cover, naming the widgets above it: its
    /// parent's [`Widget::for_each_layout_child`] leaves it out.
    ///
    /// # Safety
    /// Every pointer in `nodes` not admitted before must point at a live
    /// widget: a build registered it from the trees `roots` hands over, and
    /// no widget code ran since.
    #[inline(never)]
    pub(crate) unsafe fn admit(
        &mut self,
        nodes: &std::collections::HashMap<NodeId, *mut (dyn Widget + 'static)>,
        roots: Roots<'_>,
        taffy: &taffy::TaffyTree<()>,
    ) {
        self.walked.clear();
        let walked = &mut self.walked;
        walk(roots, &mut |widget| walked.push(identity(widget)));
        #[cfg(debug_assertions)]
        {
            // SAFETY: per the caller.
            unsafe { self.verify(nodes, roots, taffy) };
        }
        #[cfg(not(debug_assertions))]
        let _ = (nodes, taffy);
    }

    /// Whether the trees `roots` hands over are still the ones admitted,
    /// widget by widget. Only then may the map be called through.
    #[inline(never)]
    pub(crate) fn still_live(&self, roots: Roots<'_>) -> bool {
        let (mut at, mut same) = (0, true);
        walk(roots, &mut |widget| {
            if same {
                same = self.walked.get(at) == Some(&identity(widget));
                at += 1;
            }
        });
        same && at == self.walked.len()
    }

    /// Stop on a node of `nodes` the walk does not cover: neither its
    /// widget nor a walked widget whose memory holds it (a facade that lays
    /// out a field) is in the walk. Safety as for [`Self::admit`].
    #[cfg(debug_assertions)]
    unsafe fn verify(
        &mut self,
        nodes: &std::collections::HashMap<NodeId, *mut (dyn Widget + 'static)>,
        roots: Roots<'_>,
        taffy: &taffy::TaffyTree<()>,
    ) {
        self.nodes.retain(|node, (admitted, ..)| {
            nodes
                .get(node)
                .is_some_and(|&registered| std::ptr::eq(*admitted, registered))
        });
        for (&node, &registered) in nodes {
            self.nodes.entry(node).or_insert_with(|| {
                // SAFETY: not admitted before, so live per the caller.
                let widget = unsafe { &*registered };
                (registered, identity(widget), std::mem::size_of_val(widget))
            });
        }
        let met: HashSet<Identity> = self.walked.iter().copied().collect();
        let mut extents = Vec::new();
        // A widget listed twice (by two parents, or by a parent that also
        // routes to it another way) is walked twice: harmless to this check,
        // but a sign its owners disagree about who lays it out. Zero-sized
        // widgets share addresses legitimately, so only sized ones count.
        let mut seen = HashSet::new();
        let mut twice = Vec::new();
        walk(roots, &mut |widget| {
            let start = identity(widget).0;
            let size = std::mem::size_of_val(widget);
            extents.push((start, start + size));
            if size > 0 && !seen.insert(identity(widget)) {
                twice.push(widget.name().to_owned());
            }
        });
        debug_assert!(
            twice.is_empty(),
            "listed more than once by for_each_layout_child: {twice:?}"
        );
        let covered = |&(_, (start, _), size): &(*mut (dyn Widget + 'static), Identity, usize)| {
            extents
                .iter()
                .any(|&(from, to)| from <= start && start + size <= to)
        };
        let reached: HashSet<NodeId> = self
            .nodes
            .iter()
            .filter(|(_, admitted)| met.contains(&admitted.1) || covered(admitted))
            .map(|(&node, _)| node)
            .collect();
        let unreached: Vec<NodeId> = self
            .nodes
            .keys()
            .filter(|node| !reached.contains(node))
            .copied()
            .collect();
        if unreached.is_empty() {
            return;
        }
        // SAFETY: per the caller.
        let described = unsafe { describe(&unreached, &reached, nodes, taffy) };
        debug_assert!(
            described.is_empty(),
            "laid out but not visited by their parent's for_each_layout_child, \
             so their retained layout cannot be checked: {described:?}"
        );
    }
}

/// Each of `unreached` named with the widgets above it, up to the first the
/// walk reaches: "Text in Row in Section" says which parent's
/// [`Widget::for_each_layout_child`] leaves it out.
///
/// # Safety
/// Every pointer in `nodes` is live: registered by the build that just ran,
/// with no widget code run since.
#[cfg(debug_assertions)]
unsafe fn describe(
    unreached: &[NodeId],
    reached: &HashSet<NodeId>,
    nodes: &HashMap<NodeId, *mut (dyn Widget + 'static)>,
    taffy: &taffy::TaffyTree<()>,
) -> Vec<String> {
    let name = |node: NodeId| {
        nodes.get(&node).map_or_else(String::new, |&pointer| {
            // SAFETY: per the caller, every registered pointer is live.
            let widget = unsafe { &*pointer };
            match widget.test_id() {
                Some(id) => format!("{} \"{id}\"", widget.name()),
                None => widget.name().to_owned(),
            }
        })
    };
    unreached
        .iter()
        .map(|&node| {
            let mut chain = vec![name(node)];
            let mut at = node;
            let mut found = false;
            while let Some(parent) = taffy.parent(at) {
                at = parent;
                if nodes.contains_key(&parent) {
                    chain.push(name(parent));
                    if reached.contains(&parent) {
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                chain.push("a root no walk starts from".to_owned());
            }
            chain.join(" in ")
        })
        .collect()
}
