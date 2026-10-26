//! Changing a widget's own children in the retained layout tree, in place.
//!
//! A structure change normally makes the host discard its whole Taffy tree,
//! build every widget's layout again and solve it from scratch: every text is
//! measured again, though one row came into a list. A widget that only mounts
//! and unmounts children of its own, whose other widgets all stay alive,
//! reports [`StructureChange::Children`](super::StructureChange::Children)
//! instead and carries it out in [`Widget::remount_children`]
//! through a [`Remount`]: the children it mounts are built into the retained
//! tree, the ones it lets go are removed from it, and every other node keeps
//! its identity and its cached layout. The host admits what was mounted and
//! solves the tree again, which lays out only the nodes the change dirtied.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};

use super::Widget;
use crate::theme::Theme;

/// The retained layout tree a [`Widget::remount_children`] changes, and what
/// the change mounted and unmounted, for the host to admit.
pub struct Remount<'a> {
    taffy: &'a mut TaffyTree<()>,
    theme: &'a Theme,
    node_map: &'a mut HashMap<NodeId, *mut dyn Widget>,
    mounted: Vec<(NodeId, NodeId)>,
    unmounted: Vec<NodeId>,
}

impl<'a> Remount<'a> {
    /// A remount of the tree `taffy`, whose widgets `node_map` maps.
    pub(crate) fn new(
        taffy: &'a mut TaffyTree<()>,
        theme: &'a Theme,
        node_map: &'a mut HashMap<NodeId, *mut dyn Widget>,
    ) -> Self {
        Self {
            taffy,
            theme,
            node_map,
            mounted: Vec::new(),
            unmounted: Vec::new(),
        }
    }

    /// The tree, to read a node's layout or set a style.
    pub fn taffy(&mut self) -> &mut TaffyTree<()> {
        self.taffy
    }

    /// Build `child`'s layout into the tree, for the widget remounting,
    /// whose node is `owner`, to place under its own nodes through
    /// [`Self::set_children`]. Returns its node.
    pub fn mount(&mut self, owner: NodeId, child: &mut dyn Widget) -> NodeId {
        let node = child.build_layout(self.taffy, self.theme, self.node_map);
        self.mounted.push((owner, node));
        node
    }

    /// [`Self::mount`] for a widget that builds what it places under
    /// `owner` itself (`build`, with the tree, the theme and the node map, as
    /// [`Widget::build_layout`] has them): the node it returns is mounted;
    /// `None` mounts nothing.
    pub fn mount_with(
        &mut self,
        owner: NodeId,
        build: impl FnOnce(
            &mut TaffyTree<()>,
            &Theme,
            &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> Option<NodeId>,
    ) -> Option<NodeId> {
        let node = build(self.taffy, self.theme, self.node_map)?;
        self.mounted.push((owner, node));
        Some(node)
    }

    /// Remove `node`, a child the widget remounting lets go of, and
    /// everything under it from the tree and the node map. Its widgets stay
    /// alive: the widget keeps them to mount again.
    pub fn unmount(&mut self, node: NodeId) {
        let mut pending = vec![node];
        while let Some(node) = pending.pop() {
            pending.extend(self.taffy.children(node).unwrap_or_default());
            self.node_map.remove(&node);
            let _ = self.taffy.remove(node);
            self.unmounted.push(node);
        }
    }

    /// Make `children` the children of `parent`, in order.
    ///
    /// # Errors
    /// Taffy's, when a node is not in the tree.
    pub fn set_children(&mut self, parent: NodeId, children: &[NodeId]) -> taffy::TaffyResult<()> {
        self.taffy.set_children(parent, children)
    }

    /// What was mounted, as (owner, child) pairs, and every node removed.
    pub(crate) fn finish(self) -> (Vec<(NodeId, NodeId)>, Vec<NodeId>) {
        (self.mounted, self.unmounted)
    }
}
