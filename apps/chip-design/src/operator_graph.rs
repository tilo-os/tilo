//! Structural operator design. Connections do not qualify an executable netlist.
//!
//! The nodes and connections are fields of the project
//! ([`crate::resource::Project`]); this module holds their types, the rules a
//! node keeps and how the stored connections resolve into the graph shown.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ui::graph_editor::{ConnectError, Edge, Graph, GraphNode, GraphPoint, Port};
use ui::tilo_resource::{self, Id, Identified, JsonSchema, Model, Refusal};

/// The id the operator graph is published under.
pub const RESOURCE_ID: &str = "chip_design.project/derived/operator-graph";
pub const MAX_NODES: usize = 64;
pub const MAX_EDGES: usize = 256;

#[derive(Model, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperatorKind {
    #[default]
    Input,
    GateInput,
    NormWeightInput,
    RmsNorm,
    ApplyNormWeight,
    Silu,
    Multiply,
    ProjectionInput,
    MatrixWeightInput,
    LinearProjection,
    Add,
}

impl OperatorKind {
    pub const ALL: [Self; 11] = [
        Self::Input,
        Self::GateInput,
        Self::NormWeightInput,
        Self::RmsNorm,
        Self::ApplyNormWeight,
        Self::Silu,
        Self::Multiply,
        Self::Add,
        Self::ProjectionInput,
        Self::MatrixWeightInput,
        Self::LinearProjection,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::Input => "chip-op-input",
            Self::GateInput => "chip-op-gate",
            Self::NormWeightInput => "chip-op-weight",
            Self::RmsNorm => "chip-op-rms",
            Self::ApplyNormWeight => "chip-op-apply-weight",
            Self::Silu => "chip-op-silu",
            Self::Multiply => "chip-op-multiply",
            Self::Add => "chip-op-add",
            Self::ProjectionInput => "chip-op-projection",
            Self::MatrixWeightInput => "chip-op-matrix-weights",
            Self::LinearProjection => "chip-op-linear-projection",
        }
    }

    fn ports(
        self,
        shape: &[u16],
        normalization_width: u16,
        output_width: Option<u32>,
    ) -> (Vec<Port>, Vec<Port>) {
        let values_type = format!("f32{shape:?}");
        let weights_type = format!("f32[{normalization_width}]");
        let values = |id: &str| port(id, &values_type);
        let weights = |id: &str| port(id, &weights_type);
        match self {
            Self::Input | Self::GateInput => (vec![], vec![values("value")]),
            Self::NormWeightInput => (vec![], vec![weights("weights")]),
            Self::RmsNorm | Self::Silu => (vec![values("value")], vec![values("result")]),
            Self::ApplyNormWeight => (
                vec![values("value"), weights("weights")],
                vec![values("result")],
            ),
            Self::Multiply | Self::Add => (
                vec![values("left"), values("right")],
                vec![values("result")],
            ),
            Self::ProjectionInput => (vec![values("value")], vec![]),
            Self::MatrixWeightInput | Self::LinearProjection => {
                let width = output_width.unwrap_or(0);
                let matrix = port("weights", &format!("f32[{width}, {normalization_width}]"));
                if self == Self::MatrixWeightInput {
                    (vec![], vec![matrix])
                } else {
                    let mut output: Vec<u32> = shape.iter().map(|d| u32::from(*d)).collect();
                    if let Some(last) = output.last_mut() {
                        *last = width;
                    }
                    (
                        vec![values("value"), matrix],
                        vec![port("result", &format!("f32{output:?}"))],
                    )
                }
            }
        }
    }
}

fn port(id: &str, data_type: &str) -> Port {
    Port {
        id: id.into(),
        label: id.into(),
        data_type: data_type.into(),
    }
}

#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[model(check = OperatorNode::valid)]
pub struct OperatorNode {
    /// Stable node identity.
    #[model(readonly)]
    pub id: String,
    /// Operator
    #[model(readonly)]
    pub kind: OperatorKind,
    /// F32 tensor shape, up to four positive dimensions; its last axis is the
    /// normalization width.
    #[model(readonly)]
    pub shape: Vec<u16>,
    /// Last-axis normalization width.
    #[model(readonly)]
    pub normalization_width: u16,
    /// Structural matrix output width; no matrix payload is held in the
    /// document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_width: Option<u32>,
    /// Horizontal position
    pub x: u16,
    /// Vertical position
    pub y: u16,
}

impl Identified for OperatorNode {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }
}

/// One stored connection from an output port to an input port. Concurrent
/// connections merge per connection; the graph shown resolves them
/// (`OperatorState::resolve`) and reports the ones that conflict.
#[derive(
    Model, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub struct Connection {
    pub from_node: String,
    pub from_port: String,
    pub to_node: String,
    pub to_port: String,
}

impl From<Edge> for Connection {
    fn from(edge: Edge) -> Self {
        Self {
            from_node: edge.from_node,
            from_port: edge.from_port,
            to_node: edge.to_node,
            to_port: edge.to_port,
        }
    }
}

impl From<&Connection> for Edge {
    fn from(connection: &Connection) -> Self {
        Self {
            from_node: connection.from_node.clone(),
            from_port: connection.from_port.clone(),
            to_node: connection.to_node.clone(),
            to_port: connection.to_port.clone(),
        }
    }
}

impl Connection {
    /// Its stable identity: the hash of its JSON form.
    pub fn id(&self) -> String {
        let encoded = serde_json::to_string(&Edge::from(self)).unwrap_or_default();
        blake3::hash(encoded.as_bytes()).to_hex().to_string()
    }
}

impl OperatorNode {
    pub fn visual(&self) -> GraphNode {
        let (inputs, outputs) =
            self.kind
                .ports(&self.shape, self.normalization_width, self.output_width);
        GraphNode {
            id: self.id.clone(),
            title: self.kind.key().into(),
            position: GraphPoint {
                x: f64::from(self.x),
                y: f64::from(self.y),
            },
            inputs,
            outputs,
            enabled: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedEdge {
    pub id: String,
    pub edge: Edge,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorState {
    pub nodes: BTreeMap<String, OperatorNode>,
    pub candidates: BTreeMap<String, Edge>,
    pub active: Vec<Edge>,
    pub rejected: Vec<RejectedEdge>,
}

impl OperatorState {
    pub fn graph(&self) -> Graph {
        let mut graph = Graph::default();
        for node in self.nodes.values() {
            graph.insert(node.visual());
        }
        graph.edges.extend(self.active.iter().cloned());
        graph
    }

    /// The graph the stored `nodes` and `connections` make: every
    /// connection that still connects, and the ones that conflict with why.
    pub fn of<'a>(
        nodes: impl IntoIterator<Item = &'a OperatorNode>,
        connections: impl IntoIterator<Item = &'a Connection>,
    ) -> Self {
        let mut state = Self {
            nodes: nodes
                .into_iter()
                .map(|node| (node.id.clone(), node.clone()))
                .collect(),
            candidates: connections
                .into_iter()
                .map(|connection| (connection.id(), Edge::from(connection)))
                .collect(),
            ..Self::default()
        };
        state.resolve();
        state
    }

    fn resolve(&mut self) {
        self.active.clear();
        self.rejected.clear();
        let mut graph = self.graph();
        for (id, edge) in &self.candidates {
            match graph.connect(edge.clone()) {
                Ok(()) => self.active.push(edge.clone()),
                Err(error) => self.rejected.push(RejectedEdge {
                    id: id.clone(),
                    edge: edge.clone(),
                    reason: reason(&error).into(),
                }),
            }
        }
    }
}

pub fn reason(error: &ConnectError) -> &'static str {
    match error {
        ConnectError::UnknownNode => "unknown_node",
        ConnectError::UnknownPort => "unknown_port",
        ConnectError::TypeMismatch => "type_mismatch",
        ConnectError::InputOccupied => "input_occupied",
        ConnectError::Cycle => "cycle",
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn valid_node(node: &OperatorNode) -> bool {
    valid_id(&node.id)
        && node.x <= 8192
        && node.y <= 8192
        && !node.shape.is_empty()
        && node.shape.len() <= 4
        && node.shape.iter().all(|d| *d > 0)
        && node.shape.iter().map(|d| u64::from(*d)).product::<u64>() <= 65536
        && node.shape.last() == Some(&node.normalization_width)
        && (node.kind != OperatorKind::NormWeightInput || node.shape == [node.normalization_width])
        && valid_matrix(node)
}

fn valid_matrix(node: &OperatorNode) -> bool {
    let matrix = matches!(
        node.kind,
        OperatorKind::MatrixWeightInput | OperatorKind::LinearProjection
    );
    match (matrix, node.output_width) {
        (false, None) => true,
        (true, Some(width)) if (1..=1_048_576).contains(&width) => {
            let input = u64::from(node.normalization_width);
            let elements = node.shape.iter().map(|d| u64::from(*d)).product::<u64>();
            input > 0
                && input
                    .checked_mul(u64::from(width))
                    .is_some_and(|size| size <= (1_u64 << 32))
                && elements
                    .checked_div(input)
                    .and_then(|batch| batch.checked_mul(u64::from(width)))
                    .is_some_and(|size| size <= (1_u64 << 32))
                && (node.kind != OperatorKind::MatrixWeightInput
                    || node.shape == [node.normalization_width])
        }
        _ => false,
    }
}

impl OperatorNode {
    /// A node keeps a stable identity, a position on the canvas and a shape
    /// its operator can take.
    fn valid(&self) -> Result<(), Refusal> {
        if valid_node(self) {
            Ok(())
        } else {
            Err(Refusal::new(
                "invalid operator node identity, position, shape or matrix width",
            ))
        }
    }

    /// This node with the input shape `shape`, whose last axis sets the
    /// normalization width.
    #[must_use]
    pub fn with_shape(&self, shape: Vec<u16>) -> Self {
        Self {
            normalization_width: shape.last().copied().unwrap_or(0),
            shape,
            ..self.clone()
        }
    }
}
