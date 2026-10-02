//! Granular, replicated image-editor project document.
//!
//! A single JSON scalar would converge on one whole-project value, so two
//! people changing different adjustments would still lose work. Every
//! independently editable thing instead has its own CRDT location: project metadata, nodes, operation fields, order,
//! expert connection overrides, and delete tombstones.
//!
//! `order` is the approachable adjustment stack. Concurrent inserts use
//! Automerge list semantics. `connections` only stores expert deviations from
//! that stack. Tombstones win over concurrent edits, and node IDs never get
//! reused, so delayed updates cannot resurrect a deleted adjustment.
//! Cursor/selection, canvas pan/zoom and hover are deliberately per-person
//! presentation state. The normal runtime presence channel can show them to
//! collaborators, but opening a project starts with no asserted presence and
//! never makes one participant's viewport jump another's.

use crate::model::{Operation, Project};
use automerge::{
    AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable,
};
use ui::doc::{read_bool, read_number, read_str, read_u64};
use ui::graph_editor::{Edge, Graph, GraphNode, GraphPoint, Port};

const V2_KEY: &str = "image-editor.project.v2";
const METADATA: &str = "metadata";
const NODES: &str = "nodes";
const OPERATIONS: &str = "operations";
const ORDER: &str = "order";
const CONNECTIONS: &str = "connections";
const TOMBSTONES: &str = "tombstones";
const HISTORY_HIDDEN: &str = "history_hidden";

/// The actor of the shared blank-project change. Nobody edits as it.
const GENESIS_ACTOR: &[u8] = b"so.tilo.image-editor.project.v2.genesis";

/// The canonical blank-project change, built once per process. `Project::blank`
/// and every writer it drives iterate ordered collections, so the change is
/// the same bytes on every machine.
fn blank_genesis() -> Option<&'static [u8]> {
    static GENESIS: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    let bytes = GENESIS.get_or_init(|| {
        let mut doc = AutoCommit::new();
        doc.set_actor(automerge::ActorId::from(GENESIS_ACTOR));
        ProjectDoc::seed(&mut doc);
        doc.commit_with(automerge::transaction::CommitOptions::default().with_time(0));
        doc.save()
    });
    (!bytes.is_empty()).then_some(bytes.as_slice())
}

/// Stateless access to the one canonical image project document.
pub struct ProjectDoc;

impl ProjectDoc {
    /// Seed the blank project into a document that has none yet.
    pub fn seed(doc: &mut AutoCommit) {
        if Self::v2(doc).is_some() {
            return;
        }
        Self::write_fresh(doc, &Project::blank());
    }

    /// Give a document that has no project yet the *shared* blank project,
    /// then [`Self::seed`]. Called where the app opens the document (`sync`),
    /// outside any action transaction.
    ///
    /// `seed` writes the blank project under the current actor, so two
    /// replicas that each open the same empty record before syncing mint two
    /// `image-editor.project.v2` maps; a merge keeps one and every adjustment
    /// made under the other disappears. The blank project is instead adopted
    /// from one canonical change (fixed actor, no dependencies, time 0), which
    /// is byte-identical on every replica and so deduplicates on merge.
    pub fn open_shared(doc: &mut AutoCommit) {
        let untouched = doc.get_all(&ROOT, V2_KEY).is_ok_and(|v| v.is_empty());
        if untouched && let Some(genesis) = blank_genesis() {
            let _ = tilo_automerge::ApplyIncoming::load_incoming(doc, genesis);
        }
        Self::seed(doc);
    }

    /// Materialize the bounded project projection used by the UI, search,
    /// history and agents. One document remains the canonical resource.
    #[must_use]
    pub fn read(doc: &AutoCommit) -> Project {
        Self::read_v2(doc).unwrap_or_else(Project::blank)
    }

    /// Set the source image path and the project name. Other adjustments stay.
    pub fn set_source(doc: &mut AutoCommit, path: &str, name: &str) {
        Self::seed(doc);
        let root = Self::root_map(doc);
        let metadata = Self::map(doc, &root, METADATA);
        let name = if name.is_empty() {
            "Untitled image"
        } else {
            name
        };
        put_if_changed(doc, &metadata, "name", name);
        let operations = Self::map(doc, &root, OPERATIONS);
        Self::write_operation(
            doc,
            &operations,
            "source",
            &Operation::Source {
                path: path.to_string(),
            },
        );
    }

    /// Compatibility upsert for local UI code. Missing rows are never
    /// interpreted as deletes, so a stale frame cannot remove remote nodes.
    /// Resource actions use the narrower mutation methods below.
    pub fn write(doc: &mut AutoCommit, project: &Project) {
        if Self::v2(doc).is_none() {
            Self::write_fresh(doc, project);
            return;
        }
        let root = Self::root_map(doc);
        let metadata = Self::map(doc, &root, METADATA);
        put_if_changed(doc, &metadata, "name", project.name.as_str());
        put_if_changed(doc, &metadata, "output", project.output.as_str());
        let tombstones = Self::map(doc, &root, TOMBSTONES);
        let hidden = Self::map(doc, &root, HISTORY_HIDDEN);
        let nodes = Self::map(doc, &root, NODES);
        let operations = Self::map(doc, &root, OPERATIONS);
        let order = Self::list(doc, &root, ORDER);
        for (id, node) in &project.graph.nodes {
            if Self::is_tombstoned(doc, &tombstones, id) {
                continue;
            }
            put_if_changed(doc, &hidden, id, ScalarValue::Boolean(false));
            Self::write_node(doc, &nodes, id, node);
            if let Some(operation) = project.operations.get(id) {
                Self::write_operation(doc, &operations, id, operation);
            }
            if id != "source" && id != "output" {
                Self::append_order_once(doc, &order, id);
            }
        }
    }

    /// Add an adjustment to the shared stack. `id` is permanent and explicit,
    /// so retries are no-ops and a deleted id cannot be silently repurposed.
    pub fn add_adjustment(
        doc: &mut AutoCommit,
        id: &str,
        title: &str,
        operation: &Operation,
    ) -> bool {
        if matches!(operation, Operation::Source { .. } | Operation::Output) || id.is_empty() {
            return false;
        }
        Self::seed(doc);
        let root = Self::root_map(doc);
        let tombstones = Self::map(doc, &root, TOMBSTONES);
        if Self::is_tombstoned(doc, &tombstones, id) {
            return false;
        }
        let nodes = Self::map(doc, &root, NODES);
        let operations = Self::map(doc, &root, OPERATIONS);
        if doc.get(&nodes, id).ok().flatten().is_some() {
            return Self::operation_matches(doc, &operations, id, operation);
        }
        let order = Self::list(doc, &root, ORDER);
        let x = 40.0 + f64::from(u32::try_from(doc.length(&order)).unwrap_or(u32::MAX)) * 200.0;
        let node = GraphNode {
            id: id.into(),
            title: title.into(),
            position: GraphPoint { x, y: 120.0 },
            inputs: vec![image_port()],
            outputs: vec![image_port()],
            enabled: true,
        };
        Self::write_node(doc, &nodes, id, &node);
        Self::write_operation(doc, &operations, id, operation);
        Self::append_order_once(doc, &order, id);
        true
    }

    /// Delete wins over concurrent edits. The tombstone is intentionally
    /// retained: keeping it is what prevents a delayed edit from resurrecting
    /// a graph node on another participant.
    pub fn remove_adjustment(doc: &mut AutoCommit, id: &str) -> bool {
        if id == "source" || id == "output" || id.is_empty() {
            return false;
        }
        Self::seed(doc);
        let root = Self::root_map(doc);
        let nodes = Self::map(doc, &root, NODES);
        if doc.get(&nodes, id).ok().flatten().is_none() {
            return false;
        }
        let tombstones = Self::map(doc, &root, TOMBSTONES);
        if Self::is_tombstoned(doc, &tombstones, id) {
            return true;
        }
        let _ = doc.put(&tombstones, id, ScalarValue::Boolean(true));
        true
    }

    /// Hide an adjustment through the shared, reversible history channel.
    ///
    /// The visible editor's remove affordance promises undo, so it must not
    /// create the permanent tombstone used for delete-wins synchronization.
    /// Hiding is still a document edit seen by collaborators; the row and its
    /// stable resource identity remain available for a history restore.
    pub fn hide_adjustment(doc: &mut AutoCommit, id: &str) -> bool {
        if id == "source" || id == "output" || id.is_empty() {
            return false;
        }
        Self::seed(doc);
        let root = Self::root_map(doc);
        let tombstones = Self::map(doc, &root, TOMBSTONES);
        if Self::is_tombstoned(doc, &tombstones, id) {
            return false;
        }
        let nodes = Self::map(doc, &root, NODES);
        if doc.get(&nodes, id).ok().flatten().is_none() {
            return false;
        }
        let hidden = Self::map(doc, &root, HISTORY_HIDDEN);
        if Self::is_tombstoned(doc, &hidden, id) {
            return true;
        }
        let _ = doc.put(&hidden, id, ScalarValue::Boolean(true));
        true
    }

    /// Change one declared adjustment field without rebuilding any other
    /// node.  These are the same leaf fields the v2 reader materializes, so
    /// independent agent edits merge at Automerge's natural granularity.
    pub fn set_adjustment_number(doc: &mut AutoCommit, id: &str, field: &str, value: f64) -> bool {
        Self::seed(doc);
        if !value.is_finite() {
            return false;
        }
        let root = Self::root_map(doc);
        let tombstones = Self::map(doc, &root, TOMBSTONES);
        if Self::is_tombstoned(doc, &tombstones, id) {
            return false;
        }
        let operations = Self::map(doc, &root, OPERATIONS);
        let Some(row) = get_map(doc, &operations, id) else {
            return false;
        };
        let Some(kind) = read_str(doc, &row, "kind") else {
            return false;
        };
        let valid = match (kind.as_str(), field) {
            ("exposure", "stops") => (-8.0..=8.0).contains(&value),
            ("contrast" | "saturation", "amount") => (-2.0..=3.0).contains(&value),
            ("crop", "x" | "y" | "width" | "height") => (0.0..=1.0).contains(&value),
            ("rotate", "quarter_turns") => (0.0..=3.0).contains(&value) && value.fract() == 0.0,
            _ => false,
        };
        if !valid {
            return false;
        }
        if kind == "rotate" {
            put_if_changed(doc, &row, field, value as u64);
        } else {
            put_if_changed(doc, &row, field, value);
        }
        true
    }

    /// Graph position is an independently editable shared layout property;
    /// viewport pan/zoom and selection deliberately remain per-person state.
    pub fn set_adjustment_position(doc: &mut AutoCommit, id: &str, x: f64, y: f64) -> bool {
        Self::seed(doc);
        if !x.is_finite()
            || !y.is_finite()
            || !(-10_000.0..=10_000.0).contains(&x)
            || !(-10_000.0..=10_000.0).contains(&y)
        {
            return false;
        }
        let root = Self::root_map(doc);
        let tombstones = Self::map(doc, &root, TOMBSTONES);
        if Self::is_tombstoned(doc, &tombstones, id) {
            return false;
        }
        let nodes = Self::map(doc, &root, NODES);
        let Some(row) = get_map(doc, &nodes, id) else {
            return false;
        };
        put_if_changed(doc, &row, "x", x);
        put_if_changed(doc, &row, "y", y);
        true
    }

    /// Apply local history as a reversible visibility marker. Intentional
    /// shared deletion still uses `tombstones` and therefore wins forever;
    /// undoing a local insertion can hide and later reveal that same stable
    /// node without inventing a replacement ID.
    pub fn restore_history_snapshot(doc: &mut AutoCommit, project: &Project) {
        Self::seed(doc);
        let root = Self::root_map(doc);
        let nodes = Self::map(doc, &root, NODES);
        let hidden = Self::map(doc, &root, HISTORY_HIDDEN);
        let node_ids = doc.keys(&nodes).collect::<Vec<_>>();
        for id in node_ids {
            if id != "source" && id != "output" && !project.graph.nodes.contains_key(&id) {
                let _ = doc.put(&hidden, id, ScalarValue::Boolean(true));
            }
        }
        for id in project.graph.nodes.keys() {
            let _ = doc.put(&hidden, id, ScalarValue::Boolean(false));
        }
    }

    /// Persist an expert reconnection only at its target input. Different
    /// target inputs merge independently; the same input has one producer,
    /// so Automerge's deterministic scalar winner is the conflict rule.
    pub fn sync_connections(doc: &mut AutoCommit, project: &Project) {
        Self::seed(doc);
        let root = Self::root_map(doc);
        let connections = Self::map(doc, &root, CONNECTIONS);
        let order_object = Self::list(doc, &root, ORDER);
        let order = Self::order_ids(doc, &order_object);
        let baseline = Self::baseline_edges(project, &order);
        for edge in &project.graph.edges {
            let key = edge_key(&edge.to_node, &edge.to_port);
            if baseline.iter().any(|candidate| candidate == edge) {
                let _ = doc.delete(&connections, key.as_str());
            } else if let Ok(encoded) = serde_json::to_string(edge) {
                put_if_changed(doc, &connections, key.as_str(), encoded.as_str());
            }
        }
    }

    /// Persist one advanced-graph input connection. Connections are addressed
    /// by their target port, so edits to different inputs merge independently
    /// and an ordinary linear edge stays implicit instead of duplicating the
    /// baseline graph in the document.
    pub fn set_connection(doc: &mut AutoCommit, edge: &Edge) -> bool {
        Self::seed(doc);
        let project = Self::read(doc);
        if !project.graph.nodes.contains_key(&edge.from_node)
            || !project.graph.nodes.contains_key(&edge.to_node)
        {
            return false;
        }
        let root = Self::root_map(doc);
        let connections = Self::map(doc, &root, CONNECTIONS);
        let order_object = Self::list(doc, &root, ORDER);
        let order = Self::order_ids(doc, &order_object);
        let baseline = Self::baseline_edges(&project, &order);
        let key = edge_key(&edge.to_node, &edge.to_port);
        if baseline.iter().any(|candidate| candidate == edge) {
            let _ = doc.delete(&connections, key.as_str());
        } else if let Ok(encoded) = serde_json::to_string(edge) {
            put_if_changed(doc, &connections, key.as_str(), encoded.as_str());
        } else {
            return false;
        }
        true
    }

    fn write_fresh(doc: &mut AutoCommit, project: &Project) {
        let root = Self::root_map(doc);
        let metadata = Self::map(doc, &root, METADATA);
        let _ = doc.put(&metadata, "name", project.name.as_str());
        let _ = doc.put(&metadata, "output", project.output.as_str());
        let nodes = Self::map(doc, &root, NODES);
        let operations = Self::map(doc, &root, OPERATIONS);
        let hidden = Self::map(doc, &root, HISTORY_HIDDEN);
        for (id, node) in &project.graph.nodes {
            Self::write_node(doc, &nodes, id, node);
            put_if_changed(doc, &hidden, id, ScalarValue::Boolean(false));
            if let Some(operation) = project.operations.get(id) {
                Self::write_operation(doc, &operations, id, operation);
            }
        }
        let order = Self::list(doc, &root, ORDER);
        for id in Self::project_order(project) {
            Self::append_order_once(doc, &order, &id);
        }
        let _ = Self::map(doc, &root, TOMBSTONES);
        Self::sync_connections(doc, project);
    }

    fn read_v2(doc: &AutoCommit) -> Option<Project> {
        let root = Self::v2(doc)?;
        let metadata = get_map(doc, &root, METADATA)?;
        let name = read_str(doc, &metadata, "name").unwrap_or_else(|| "Untitled image".into());
        let output = read_str(doc, &metadata, "output").unwrap_or_else(|| "output".into());
        let nodes = get_map(doc, &root, NODES)?;
        let operations = get_map(doc, &root, OPERATIONS)?;
        let tombstones = get_map(doc, &root, TOMBSTONES);
        let hidden = get_map(doc, &root, HISTORY_HIDDEN);
        let mut graph = Graph::default();
        let mut op_map = std::collections::BTreeMap::new();
        for id in doc.keys(&nodes) {
            if tombstones
                .as_ref()
                .is_some_and(|map| Self::is_tombstoned(doc, map, &id))
            {
                continue;
            }
            if hidden
                .as_ref()
                .is_some_and(|map| Self::is_tombstoned(doc, map, &id))
            {
                continue;
            }
            let Some(node) = Self::read_node(doc, &nodes, &id) else {
                continue;
            };
            let Some(operation) = Self::read_operation(doc, &operations, &id) else {
                continue;
            };
            graph.insert(node);
            op_map.insert(id, operation);
        }
        if !graph.nodes.contains_key("source") || !graph.nodes.contains_key(&output) {
            return Some(Project::blank());
        }
        let order = get_list(doc, &root, ORDER)
            .map(|list| Self::order_ids(doc, &list))
            .unwrap_or_default();
        for edge in Self::baseline_edges_for_graph(&graph, &output, &order) {
            let _ = graph.connect(edge);
        }
        if let Some(connections) = get_map(doc, &root, CONNECTIONS) {
            for key in doc.keys(&connections) {
                let Some(encoded) = read_str(doc, &connections, &key) else {
                    continue;
                };
                let Ok(edge) = serde_json::from_str::<Edge>(&encoded) else {
                    continue;
                };
                if graph.nodes.contains_key(&edge.from_node)
                    && graph.nodes.contains_key(&edge.to_node)
                {
                    let _ = graph.reconnect_input(edge);
                }
            }
        }
        Some(Project {
            name,
            graph,
            operations: op_map,
            output,
        })
    }

    fn v2(doc: &AutoCommit) -> Option<ObjId> {
        get_map(doc, &ROOT, V2_KEY)
    }
    fn root_map(doc: &mut AutoCommit) -> ObjId {
        map_or_create(doc, &ROOT, V2_KEY)
    }
    fn map(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> ObjId {
        map_or_create(doc, parent, key)
    }
    fn list(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> ObjId {
        list_or_create(doc, parent, key)
    }
    fn is_tombstoned(doc: &AutoCommit, tombstones: &ObjId, id: &str) -> bool {
        matches!(doc.get(tombstones, id), Ok(Some((Value::Scalar(value), _))) if matches!(value.as_ref(), ScalarValue::Boolean(true)))
    }
    fn write_node(doc: &mut AutoCommit, nodes: &ObjId, id: &str, node: &GraphNode) {
        let row = map_or_create(doc, nodes, id);
        put_if_changed(doc, &row, "title", node.title.as_str());
        put_if_changed(doc, &row, "x", node.position.x);
        put_if_changed(doc, &row, "y", node.position.y);
        put_if_changed(doc, &row, "enabled", ScalarValue::Boolean(node.enabled));
        if let Ok(inputs) = serde_json::to_string(&node.inputs) {
            put_if_changed(doc, &row, "inputs", inputs.as_str());
        }
        if let Ok(outputs) = serde_json::to_string(&node.outputs) {
            put_if_changed(doc, &row, "outputs", outputs.as_str());
        }
    }
    fn read_node(doc: &AutoCommit, nodes: &ObjId, id: &str) -> Option<GraphNode> {
        let row = get_map(doc, nodes, id)?;
        Some(GraphNode {
            id: id.into(),
            title: read_str(doc, &row, "title").unwrap_or_else(|| id.into()),
            position: GraphPoint {
                x: read_number(doc, &row, "x").unwrap_or(40.0),
                y: read_number(doc, &row, "y").unwrap_or(120.0),
            },
            inputs: read_str(doc, &row, "inputs")
                .and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_else(|| {
                    if id == "source" {
                        vec![]
                    } else {
                        vec![image_port()]
                    }
                }),
            outputs: read_str(doc, &row, "outputs")
                .and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_else(|| vec![image_port()]),
            enabled: read_bool(doc, &row, "enabled").unwrap_or(true),
        })
    }
    fn write_operation(doc: &mut AutoCommit, operations: &ObjId, id: &str, operation: &Operation) {
        let row = map_or_create(doc, operations, id);
        match operation {
            Operation::Source { path } => {
                put_if_changed(doc, &row, "kind", "source");
                put_if_changed(doc, &row, "path", path.as_str());
            }
            Operation::Exposure { stops } => {
                put_if_changed(doc, &row, "kind", "exposure");
                put_if_changed(doc, &row, "stops", f64::from(*stops));
            }
            Operation::Contrast { amount } => {
                put_if_changed(doc, &row, "kind", "contrast");
                put_if_changed(doc, &row, "amount", f64::from(*amount));
            }
            Operation::Saturation { amount } => {
                put_if_changed(doc, &row, "kind", "saturation");
                put_if_changed(doc, &row, "amount", f64::from(*amount));
            }
            Operation::Grayscale => put_if_changed(doc, &row, "kind", "grayscale"),
            Operation::Rotate { quarter_turns } => {
                put_if_changed(doc, &row, "kind", "rotate");
                put_if_changed(doc, &row, "quarter_turns", u64::from(*quarter_turns));
            }
            Operation::Crop {
                x,
                y,
                width,
                height,
            } => {
                put_if_changed(doc, &row, "kind", "crop");
                put_if_changed(doc, &row, "x", f64::from(*x));
                put_if_changed(doc, &row, "y", f64::from(*y));
                put_if_changed(doc, &row, "width", f64::from(*width));
                put_if_changed(doc, &row, "height", f64::from(*height));
            }
            Operation::Output => put_if_changed(doc, &row, "kind", "output"),
        }
    }
    fn read_operation(doc: &AutoCommit, operations: &ObjId, id: &str) -> Option<Operation> {
        let row = get_map(doc, operations, id)?;
        let kind = read_str(doc, &row, "kind")?;
        let float = |key| read_number(doc, &row, key).unwrap_or_default() as f32;
        match kind.as_str() {
            "source" => Some(Operation::Source {
                path: read_str(doc, &row, "path").unwrap_or_default(),
            }),
            "exposure" => Some(Operation::Exposure {
                stops: float("stops"),
            }),
            "contrast" => Some(Operation::Contrast {
                amount: float("amount"),
            }),
            "saturation" => Some(Operation::Saturation {
                amount: float("amount"),
            }),
            "grayscale" => Some(Operation::Grayscale),
            "rotate" => Some(Operation::Rotate {
                quarter_turns: read_u64(doc, &row, "quarter_turns").unwrap_or_default() as u8,
            }),
            "crop" => Some(Operation::Crop {
                x: float("x"),
                y: float("y"),
                width: float("width"),
                height: float("height"),
            }),
            "output" => Some(Operation::Output),
            _ => None,
        }
    }
    fn operation_matches(
        doc: &AutoCommit,
        operations: &ObjId,
        id: &str,
        desired: &Operation,
    ) -> bool {
        Self::read_operation(doc, operations, id).as_ref() == Some(desired)
    }
    fn append_order_once(doc: &mut AutoCommit, order: &ObjId, id: &str) {
        if Self::order_ids(doc, order).iter().any(|known| known == id) {
            return;
        }
        let _ = doc.insert(order, doc.length(order), id);
    }
    fn order_ids(doc: &AutoCommit, order: &ObjId) -> Vec<String> {
        (0..doc.length(order))
            .filter_map(|index| read_str(doc, order, index))
            .filter(|id| id != "source" && id != "output")
            .collect()
    }
    fn project_order(project: &Project) -> Vec<String> {
        project
            .graph
            .evaluation_order()
            .unwrap_or_default()
            .into_iter()
            .filter(|id| id != "source" && id != &project.output)
            .collect()
    }
    fn baseline_edges(project: &Project, order: &[String]) -> Vec<Edge> {
        Self::baseline_edges_for_graph(&project.graph, &project.output, order)
    }
    fn baseline_edges_for_graph(graph: &Graph, output: &str, order: &[String]) -> Vec<Edge> {
        let mut chain = vec!["source".to_owned()];
        chain.extend(
            order
                .iter()
                .filter(|id| graph.nodes.contains_key(*id))
                .cloned(),
        );
        chain.push(output.into());
        chain
            .windows(2)
            .filter_map(|pair| {
                let from_port = graph.nodes.get(&pair[0])?.outputs.first()?.id.clone();
                let to_port = graph.nodes.get(&pair[1])?.inputs.first()?.id.clone();
                Some(Edge {
                    from_node: pair[0].clone(),
                    from_port,
                    to_node: pair[1].clone(),
                    to_port,
                })
            })
            .collect()
    }
}

fn image_port() -> Port {
    Port {
        id: "image".into(),
        data_type: "image/rgba8".into(),
        label: "Image".into(),
    }
}
fn edge_key(node: &str, port: &str) -> String {
    format!("{node}\0{port}")
}
fn get_map(doc: &AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
}
fn get_list(doc: &AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
        _ => None,
    }
}
#[allow(
    clippy::expect_used,
    reason = "the parent is always ROOT or a map created by ProjectDoc in this call chain"
)]
fn map_or_create(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> ObjId {
    get_map(doc, parent, key).unwrap_or_else(|| {
        doc.put_object(parent, key, ObjType::Map)
            .expect("image project map lives under a map")
    })
}
#[allow(
    clippy::expect_used,
    reason = "the parent is always ROOT or a map created by ProjectDoc in this call chain"
)]
fn list_or_create(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> ObjId {
    get_list(doc, parent, key).unwrap_or_else(|| {
        doc.put_object(parent, key, ObjType::List)
            .expect("image project list lives under a map")
    })
}
fn put_if_changed<V: Into<ScalarValue>>(doc: &mut AutoCommit, obj: &ObjId, key: &str, value: V) {
    let value = value.into();
    let same = matches!(doc.get(obj, key), Ok(Some((Value::Scalar(current), _))) if current.as_ref() == &value);
    if !same {
        let _ = doc.put(obj, key, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_automerge::ApplyIncoming;
    fn merged(left: &mut AutoCommit, right: &mut AutoCommit) {
        let mut copy = AutoCommit::load(&right.save()).unwrap();
        left.merge_incoming(&mut copy).unwrap();
    }
    /// Two replicas open the same empty record and each add an adjustment
    /// before syncing: both must survive the merge.
    #[test]
    fn concurrent_first_opens_on_two_replicas_keep_both_adjustments() {
        for _ in 0..8 {
            let mut base = AutoCommit::new();
            base.commit();
            let mut a = base.fork();
            let mut b = base.fork();
            ProjectDoc::open_shared(&mut a);
            ProjectDoc::open_shared(&mut b);
            let exposure = Operation::Exposure { stops: 0.5 };
            assert!(ProjectDoc::add_adjustment(
                &mut a,
                "exposure-a",
                "Exposure",
                &exposure
            ));
            assert!(ProjectDoc::add_adjustment(
                &mut b,
                "exposure-b",
                "Exposure",
                &exposure
            ));
            a.merge_incoming(&mut b).unwrap();
            let project = ProjectDoc::read(&a);
            assert!(
                project.graph.nodes.contains_key("exposure-a")
                    && project.graph.nodes.contains_key("exposure-b"),
                "both replicas' adjustments survive; got {:?}",
                project.graph.nodes.keys().collect::<Vec<_>>()
            );
        }
    }

    /// Storage shape: repeated snapshot writes stay bounded.
    #[test]
    fn project_doc_storage_stays_bounded() {
        repetitive_snapshot_writes_do_not_materialize_unbounded_graphs();
    }
    /// Concurrent participants converge per adjustment: interleaved adds, independent
    /// metadata/parameter edits, and delete-wins without resurrection.
    #[test]
    fn concurrent_project_edits_converge_per_node() {
        two_participants_keep_interleaved_adjustments();
        independent_metadata_and_node_parameters_converge_without_graph_replacement();
        delete_wins_over_interleaved_parameter_edit_without_resurrection();
    }

    fn two_participants_keep_interleaved_adjustments() {
        let mut a = AutoCommit::new();
        ProjectDoc::seed(&mut a);
        let mut b = AutoCommit::load(&a.save()).unwrap();
        assert!(ProjectDoc::add_adjustment(
            &mut a,
            "exposure-a",
            "Exposure",
            &Operation::Exposure { stops: 0.5 }
        ));
        assert!(ProjectDoc::add_adjustment(
            &mut b,
            "contrast-b",
            "Contrast",
            &Operation::Contrast { amount: 0.2 }
        ));
        merged(&mut a, &mut b);
        merged(&mut b, &mut a);
        let pa = ProjectDoc::read(&a);
        let pb = ProjectDoc::read(&b);
        assert_eq!(pa, pb);
        assert!(pa.operations.contains_key("exposure-a"));
        assert!(pa.operations.contains_key("contrast-b"));
        assert_eq!(pa.graph.evaluation_order().unwrap().len(), 4);
    }
    fn independent_metadata_and_node_parameters_converge_without_graph_replacement() {
        let mut a = AutoCommit::new();
        ProjectDoc::seed(&mut a);
        assert!(ProjectDoc::add_adjustment(
            &mut a,
            "exposure-a",
            "Exposure",
            &Operation::Exposure { stops: 0.5 }
        ));
        assert!(ProjectDoc::add_adjustment(
            &mut a,
            "crop-b",
            "Crop",
            &Operation::Crop {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0
            }
        ));
        let mut b = AutoCommit::load(&a.save()).unwrap();
        let mut pa = ProjectDoc::read(&a);
        pa.name = "Alice’s grade".into();
        *pa.operations.get_mut("exposure-a").unwrap() = Operation::Exposure { stops: 1.25 };
        ProjectDoc::write(&mut a, &pa);
        let mut pb = ProjectDoc::read(&b);
        *pb.operations.get_mut("crop-b").unwrap() = Operation::Crop {
            x: 0.1,
            y: 0.2,
            width: 0.7,
            height: 0.6,
        };
        ProjectDoc::write(&mut b, &pb);
        merged(&mut a, &mut b);
        merged(&mut b, &mut a);
        let project = ProjectDoc::read(&a);
        assert_eq!(project, ProjectDoc::read(&b));
        assert_eq!(project.name, "Alice’s grade");
        assert!(
            matches!(project.operations["exposure-a"], Operation::Exposure { stops } if (stops - 1.25).abs() < f32::EPSILON)
        );
        assert!(
            matches!(project.operations["crop-b"], Operation::Crop { x, y, width, height } if (x, y, width, height) == (0.1, 0.2, 0.7, 0.6))
        );
    }
    fn delete_wins_over_interleaved_parameter_edit_without_resurrection() {
        let mut a = AutoCommit::new();
        ProjectDoc::seed(&mut a);
        assert!(ProjectDoc::add_adjustment(
            &mut a,
            "tone",
            "Exposure",
            &Operation::Exposure { stops: 0.5 }
        ));
        let mut b = AutoCommit::load(&a.save()).unwrap();
        assert!(ProjectDoc::remove_adjustment(&mut a, "tone"));
        let mut edited = ProjectDoc::read(&b);
        *edited.operations.get_mut("tone").unwrap() = Operation::Exposure { stops: 2.0 };
        ProjectDoc::write(&mut b, &edited);
        merged(&mut a, &mut b);
        merged(&mut b, &mut a);
        assert!(!ProjectDoc::read(&a).operations.contains_key("tone"));
        assert!(!ProjectDoc::add_adjustment(
            &mut a,
            "tone",
            "Exposure",
            &Operation::Exposure { stops: 0.5 }
        ));
    }
    fn repetitive_snapshot_writes_do_not_materialize_unbounded_graphs() {
        let mut doc = AutoCommit::new();
        ProjectDoc::seed(&mut doc);
        let project = ProjectDoc::read(&doc);
        let before = doc.save().len();
        for _ in 0..32 {
            ProjectDoc::write(&mut doc, &project);
        }
        assert_eq!(before, doc.save().len());
        assert_eq!(ProjectDoc::read(&doc).graph.nodes.len(), 2);
    }
}
