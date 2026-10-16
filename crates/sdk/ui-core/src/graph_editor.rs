//! Reusable graph model for node/graph editors.
//!
//! This module intentionally owns no domain data. Image, audio, automation,
//! and shader editors can all project their typed nodes into a [`Graph`] and
//! receive the same cycle-free connection rules and keyboard-reachable
//! connection targets.

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

/// A stable, geometry-independent connection destination used by keyboard,
/// switch, voice, and assistive-technology interactions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionTarget {
    pub node: NodeId,
    pub port: String,
}

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

        // Case `keyboard_connection_targets_are_stable_and_compatible`.
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

            let targets = connection_targets(&graph, "source", "out");
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
        }
    }

    #[test]
    fn evaluation_order_follows_dependencies_not_visual_position() {
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
    }
}
