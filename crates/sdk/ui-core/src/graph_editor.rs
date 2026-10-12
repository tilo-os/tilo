//! Reusable interaction model for node/graph editors.
//!
//! This module intentionally owns no domain data. Image, audio, automation,
//! and shader editors can all project their typed nodes into [`GraphEditor`]
//! and receive the same selection, navigation, connection, viewport, and
//! accessibility semantics.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

pub type NodeId = String;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GraphPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Port {
    pub id: String,
    pub data_type: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: NodeId,
    pub title: String,
    pub position: GraphPoint,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Edge {
    pub from_node: NodeId,
    pub from_port: String,
    pub to_node: NodeId,
    pub to_port: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectError {
    UnknownNode,
    UnknownPort,
    TypeMismatch,
    InputOccupied,
    Cycle,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: BTreeMap<NodeId, GraphNode>,
    pub edges: BTreeSet<Edge>,
}

impl Graph {
    pub fn insert(&mut self, node: GraphNode) {
        self.nodes.insert(node.id.clone(), node);
    }

    /// Connect typed ports. Processing graphs are DAGs: rejecting a cycle at
    /// the edit boundary means evaluators never need a recursion escape hatch.
    ///
    /// # Errors
    ///
    /// Returns the precise structural reason when either endpoint/port is
    /// unknown, the port types differ, the input is occupied, or the edge
    /// would introduce a cycle.
    pub fn connect(&mut self, edge: Edge) -> Result<(), ConnectError> {
        let Some(from) = self.nodes.get(&edge.from_node) else {
            return Err(ConnectError::UnknownNode);
        };
        let Some(to) = self.nodes.get(&edge.to_node) else {
            return Err(ConnectError::UnknownNode);
        };
        let Some(out) = from.outputs.iter().find(|p| p.id == edge.from_port) else {
            return Err(ConnectError::UnknownPort);
        };
        let Some(input) = to.inputs.iter().find(|p| p.id == edge.to_port) else {
            return Err(ConnectError::UnknownPort);
        };
        if out.data_type != input.data_type {
            return Err(ConnectError::TypeMismatch);
        }
        if self
            .edges
            .iter()
            .any(|e| e.to_node == edge.to_node && e.to_port == edge.to_port)
        {
            return Err(ConnectError::InputOccupied);
        }
        if edge.from_node == edge.to_node || self.reachable(&edge.to_node, &edge.from_node) {
            return Err(ConnectError::Cycle);
        }
        self.edges.insert(edge);
        Ok(())
    }

    pub fn disconnect_input(&mut self, node: &str, port: &str) -> Option<Edge> {
        let edge = self
            .edges
            .iter()
            .find(|e| e.to_node == node && e.to_port == port)
            .cloned()?;
        self.edges.remove(&edge);
        Some(edge)
    }

    /// Replace one input connection atomically. If the proposed edge is
    /// invalid (including a cycle or type mismatch), the previous edge is
    /// restored so an imprecise pointer drop can never damage the graph.
    ///
    /// # Errors
    ///
    /// Returns the same structural error as [`Self::connect`] when the new
    /// edge has an invalid endpoint, incompatible type, occupied input, or
    /// would introduce a cycle.
    pub fn reconnect_input(&mut self, edge: Edge) -> Result<Option<Edge>, ConnectError> {
        let previous = self.disconnect_input(&edge.to_node, &edge.to_port);
        match self.connect(edge) {
            Ok(()) => Ok(previous),
            Err(error) => {
                if let Some(previous) = previous {
                    // This edge was valid before the attempted replacement;
                    // restoring it cannot introduce a new failure.
                    let _ = self.connect(previous);
                }
                Err(error)
            }
        }
    }

    /// The target and every node that contributes to it. Editors use this to
    /// distinguish intentional side branches from the live production path.
    pub fn contributing_nodes(&self, target: &str) -> BTreeSet<NodeId> {
        if !self.nodes.contains_key(target) {
            return BTreeSet::new();
        }
        let mut contributing = BTreeSet::new();
        let mut pending = vec![target.to_owned()];
        while let Some(node) = pending.pop() {
            if contributing.insert(node.clone()) {
                pending.extend(
                    self.edges
                        .iter()
                        .filter(|edge| edge.to_node == node)
                        .map(|edge| edge.from_node.clone()),
                );
            }
        }
        contributing
    }

    pub fn remove(&mut self, id: &str) -> Option<GraphNode> {
        let removed = self.nodes.remove(id)?;
        self.edges.retain(|e| e.from_node != id && e.to_node != id);
        Some(removed)
    }

    /// Return a stable topological order for evaluation.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectError::Cycle`] if the graph was deserialized from an
    /// invalid source that bypassed [`Graph::connect`].
    pub fn evaluation_order(&self) -> Result<Vec<NodeId>, ConnectError> {
        let mut degree = self
            .nodes
            .keys()
            .map(|id| (id.clone(), 0usize))
            .collect::<BTreeMap<_, _>>();
        for edge in &self.edges {
            if let Some(d) = degree.get_mut(&edge.to_node) {
                *d = d.saturating_add(1);
            }
        }
        let mut ready = degree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(id, _)| id.clone())
            .collect::<VecDeque<_>>();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(id) = ready.pop_front() {
            order.push(id.clone());
            for edge in self.edges.iter().filter(|e| e.from_node == id) {
                if let Some(d) = degree.get_mut(&edge.to_node) {
                    *d = d.saturating_sub(1);
                    if *d == 0 {
                        ready.push_back(edge.to_node.clone());
                    }
                }
            }
        }
        if order.len() == self.nodes.len() {
            Ok(order)
        } else {
            Err(ConnectError::Cycle)
        }
    }

    fn reachable(&self, start: &str, target: &str) -> bool {
        let mut todo = vec![start];
        let mut seen = BTreeSet::new();
        while let Some(id) = todo.pop() {
            if id == target {
                return true;
            }
            if seen.insert(id) {
                todo.extend(
                    self.edges
                        .iter()
                        .filter(|e| e.from_node == id)
                        .map(|e| e.to_node.as_str()),
                );
            }
        }
        false
    }
}

/// Device-local canvas state. It is deliberately separate from [`Graph`]: a
/// collaborator panning their view must not move everyone else's workspace.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphEditor {
    pub selection: BTreeSet<NodeId>,
    pub pan: GraphPoint,
    pub zoom: f64,
    node_drag: Option<NodeDrag>,
    pan_drag: Option<PanDrag>,
    connection_drag: Option<ConnectionDrag>,
}

#[derive(Clone, Debug, PartialEq)]
struct NodeDrag {
    node: NodeId,
    pointer_start: GraphPoint,
    node_start: GraphPoint,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PanDrag {
    pointer_start: GraphPoint,
    pan_start: GraphPoint,
}

#[derive(Clone, Debug, PartialEq)]
struct ConnectionDrag {
    from_node: NodeId,
    from_port: String,
    pointer: GraphPoint,
}

/// A stable, geometry-independent connection destination used by keyboard,
/// switch, voice, and assistive-technology interactions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionTarget {
    pub node: NodeId,
    pub port: String,
}

impl Default for GraphEditor {
    fn default() -> Self {
        Self {
            selection: BTreeSet::new(),
            pan: GraphPoint::default(),
            zoom: 1.0,
            node_drag: None,
            pan_drag: None,
            connection_drag: None,
        }
    }
}

impl GraphEditor {
    /// Compatible inputs in deterministic graph order. Applications can use
    /// this shared traversal instead of inventing pointer-only graph controls.
    pub fn connection_targets(
        graph: &Graph,
        from_node: &str,
        from_port: &str,
    ) -> Vec<ConnectionTarget> {
        let Some(output) = graph
            .nodes
            .get(from_node)
            .and_then(|node| node.outputs.iter().find(|port| port.id == from_port))
        else {
            return Vec::new();
        };
        let mut targets = Vec::new();
        for (node_id, node) in &graph.nodes {
            if node_id == from_node {
                continue;
            }
            for port in &node.inputs {
                if port.data_type == output.data_type {
                    targets.push(ConnectionTarget {
                        node: node_id.clone(),
                        port: port.id.clone(),
                    });
                }
            }
        }
        targets
    }

    /// Move through compatible inputs, wrapping at either end. `None` enters
    /// the list at its first (or last) item.
    pub fn next_connection_target(
        graph: &Graph,
        from_node: &str,
        from_port: &str,
        current: Option<&ConnectionTarget>,
        backwards: bool,
    ) -> Option<ConnectionTarget> {
        let targets = Self::connection_targets(graph, from_node, from_port);
        if targets.is_empty() {
            return None;
        }
        let current_index =
            current.and_then(|target| targets.iter().position(|candidate| candidate == target));
        let index = match (current_index, backwards) {
            (Some(index), true) => index
                .checked_sub(1)
                .unwrap_or_else(|| targets.len().saturating_sub(1)),
            (Some(index), false) => index
                .checked_add(1)
                .filter(|next| *next < targets.len())
                .unwrap_or(0),
            (None, true) => targets.len().saturating_sub(1),
            (None, false) => 0,
        };
        targets.get(index).cloned()
    }
    pub fn select_only(&mut self, id: impl Into<NodeId>) {
        self.selection.clear();
        self.selection.insert(id.into());
    }
    pub fn zoom_about(&mut self, screen: GraphPoint, factor: f64) {
        let old = self.zoom;
        self.zoom = (self.zoom * factor).clamp(0.2, 4.0);
        self.pan.x = screen.x - (screen.x - self.pan.x) * self.zoom / old;
        self.pan.y = screen.y - (screen.y - self.pan.y) * self.zoom / old;
    }
    pub fn screen_position(&self, p: GraphPoint) -> GraphPoint {
        GraphPoint {
            x: p.x * self.zoom + self.pan.x,
            y: p.y * self.zoom + self.pan.y,
        }
    }

    /// Begin moving one node from a pointer expressed in graph-screen space.
    /// The domain graph owns the durable position; this editor owns only the
    /// transient gesture so every graph-based app gets identical behavior.
    pub fn begin_node_drag(&mut self, graph: &Graph, id: &str, pointer: GraphPoint) -> bool {
        if self.pan_drag.is_some() || self.connection_drag.is_some() {
            return false;
        }
        let Some(node) = graph.nodes.get(id) else {
            return false;
        };
        self.select_only(id);
        self.node_drag = Some(NodeDrag {
            node: id.to_owned(),
            pointer_start: pointer,
            node_start: node.position,
        });
        true
    }

    /// Apply a held drag. Dividing by zoom keeps the node under the pointer at
    /// every scale rather than making movement too fast or too slow.
    pub fn update_node_drag(&self, graph: &mut Graph, pointer: GraphPoint) -> bool {
        let Some(drag) = &self.node_drag else {
            return false;
        };
        let Some(node) = graph.nodes.get_mut(&drag.node) else {
            return false;
        };
        node.position = GraphPoint {
            x: drag.node_start.x + (pointer.x - drag.pointer_start.x) / self.zoom,
            y: drag.node_start.y + (pointer.y - drag.pointer_start.y) / self.zoom,
        };
        true
    }

    pub fn end_node_drag(&mut self) -> Option<NodeId> {
        self.node_drag.take().map(|drag| drag.node)
    }

    pub fn is_dragging_node(&self) -> bool {
        self.node_drag.is_some()
    }

    /// Begin moving the viewport from empty canvas. Node and viewport drags
    /// are mutually exclusive, preventing a second touch from stealing the
    /// active gesture.
    pub fn begin_pan(&mut self, pointer: GraphPoint) -> bool {
        if self.node_drag.is_some() || self.pan_drag.is_some() || self.connection_drag.is_some() {
            return false;
        }
        self.pan_drag = Some(PanDrag {
            pointer_start: pointer,
            pan_start: self.pan,
        });
        true
    }

    pub fn update_pan(&mut self, pointer: GraphPoint) -> bool {
        let Some(drag) = self.pan_drag else {
            return false;
        };
        self.pan = GraphPoint {
            x: drag.pan_start.x + pointer.x - drag.pointer_start.x,
            y: drag.pan_start.y + pointer.y - drag.pointer_start.y,
        };
        true
    }

    pub fn end_pan(&mut self) -> bool {
        self.pan_drag.take().is_some()
    }

    pub fn is_panning(&self) -> bool {
        self.pan_drag.is_some()
    }

    pub fn begin_connection(
        &mut self,
        graph: &Graph,
        from_node: &str,
        from_port: &str,
        pointer: GraphPoint,
    ) -> bool {
        if self.node_drag.is_some() || self.pan_drag.is_some() || self.connection_drag.is_some() {
            return false;
        }
        let valid_output = graph
            .nodes
            .get(from_node)
            .is_some_and(|node| node.outputs.iter().any(|port| port.id == from_port));
        if !valid_output {
            return false;
        }
        self.connection_drag = Some(ConnectionDrag {
            from_node: from_node.to_owned(),
            from_port: from_port.to_owned(),
            pointer,
        });
        true
    }

    pub fn update_connection(&mut self, pointer: GraphPoint) -> bool {
        let Some(drag) = &mut self.connection_drag else {
            return false;
        };
        drag.pointer = pointer;
        true
    }

    pub fn connection_preview(&self) -> Option<(&str, &str, GraphPoint)> {
        self.connection_drag.as_ref().map(|drag| {
            (
                drag.from_node.as_str(),
                drag.from_port.as_str(),
                drag.pointer,
            )
        })
    }

    /// Complete the active connection gesture against a target input.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectError::UnknownPort`] when no gesture is active, or
    /// forwards the graph's validation error when the proposed edge is not
    /// structurally valid.
    pub fn finish_connection(
        &mut self,
        graph: &mut Graph,
        to_node: &str,
        to_port: &str,
    ) -> Result<Option<Edge>, ConnectError> {
        let Some(drag) = self.connection_drag.take() else {
            return Err(ConnectError::UnknownPort);
        };
        graph.reconnect_input(Edge {
            from_node: drag.from_node,
            from_port: drag.from_port,
            to_node: to_node.to_owned(),
            to_port: to_port.to_owned(),
        })
    }

    pub fn is_connecting(&self) -> bool {
        self.connection_drag.is_some()
    }
}

/// Shared responsive geometry for editors whose approachable surface is a
/// primary canvas plus a reversible operation stack, with an optional expert
/// graph. Future audio/shader editors use the same break point and guarantees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdaptiveEditorLayout {
    pub canvas: ui_placeholder::Rect,
    pub stack: ui_placeholder::Rect,
    pub inspector: ui_placeholder::Rect,
}

/// Small geometry type kept independent of the renderer so graph tooling can
/// be tested and reused by non-Vello frontends.
pub mod ui_placeholder {
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    pub struct Rect {
        pub x0: f64,
        pub y0: f64,
        pub x1: f64,
        pub y1: f64,
    }
    impl Rect {
        pub fn width(self) -> f64 {
            self.x1 - self.x0
        }
        pub fn height(self) -> f64 {
            self.y1 - self.y0
        }
    }
}
impl AdaptiveEditorLayout {
    pub fn resolve(width: f64, height: f64) -> Self {
        // Reserve a genuine two-line status region. Graph-based editors often
        // report recomputation/export outcomes; a 28px sliver clipped wrapped
        // feedback on phones and at enlarged text sizes.
        const STATUS_HEIGHT: f64 = 48.0;
        let content = ui_placeholder::Rect {
            x0: 0.0,
            y0: 92.0,
            x1: width,
            y1: (height - STATUS_HEIGHT).max(92.0),
        };
        let (canvas, stack) = if width >= 820.0 {
            let split = (width - 310.0).max(0.0);
            (
                ui_placeholder::Rect {
                    x1: (split - 1.0).max(0.0),
                    ..content
                },
                ui_placeholder::Rect {
                    x0: split,
                    ..content
                },
            )
        } else {
            let split = (content.y1 - 300.0).max(content.y0);
            (
                ui_placeholder::Rect {
                    y1: (split - 1.0).max(content.y0),
                    ..content
                },
                ui_placeholder::Rect {
                    y0: split,
                    ..content
                },
            )
        };
        let inspector = ui_placeholder::Rect {
            x0: stack.x0 + 12.0,
            y0: (stack.y1 - 68.0).max(stack.y0),
            x1: (stack.x1 - 12.0).max(stack.x0 + 12.0),
            y1: (stack.y1 - 8.0).max(stack.y0),
        };
        Self {
            canvas,
            stack,
            inspector,
        }
    }
    pub fn has_no_overlap(self) -> bool {
        if self.canvas.width() <= 0.0 || self.canvas.height() <= 0.0 {
            return true;
        }
        self.canvas.x1 <= self.stack.x0 || self.canvas.y1 <= self.stack.y0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn node(id: &str) -> GraphNode {
        GraphNode {
            id: id.into(),
            title: id.into(),
            position: GraphPoint::default(),
            inputs: vec![Port {
                id: "in".into(),
                data_type: "image".into(),
                label: "Image".into(),
            }],
            outputs: vec![Port {
                id: "out".into(),
                data_type: "image".into(),
                label: "Image".into(),
            }],
            enabled: true,
        }
    }
    fn edge(a: &str, b: &str) -> Edge {
        Edge {
            from_node: a.into(),
            from_port: "out".into(),
            to_node: b.into(),
            to_port: "in".into(),
        }
    }
    #[test]
    fn connections_refuse_cycles_and_invalid_targets_and_commit_requested_inputs() {
        // Case `cycles_are_refused_without_mutating_the_graph`.
        {
            let mut g = Graph::default();
            for n in ["a", "b", "c"] {
                g.insert(node(n));
            }
            g.connect(edge("a", "b")).unwrap();
            g.connect(edge("b", "c")).unwrap();
            assert_eq!(g.connect(edge("c", "a")), Err(ConnectError::Cycle));
            assert_eq!(g.edges.len(), 2);
        }

        // Case `invalid_reconnection_restores_the_previous_edge`.
        {
            let mut graph = Graph::default();
            for id in ["source", "grade", "output"] {
                graph.insert(node(id));
            }
            graph.connect(edge("source", "grade")).unwrap();
            graph.connect(edge("grade", "output")).unwrap();
            let before = graph.edges.clone();

            assert_eq!(
                graph.reconnect_input(edge("output", "grade")),
                Err(ConnectError::Cycle)
            );
            assert_eq!(graph.edges, before);
        }

        // Case `connection_drag_commits_to_the_requested_input`.
        {
            let mut graph = Graph::default();
            graph.insert(node("source"));
            graph.insert(node("output"));
            let mut editor = GraphEditor::default();
            assert!(editor.begin_connection(
                &graph,
                "source",
                "out",
                GraphPoint { x: 20.0, y: 30.0 }
            ));
            assert!(editor.update_connection(GraphPoint { x: 80.0, y: 50.0 }));
            assert_eq!(
                editor.connection_preview(),
                Some(("source", "out", GraphPoint { x: 80.0, y: 50.0 }))
            );
            assert_eq!(
                editor.finish_connection(&mut graph, "output", "in"),
                Ok(None)
            );
            assert!(graph.edges.contains(&edge("source", "output")));
            assert!(!editor.is_connecting());
        }

        // Case `keyboard_connection_targets_are_stable_compatible_and_wrap`.
        {
            let mut graph = Graph::default();
            for id in ["source", "first", "second"] {
                graph.insert(node(id));
            }
            graph.nodes.get_mut("second").unwrap().inputs.push(Port {
                id: "mask".into(),
                data_type: "mask".into(),
                label: "Mask".into(),
            });

            let targets = GraphEditor::connection_targets(&graph, "source", "out");
            assert_eq!(
                targets,
                vec![
                    ConnectionTarget {
                        node: "first".into(),
                        port: "in".into()
                    },
                    ConnectionTarget {
                        node: "second".into(),
                        port: "in".into()
                    },
                ]
            );
            let first =
                GraphEditor::next_connection_target(&graph, "source", "out", None, false).unwrap();
            let second =
                GraphEditor::next_connection_target(&graph, "source", "out", Some(&first), false)
                    .unwrap();
            assert_eq!(second.node, "second");
            assert_eq!(
                GraphEditor::next_connection_target(&graph, "source", "out", Some(&second), false,),
                Some(first)
            );
        }
    }

    #[test]
    fn pointer_drags_pan_and_zoom_track_graph_and_screen_space() {
        // Case `node_drag_tracks_pointer_in_graph_space_at_any_zoom`.
        {
            let mut graph = Graph::default();
            graph.insert(node("grade"));
            graph.nodes.get_mut("grade").unwrap().position = GraphPoint { x: 40.0, y: 60.0 };
            let mut editor = GraphEditor {
                zoom: 2.0,
                ..GraphEditor::default()
            };

            assert!(editor.begin_node_drag(&graph, "grade", GraphPoint { x: 100.0, y: 120.0 }));
            assert!(editor.update_node_drag(&mut graph, GraphPoint { x: 140.0, y: 100.0 }));
            assert_eq!(
                graph.nodes["grade"].position,
                GraphPoint { x: 60.0, y: 50.0 }
            );
            assert_eq!(editor.end_node_drag().as_deref(), Some("grade"));
            assert!(!editor.is_dragging_node());
        }

        // Case `viewport_pan_is_exclusive_and_tracks_screen_delta`.
        {
            let mut graph = Graph::default();
            graph.insert(node("grade"));
            let mut editor = GraphEditor::default();
            assert!(editor.begin_pan(GraphPoint { x: 40.0, y: 60.0 }));
            assert!(!editor.begin_node_drag(&graph, "grade", GraphPoint { x: 40.0, y: 60.0 }));
            assert!(editor.update_pan(GraphPoint { x: 75.0, y: 42.0 }));
            assert_eq!(editor.pan, GraphPoint { x: 35.0, y: -18.0 });
            assert!(editor.end_pan());
            assert!(!editor.is_panning());
        }

        // Case `zoom_keeps_the_point_under_the_pointer`.
        {
            let mut e = GraphEditor::default();
            let p = GraphPoint { x: 240.0, y: 120.0 };
            let before = e.screen_position(p);
            e.zoom_about(before, 2.0);
            assert_eq!(e.screen_position(p), before);
        }

        // Case `adaptive_canvas_and_stack_never_overlap`.
        {
            for (w, h) in [(1280.0, 800.0), (760.0, 700.0), (390.0, 640.0)] {
                let l = AdaptiveEditorLayout::resolve(w, h);
                assert!(l.has_no_overlap());
                assert!(
                    l.stack.x0 >= 0.0 && l.stack.y0 >= 92.0 && l.stack.x1 <= w && l.stack.y1 <= h
                );
                assert!(
                    l.stack.y1 <= h - 48.0 || h < 140.0,
                    "editor content preserves the shared status region"
                );
            }
        }
    }

    #[test]
    fn evaluation_order_follows_dependencies_and_contributing_nodes() {
        // Case `order_follows_dependencies_not_visual_position`.
        {
            let mut g = Graph::default();
            for n in ["output", "source", "grade"] {
                g.insert(node(n));
            }
            g.connect(edge("source", "grade")).unwrap();
            g.connect(edge("grade", "output")).unwrap();
            assert_eq!(g.evaluation_order().unwrap(), ["source", "grade", "output"]);
        }

        // Case `contributing_nodes_exclude_disconnected_side_branches`.
        {
            let mut graph = Graph::default();
            for id in ["source", "grade", "unused", "output"] {
                graph.insert(node(id));
            }
            graph.connect(edge("source", "grade")).unwrap();
            graph.connect(edge("grade", "output")).unwrap();
            graph.connect(edge("source", "unused")).unwrap();
            assert_eq!(
                graph.contributing_nodes("output"),
                BTreeSet::from(["source".into(), "grade".into(), "output".into()])
            );
        }
    }
}
