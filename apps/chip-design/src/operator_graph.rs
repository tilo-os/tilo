//! Structural operator design. Connections do not qualify an executable netlist.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ui::automerge::{ObjId, ObjType, ROOT, ReadDoc, Value, transaction::Transactable};
use ui::graph_editor::{ConnectError, Edge, Graph, GraphNode, GraphPoint, Port};
use ui::tilo_actions::{Action, ActionError, ActionParam, ParamType};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};

pub const RESOURCE_ID: &str = "chip_design.operator_graph";
const NODES: &str = "operator_nodes";
const EDGES: &str = "operator_edges";
const MAX_NODES: usize = 64;
const MAX_EDGES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorKind {
    Input,
    GateInput,
    NormWeightInput,
    RmsNorm,
    ApplyNormWeight,
    Silu,
    Multiply,
    ProjectionInput,
}

impl OperatorKind {
    pub const ALL: [Self; 8] = [
        Self::Input,
        Self::GateInput,
        Self::NormWeightInput,
        Self::RmsNorm,
        Self::ApplyNormWeight,
        Self::Silu,
        Self::Multiply,
        Self::ProjectionInput,
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
            Self::ProjectionInput => "chip-op-projection",
        }
    }

    fn ports(self, shape: &[u16], normalization_width: u16) -> (Vec<Port>, Vec<Port>) {
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
            Self::Multiply => (
                vec![values("left"), values("right")],
                vec![values("result")],
            ),
            Self::ProjectionInput => (vec![values("value")], vec![]),
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorNode {
    pub id: String,
    pub kind: OperatorKind,
    pub shape: Vec<u16>,
    pub normalization_width: u16,
    pub x: u16,
    pub y: u16,
}

impl OperatorNode {
    pub fn visual(&self) -> GraphNode {
        let (inputs, outputs) = self.kind.ports(&self.shape, self.normalization_width);
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum OperatorAction {
    AddNode { node: OperatorNode },
    MoveNode { node: String, x: u16, y: u16 },
    RemoveNode { node: String },
    Connect { edge: Edge },
    Disconnect { node: String, port: String },
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
}

fn refuse(message: &str) -> ActionError {
    ActionError::Refused(message.into())
}

fn map(doc: &Doc, key: &str) -> Result<ObjId, ModelError> {
    match doc.get(ROOT, key)? {
        Some((Value::Object(ObjType::Map), id)) => Ok(id),
        _ => Err(ModelError::invalid(
            "operator collection must be initialized map",
        )),
    }
}

fn put_node(doc: &mut Doc, collection: &ObjId, node: &OperatorNode) -> Result<(), ActionError> {
    let json = serde_json::to_string(node).map_err(|error| refuse(&error.to_string()))?;
    doc.put(collection, &node.id, json)?;
    Ok(())
}

impl Action for OperatorAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let state = OperatorGraph::read(doc).map_err(|error| refuse(&error.to_string()))?;
        let nodes = map(doc, NODES).map_err(|error| refuse(&error.to_string()))?;
        let edges = map(doc, EDGES).map_err(|error| refuse(&error.to_string()))?;
        match self {
            Self::AddNode { node } => {
                if !valid_node(node) {
                    return Err(refuse("invalid node identity or position"));
                }
                if let Some(existing) = state.nodes.get(&node.id) {
                    return if existing == node {
                        Ok(())
                    } else {
                        Err(refuse("node identity already exists"))
                    };
                }
                if state.nodes.len() >= MAX_NODES {
                    return Err(refuse("operator node limit reached"));
                }
                put_node(doc, &nodes, node)?;
            }
            Self::MoveNode { node, x, y } => {
                let mut moved = state
                    .nodes
                    .get(node)
                    .cloned()
                    .ok_or_else(|| refuse("unknown node"))?;
                if *x > 8192 || *y > 8192 {
                    return Err(refuse("invalid node position"));
                }
                moved.x = *x;
                moved.y = *y;
                put_node(doc, &nodes, &moved)?;
            }
            Self::RemoveNode { node } => {
                if !state.nodes.contains_key(node) {
                    return Err(refuse("unknown node"));
                }
                doc.delete(&nodes, node)?;
                for (id, edge) in &state.candidates {
                    if edge.from_node == *node || edge.to_node == *node {
                        doc.delete(&edges, id)?;
                    }
                }
            }
            Self::Connect { edge } => {
                let encoded =
                    serde_json::to_string(edge).map_err(|error| refuse(&error.to_string()))?;
                let id = blake3::hash(encoded.as_bytes()).to_hex().to_string();
                if state.active.contains(edge) {
                    return Ok(());
                }
                if state.candidates.len() >= MAX_EDGES {
                    return Err(refuse("operator edge limit reached"));
                }
                state
                    .graph()
                    .connect(edge.clone())
                    .map_err(|error| refuse(reason(&error)))?;
                doc.put(&edges, id, encoded)?;
            }
            Self::Disconnect { node, port } => {
                let graph = state.graph();
                if !graph
                    .nodes
                    .get(node)
                    .is_some_and(|n| n.inputs.iter().any(|p| p.id == *port))
                {
                    return Err(refuse("unknown input port"));
                }
                for (id, edge) in &state.candidates {
                    if edge.to_node == *node && edge.to_port == *port {
                        doc.delete(&edges, id)?;
                    }
                }
            }
        }
        Ok(())
    }
}

pub struct OperatorGraph;
impl Resource for OperatorGraph {
    const KIND: &'static str = "chip_design.operator_graph";
    const LABEL: &'static str = "Operator graph";
    const ACTION_TYPE: &'static str = "chip_design.operator_edit";
    type State = OperatorState;
    type Action = OperatorAction;

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        for key in [NODES, EDGES] {
            match doc.get(ROOT, key)? {
                None => {
                    doc.put_object(ROOT, key, ObjType::Map)?;
                }
                Some((Value::Object(ObjType::Map), _)) => {}
                _ => return Err(ModelError::invalid("invalid operator collection")),
            }
        }
        Ok(())
    }

    fn read(doc: &Doc) -> Result<OperatorState, ModelError> {
        let mut state = OperatorState::default();
        if doc.get(ROOT, NODES)?.is_none() {
            return Ok(state);
        }
        let nodes = map(doc, NODES)?;
        for id in doc.keys(&nodes) {
            let Some((value, _)) = doc.get(&nodes, &id)? else {
                continue;
            };
            let node: OperatorNode = serde_json::from_str(
                value
                    .as_str()
                    .ok_or_else(|| ModelError::invalid("invalid operator node"))?,
            )
            .map_err(|error| ModelError::invalid(error.to_string()))?;
            if node.id != id || !valid_node(&node) {
                return Err(ModelError::invalid(
                    "invalid operator node identity or position",
                ));
            }
            state.nodes.insert(id, node);
        }
        let edges = map(doc, EDGES)?;
        for id in doc.keys(&edges) {
            let Some((value, _)) = doc.get(&edges, &id)? else {
                continue;
            };
            let raw = value
                .as_str()
                .ok_or_else(|| ModelError::invalid("invalid operator edge"))?;
            let edge: Edge = serde_json::from_str(raw)
                .map_err(|error| ModelError::invalid(error.to_string()))?;
            if blake3::hash(raw.as_bytes()).to_hex().as_str() != id {
                return Err(ModelError::invalid("operator edge identity mismatch"));
            }
            state.candidates.insert(id, edge);
        }
        state.resolve();
        Ok(state)
    }

    const READABLE: bool = true;
    fn read_content(
        state: &OperatorState,
        _doc: &Doc,
        _item: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        Ok(
            serde_json::json!({"qualification":"structural_design_only", "graph":state.graph(), "nodes":state.nodes,
            "candidates":state.candidates,"active":state.active,"rejected":state.rejected}),
        )
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            add_declaration(),
            connect_declaration(),
            disconnect_declaration(),
            move_declaration(),
            remove_declaration(),
        ]
    }
}

fn text(name: &'static str, label: &'static str) -> ActionParam {
    ActionParam::required(name, ParamType::Text, label)
}
fn position(args: &ui::tilo_actions::ActionArgs, name: &str) -> Result<u16, String> {
    u16::try_from(args.integer(name).ok_or("missing integer")?).map_err(|error| error.to_string())
}
fn add_declaration() -> ActionDecl<OperatorGraph> {
    ActionDecl::new(
        "add-node",
        "Add operator",
        vec![
            text("node", "Stable node identity"),
            text("kind", "Operator kind"),
            ActionParam::required(
                "shape",
                ParamType::Array,
                "F32 tensor shape, up to four positive dimensions",
            ),
            ActionParam::required(
                "normalization_width",
                ParamType::Integer,
                "Last-axis normalization width",
            ),
            ActionParam::required("x", ParamType::Integer, "Horizontal position"),
            ActionParam::required("y", ParamType::Integer, "Vertical position"),
        ],
        |args, _| {
            let kind: OperatorKind = serde_json::from_value(serde_json::Value::String(
                args.text("kind").ok_or("missing kind")?.into(),
            ))
            .map_err(|error| error.to_string())?;
            Ok(OperatorAction::AddNode {
                node: OperatorNode {
                    id: args.text("node").ok_or("missing node")?.into(),
                    kind,
                    shape: args
                        .array("shape")
                        .ok_or("missing shape")?
                        .iter()
                        .map(|d| {
                            d.as_u64()
                                .and_then(|n| u16::try_from(n).ok())
                                .ok_or_else(|| "invalid shape dimension".to_owned())
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    normalization_width: position(args, "normalization_width")?,
                    x: position(args, "x")?,
                    y: position(args, "y")?,
                },
            })
        },
    )
    .describe("Edit a structural operator design; does not execute or qualify a netlist.")
}
fn connect_declaration() -> ActionDecl<OperatorGraph> {
    ActionDecl::new(
        "connect",
        "Connect operator ports",
        vec![
            text("from_node", "Source node"),
            text("from_port", "Output port"),
            text("to_node", "Destination node"),
            text("to_port", "Input port"),
        ],
        |args, _| {
            Ok(OperatorAction::Connect {
                edge: Edge {
                    from_node: args.text("from_node").ok_or("missing source node")?.into(),
                    from_port: args.text("from_port").ok_or("missing output port")?.into(),
                    to_node: args
                        .text("to_node")
                        .ok_or("missing destination node")?
                        .into(),
                    to_port: args.text("to_port").ok_or("missing input port")?.into(),
                },
            })
        },
    )
}
fn disconnect_declaration() -> ActionDecl<OperatorGraph> {
    ActionDecl::new(
        "disconnect",
        "Disconnect input",
        vec![text("node", "Destination node"), text("port", "Input port")],
        |args, _| {
            Ok(OperatorAction::Disconnect {
                node: args.text("node").ok_or("missing node")?.into(),
                port: args.text("port").ok_or("missing port")?.into(),
            })
        },
    )
}
fn move_declaration() -> ActionDecl<OperatorGraph> {
    ActionDecl::new(
        "move-node",
        "Move operator",
        vec![
            text("node", "Node identity"),
            ActionParam::required("x", ParamType::Integer, "Horizontal position"),
            ActionParam::required("y", ParamType::Integer, "Vertical position"),
        ],
        |args, _| {
            Ok(OperatorAction::MoveNode {
                node: args.text("node").ok_or("missing node")?.into(),
                x: position(args, "x")?,
                y: position(args, "y")?,
            })
        },
    )
}
fn remove_declaration() -> ActionDecl<OperatorGraph> {
    ActionDecl::new(
        "remove-node",
        "Remove operator",
        vec![text("node", "Node identity")],
        |args, _| {
            Ok(OperatorAction::RemoveNode {
                node: args.text("node").ok_or("missing node")?.into(),
            })
        },
    )
}
