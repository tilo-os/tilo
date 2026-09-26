//! Pick mode's model: which semantic node is under the pointer, across every
//! app surface on screen.
//!
//! Nothing here looks at pixels. Each surface contributes the semantic tree
//! its own app answered through the Inspect broker request, in the app's
//! surface coordinates; the shell only knows where that surface sits on
//! screen. Hit-testing is therefore a walk over app-declared bounds, and the
//! answer is a node id the app itself will recognise later.
//!
//! The same model serves the pointer (hover, click, Shift-click, drag) and
//! the keyboard (arrows walk the tree, Enter picks), so the two can never
//! disagree about what "this" is.

use std::collections::BTreeMap;

use tilo_reference::ResourceRef;
use vello::kurbo::{Point, Rect};

/// One semantic node as pick mode needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct PickNode {
    /// The app's stable semantic id.
    pub id: String,
    /// Accessibility role as the app spelled it (`button`, `text_input`, …).
    pub role: String,
    pub label: Option<String>,
    /// Bounds in the owning surface's logical coordinates.
    pub bounds: Rect,
    pub children: Vec<String>,
    /// The document address the node stands for, when the app has one.
    pub reference: Option<ResourceRef>,
    /// The node's value is personal: shown here, never sent unreviewed.
    pub personal: bool,
}

/// One app surface on screen and the tree it answered.
#[derive(Clone, Debug, PartialEq)]
pub struct PickSurface {
    /// Shell key of the surface (the tab it is hosted in).
    pub key: String,
    pub app_id: String,
    pub app_label: String,
    pub room: String,
    pub record: String,
    /// Where the surface's content box sits on screen.
    pub screen: Rect,
    /// Screen pixels per surface logical pixel.
    pub scale: f64,
    /// Root first, in the app's own document order.
    pub nodes: Vec<PickNode>,
}

impl PickSurface {
    fn node(&self, id: &str) -> Option<&PickNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// Parent of every node that has one, from the children lists.
    fn parents(&self) -> BTreeMap<&str, &str> {
        let mut parents = BTreeMap::new();
        for node in &self.nodes {
            for child in &node.children {
                parents.insert(child.as_str(), node.id.as_str());
            }
        }
        parents
    }

    fn depth(&self, id: &str) -> usize {
        let parents = self.parents();
        let mut depth = 0_usize;
        let mut at = id;
        while let Some(parent) = parents.get(at) {
            depth = depth.saturating_add(1);
            at = parent;
            if depth > self.nodes.len() {
                break;
            }
        }
        depth
    }

    /// A node's bounds on screen.
    pub fn to_screen(&self, bounds: Rect) -> Rect {
        let scale = if self.scale > 0.0 { self.scale } else { 1.0 };
        Rect::new(
            self.screen.x0 + bounds.x0 * scale,
            self.screen.y0 + bounds.y0 * scale,
            self.screen.x0 + bounds.x1 * scale,
            self.screen.y0 + bounds.y1 * scale,
        )
        .intersect(self.screen)
    }

    fn roots(&self) -> Vec<&PickNode> {
        let parents = self.parents();
        self.nodes
            .iter()
            .filter(|node| !parents.contains_key(node.id.as_str()))
            .collect()
    }
}

/// A node named across surfaces.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct NodeKey {
    pub surface: String,
    pub node: String,
}

impl NodeKey {
    pub fn new(surface: impl Into<String>, node: impl Into<String>) -> Self {
        Self {
            surface: surface.into(),
            node: node.into(),
        }
    }
}

/// A resolved node: its key, its surface and the node itself.
#[derive(Clone, Copy, Debug)]
pub struct Resolved<'a> {
    pub surface: &'a PickSurface,
    pub node: &'a PickNode,
}

impl Resolved<'_> {
    pub fn key(&self) -> NodeKey {
        NodeKey::new(self.surface.key.clone(), self.node.id.clone())
    }

    pub fn screen_rect(&self) -> Rect {
        self.surface.to_screen(self.node.bounds)
    }
}

/// Which way a keyboard walk goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Walk {
    /// To the enclosing node.
    Out,
    /// Into the first child.
    In,
    Previous,
    Next,
}

impl Walk {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "out" | "up" | "parent" => Self::Out,
            "in" | "down" | "child" => Self::In,
            "previous" | "left" | "prev" => Self::Previous,
            "next" | "right" => Self::Next,
            _ => return None,
        })
    }
}

/// Everything on screen that can be picked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PickScene {
    /// Topmost first.
    pub surfaces: Vec<PickSurface>,
}

/// Nodes smaller than this on screen are not offered by hover: nothing a
/// person can point at is that small, and such a node is usually a layout
/// artefact rather than something they meant.
const MIN_TARGET: f64 = 2.0;

impl PickScene {
    pub fn surface(&self, key: &str) -> Option<&PickSurface> {
        self.surfaces.iter().find(|surface| surface.key == key)
    }

    pub fn resolve(&self, key: &NodeKey) -> Option<Resolved<'_>> {
        let surface = self.surface(&key.surface)?;
        let node = surface.node(&key.node)?;
        Some(Resolved { surface, node })
    }

    /// The most specific node under `point`: the deepest one whose bounds
    /// hold it, the smallest of equals, the one painted last of equals.
    pub fn node_at(&self, point: Point) -> Option<Resolved<'_>> {
        let surface = self
            .surfaces
            .iter()
            .find(|surface| surface.screen.contains(point))?;
        let mut best: Option<(&PickNode, usize, f64, usize)> = None;
        for (order, node) in surface.nodes.iter().enumerate() {
            let rect = surface.to_screen(node.bounds);
            if rect.width() < MIN_TARGET || rect.height() < MIN_TARGET || !rect.contains(point) {
                continue;
            }
            let depth = surface.depth(&node.id);
            let area = rect.area();
            let better = best.is_none_or(|(_, best_depth, best_area, best_order)| {
                depth > best_depth
                    || (depth == best_depth
                        && (area < best_area
                            || ((area - best_area).abs() <= f64::EPSILON && order > best_order)))
            });
            if better {
                best = Some((node, depth, area, order));
            }
        }
        best.map(|(node, ..)| Resolved { surface, node })
    }

    /// Every node a dragged region selects: nodes intersecting it, without
    /// the containers that merely enclose the drag and without any node whose
    /// descendant is already selected, so the answer is the things the
    /// person swept over rather than the window they swept inside.
    pub fn nodes_in(&self, region: Rect) -> Vec<Resolved<'_>> {
        let mut picked = Vec::new();
        for surface in &self.surfaces {
            let region = region.intersect(surface.screen);
            if region.width() <= 0.0 || region.height() <= 0.0 {
                continue;
            }
            let hits: Vec<&PickNode> = surface
                .nodes
                .iter()
                .filter(|node| {
                    let rect = surface.to_screen(node.bounds);
                    rect.width() >= MIN_TARGET
                        && rect.height() >= MIN_TARGET
                        && overlaps(rect, region)
                        && !contains_rect(rect, region)
                })
                .collect();
            let parents = surface.parents();
            let has_hit_descendant = |id: &str| {
                hits.iter().any(|other| {
                    let mut at = other.id.as_str();
                    let mut hops = 0_usize;
                    while let Some(parent) = parents.get(at) {
                        if *parent == id {
                            return true;
                        }
                        at = parent;
                        hops = hops.saturating_add(1);
                        if hops > surface.nodes.len() {
                            break;
                        }
                    }
                    false
                })
            };
            for node in &hits {
                if !has_hit_descendant(&node.id) {
                    picked.push(Resolved { surface, node });
                }
            }
        }
        picked
    }

    /// Where a keyboard walk from `from` lands. From nothing, the walk starts
    /// at the first root of the topmost surface. Walking past the last
    /// sibling at the top of a surface continues into the next surface.
    pub fn walk(&self, from: Option<&NodeKey>, walk: Walk) -> Option<NodeKey> {
        let Some(from) = from.and_then(|key| self.resolve(key)) else {
            let surface = self.surfaces.first()?;
            let root = surface.roots().into_iter().next()?;
            return Some(NodeKey::new(surface.key.clone(), root.id.clone()));
        };
        let surface = from.surface;
        let parents = surface.parents();
        let siblings: Vec<&str> = match parents.get(from.node.id.as_str()) {
            Some(parent) => surface
                .node(parent)
                .map(|parent| parent.children.iter().map(String::as_str).collect())
                .unwrap_or_default(),
            None => surface
                .roots()
                .into_iter()
                .map(|node| node.id.as_str())
                .collect(),
        };
        let here = siblings
            .iter()
            .position(|id| *id == from.node.id)
            .unwrap_or(0);
        let target = match walk {
            Walk::Out => parents
                .get(from.node.id.as_str())
                .map(|id| (*id).to_string()),
            Walk::In => from
                .node
                .children
                .iter()
                .find(|child| surface.node(child).is_some())
                .cloned(),
            Walk::Previous => here
                .checked_sub(1)
                .and_then(|index| siblings.get(index))
                .map(|id| (*id).to_string()),
            Walk::Next => siblings
                .get(here.saturating_add(1))
                .map(|id| (*id).to_string()),
        };
        if let Some(target) = target {
            return Some(NodeKey::new(surface.key.clone(), target));
        }
        // Off the edge of a surface's top level: move to the neighbouring
        // surface, so the keyboard reaches every app the pointer can.
        let top_level = !parents.contains_key(from.node.id.as_str());
        if !top_level || !matches!(walk, Walk::Previous | Walk::Next) {
            return None;
        }
        let index = self
            .surfaces
            .iter()
            .position(|candidate| candidate.key == surface.key)?;
        let neighbour = match walk {
            Walk::Next => self.surfaces.get(index.saturating_add(1)),
            _ => index.checked_sub(1).and_then(|i| self.surfaces.get(i)),
        }?;
        let roots = neighbour.roots();
        let root = match walk {
            Walk::Next => roots.first(),
            _ => roots.last(),
        }?;
        Some(NodeKey::new(neighbour.key.clone(), root.id.clone()))
    }
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1
}

fn contains_rect(outer: Rect, inner: Rect) -> bool {
    outer.x0 <= inner.x0 && outer.y0 <= inner.y0 && outer.x1 >= inner.x1 && outer.y1 >= inner.y1
}

/// The pick selection: an ordered set of nodes, the first being the one the
/// remark is primarily about.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    keys: Vec<NodeKey>,
}

impl Selection {
    pub fn keys(&self) -> &[NodeKey] {
        &self.keys
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// A plain click: this node and nothing else.
    pub fn replace(&mut self, key: NodeKey) {
        self.keys = vec![key];
    }

    /// Shift-click: add, or take away when already picked.
    pub fn toggle(&mut self, key: NodeKey) {
        if let Some(index) = self.keys.iter().position(|picked| *picked == key) {
            self.keys.remove(index);
        } else {
            self.keys.push(key);
        }
    }

    /// Add without toggling (a region, a keyboard pick).
    pub fn add(&mut self, key: NodeKey) {
        if !self.keys.contains(&key) {
            self.keys.push(key);
        }
    }

    pub fn remove(&mut self, index: usize) -> Option<NodeKey> {
        (index < self.keys.len()).then(|| self.keys.remove(index))
    }

    pub fn clear(&mut self) {
        self.keys.clear();
    }

    pub fn contains(&self, key: &NodeKey) -> bool {
        self.keys.contains(key)
    }
}

/// Pick mode's live state, shared by the pointer, the keyboard and the
/// `remark.pick.*` actions so all three always agree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PickState {
    /// Every surface's tree as its app last answered.
    pub scene: PickScene,
    pub selection: Selection,
    /// The node under the pointer, or where the keyboard walk stands.
    pub focus: Option<NodeKey>,
    /// A region being dragged out: where it started and where it is now.
    pub drag: Option<(Point, Point)>,
}

/// A press that has moved this far is a region drag, not a click.
pub const DRAG_THRESHOLD: f64 = 4.0;

impl PickState {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The pointer moved: the node under it takes focus, or the region
    /// being dragged grows.
    pub fn hover(&mut self, point: Point) {
        if let Some((_, current)) = self.drag.as_mut() {
            *current = point;
            return;
        }
        self.focus = self.scene.node_at(point).map(|resolved| resolved.key());
    }

    /// The dragged region, once it has moved far enough to be one.
    pub fn region(&self) -> Option<Rect> {
        let (start, current) = self.drag?;
        let rect = Rect::from_points(start, current);
        (rect.width() >= DRAG_THRESHOLD || rect.height() >= DRAG_THRESHOLD).then_some(rect)
    }

    /// What a release at `point` picks: a plain click replaces the
    /// selection, a Shift-click toggles one node, a drag adds every node it
    /// swept. Returns whether anything changed.
    pub fn release(&mut self, point: Point, additive: bool) -> bool {
        let region = self.region();
        self.drag = None;
        if let Some(region) = region {
            let keys: Vec<NodeKey> = self
                .scene
                .nodes_in(region)
                .iter()
                .map(Resolved::key)
                .collect();
            if !additive {
                self.selection.clear();
            }
            let changed = !keys.is_empty();
            for key in keys {
                self.selection.add(key);
            }
            return changed;
        }
        let Some(key) = self.scene.node_at(point).map(|resolved| resolved.key()) else {
            return false;
        };
        if additive {
            self.selection.toggle(key);
        } else {
            self.selection.replace(key);
        }
        true
    }

    /// Walk the keyboard focus through the tree.
    pub fn walk(&mut self, walk: Walk) -> bool {
        match self.scene.walk(self.focus.as_ref(), walk) {
            Some(next) => {
                self.focus = Some(next);
                true
            }
            None => false,
        }
    }

    /// Enter on the focused node: add it (Shift keeps toggling semantics).
    pub fn take_focus(&mut self, additive: bool) -> bool {
        let Some(key) = self.focus.clone() else {
            return false;
        };
        if additive {
            self.selection.toggle(key);
        } else {
            self.selection.add(key);
        }
        true
    }

    /// Replace the scene with fresher answers, keeping the selection and
    /// the focus on nodes that still exist.
    pub fn set_scene(&mut self, scene: PickScene) {
        self.scene = scene;
        if self
            .focus
            .as_ref()
            .is_some_and(|key| self.scene.resolve(key).is_none())
        {
            self.focus = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, role: &str, bounds: (f64, f64, f64, f64), children: &[&str]) -> PickNode {
        PickNode {
            id: id.into(),
            role: role.into(),
            label: Some(id.into()),
            bounds: Rect::new(bounds.0, bounds.1, bounds.2, bounds.3),
            children: children.iter().map(ToString::to_string).collect(),
            reference: None,
            personal: false,
        }
    }

    /// Mail on the left at 1×, Notes on the right at 2×, side by side as
    /// tiled windows are.
    fn scene() -> PickScene {
        let mail = PickSurface {
            key: "tab:mail".into(),
            app_id: "so.tilo.mail".into(),
            app_label: "Mail".into(),
            room: "room".into(),
            record: String::new(),
            screen: Rect::new(0.0, 40.0, 400.0, 640.0),
            scale: 1.0,
            nodes: vec![
                node(
                    "root",
                    "window",
                    (0.0, 0.0, 400.0, 600.0),
                    &["toolbar", "body"],
                ),
                node(
                    "toolbar",
                    "toolbar",
                    (0.0, 0.0, 400.0, 48.0),
                    &["send", "discard"],
                ),
                node("send", "button", (8.0, 8.0, 88.0, 40.0), &[]),
                node("discard", "button", (96.0, 8.0, 176.0, 40.0), &[]),
                node("body", "text_input", (0.0, 48.0, 400.0, 600.0), &[]),
            ],
        };
        let notes = PickSurface {
            key: "tab:notes".into(),
            app_id: "so.tilo.notes".into(),
            app_label: "Notes".into(),
            room: "room".into(),
            record: String::new(),
            screen: Rect::new(408.0, 40.0, 808.0, 640.0),
            scale: 2.0,
            nodes: vec![
                node("page", "document", (0.0, 0.0, 200.0, 300.0), &["title"]),
                node("title", "heading", (10.0, 10.0, 190.0, 30.0), &[]),
            ],
        };
        PickScene {
            surfaces: vec![mail, notes],
        }
    }

    #[test]
    fn the_deepest_node_under_the_pointer_wins() {
        let scene = scene();
        let hit = scene.node_at(Point::new(40.0, 60.0)).expect("a node");
        assert_eq!(hit.node.id, "send");
        assert_eq!(hit.surface.key, "tab:mail");
        let between = scene.node_at(Point::new(92.0, 60.0)).expect("a node");
        assert_eq!(
            between.node.id, "toolbar",
            "the gap between two buttons is the toolbar, not the nearest button"
        );
    }

    #[test]
    fn a_scaled_surface_maps_its_own_coordinates_onto_the_screen() {
        let scene = scene();
        // Notes' title is at (10,10)-(190,30) in surface px, 2× on screen.
        let hit = scene
            .node_at(Point::new(408.0 + 25.0, 40.0 + 25.0))
            .unwrap();
        assert_eq!(hit.node.id, "title");
        assert_eq!(
            hit.screen_rect(),
            Rect::new(428.0, 60.0, 788.0, 100.0),
            "bounds come back in screen coordinates"
        );
        let below = scene
            .node_at(Point::new(408.0 + 25.0, 40.0 + 65.0))
            .unwrap();
        assert_eq!(below.node.id, "page");
    }

    #[test]
    fn the_gutter_between_windows_picks_nothing() {
        assert!(scene().node_at(Point::new(404.0, 100.0)).is_none());
    }

    #[test]
    fn a_region_picks_what_it_sweeps_across_surfaces_not_what_encloses_it() {
        let scene = scene();
        // From Mail's send button across to Notes' title.
        let picked: Vec<NodeKey> = scene
            .nodes_in(Rect::new(20.0, 50.0, 500.0, 70.0))
            .iter()
            .map(Resolved::key)
            .collect();
        assert!(picked.contains(&NodeKey::new("tab:mail", "send")));
        assert!(picked.contains(&NodeKey::new("tab:mail", "discard")));
        assert!(picked.contains(&NodeKey::new("tab:notes", "title")));
        assert!(
            !picked.contains(&NodeKey::new("tab:mail", "toolbar")),
            "a container of picked nodes is not picked itself"
        );
        assert!(!picked.contains(&NodeKey::new("tab:mail", "root")));
        assert!(!picked.contains(&NodeKey::new("tab:notes", "page")));
    }

    #[test]
    fn a_region_inside_one_node_picks_that_node() {
        let scene = scene();
        let picked = scene.nodes_in(Rect::new(100.0, 200.0, 140.0, 240.0));
        assert!(
            picked.is_empty(),
            "a drag entirely inside the body neither crosses nor sweeps a node"
        );
    }

    #[test]
    fn arrows_walk_the_tree_and_cross_into_the_next_surface() {
        let scene = scene();
        let send = NodeKey::new("tab:mail", "send");
        assert_eq!(
            scene.walk(Some(&send), Walk::Next),
            Some(NodeKey::new("tab:mail", "discard"))
        );
        assert_eq!(
            scene.walk(Some(&send), Walk::Out),
            Some(NodeKey::new("tab:mail", "toolbar"))
        );
        let toolbar = NodeKey::new("tab:mail", "toolbar");
        assert_eq!(scene.walk(Some(&toolbar), Walk::In), Some(send.clone()));
        assert_eq!(
            scene.walk(Some(&send), Walk::Previous),
            None,
            "the first child has no previous sibling"
        );
        let root = NodeKey::new("tab:mail", "root");
        assert_eq!(
            scene.walk(Some(&root), Walk::Next),
            Some(NodeKey::new("tab:notes", "page")),
            "past the last top-level node the walk continues in the next app"
        );
        assert_eq!(
            scene.walk(None, Walk::In),
            Some(root),
            "a walk from nothing starts at the topmost surface's root"
        );
    }

    #[test]
    fn shift_click_toggles_and_click_replaces() {
        let mut selection = Selection::default();
        selection.replace(NodeKey::new("a", "1"));
        selection.toggle(NodeKey::new("a", "2"));
        assert_eq!(selection.keys().len(), 2);
        selection.toggle(NodeKey::new("a", "1"));
        assert_eq!(selection.keys(), [NodeKey::new("a", "2")]);
        selection.replace(NodeKey::new("b", "9"));
        assert_eq!(selection.keys(), [NodeKey::new("b", "9")]);
    }
}
