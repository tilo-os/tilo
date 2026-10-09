//! App-local widgets: the host that mounts Tube's one persistent video player
//! into each rebuilt watch page, and the Taffy helpers it uses.

pub mod player_host;

pub use player_host::{PlayerHost, SharedPlayer};

use vello::kurbo::{Point, Rect};

// --- Taffy layout helpers ------------------------------------------------
//
// `TaffyTree::new_leaf`/`new_with_children`/`layout` are typed fallible for
// API generality, but every leaf widget in this module calls them the exact
// way `ui` itself does internally (see `ui::widget::TaffyTreeExt`'s doc
// comment, which this mirrors): `new_leaf` always inserts and returns `Ok`;
// `layout` is only ever queried here for a node this same widget just built
// and had computed by this frame's `compute_layout` pass. Centralised here
// (rather than one `#[allow]` per call site) so every widget's
// `build_layout`/`apply_layout` reads the same as the rest of the toolkit.

/// Insert a leaf taffy node. See the module doc comment above for why the
/// internal `unwrap` cannot actually panic.
pub(crate) fn leaf_node(taffy: &mut taffy::TaffyTree<()>, style: taffy::Style) -> taffy::NodeId {
    #[allow(
        clippy::unwrap_used,
        reason = "see widgets Taffy layout helpers doc comment"
    )]
    let node = taffy.new_leaf(style).unwrap();
    node
}

/// This node's just-computed layout. See the module doc comment above for
/// why the internal `unwrap` cannot actually panic. Prefer [`layout_rect`]
/// when a plain `origin + layout` rect is all that's needed.
pub(crate) fn node_layout(taffy: &taffy::TaffyTree<()>, node: taffy::NodeId) -> &taffy::Layout {
    #[allow(
        clippy::unwrap_used,
        reason = "see widgets Taffy layout helpers doc comment"
    )]
    let l = taffy.layout(node).unwrap();
    l
}

/// This widget's on-screen rect: `origin` plus the just-computed layout for
/// `node`. See the module doc comment above for why the internal `unwrap`
/// cannot actually panic.
pub(crate) fn layout_rect(
    taffy: &taffy::TaffyTree<()>,
    node: taffy::NodeId,
    origin: Point,
) -> Rect {
    #[allow(
        clippy::unwrap_used,
        reason = "see widgets Taffy layout helpers doc comment"
    )]
    let l = taffy.layout(node).unwrap();
    ui::widget::placed(l, origin)
}
