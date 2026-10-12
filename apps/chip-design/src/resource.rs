//! The chip project: imported engineering evidence and the structural
//! operator design, one model with its verbs.
//!
//! Reports and verification receipts are kept as the exact text they were
//! imported as, under the content identity of those bytes; only importing
//! and removing them changes them. Moving an operator and its matrix output
//! width are field edits of its node; adding, reshaping and removing nodes
//! and connecting their ports are verbs, checked against the graph as it
//! stands. What a view shows is the parsed [`ProjectState`].

use std::collections::BTreeMap;

use crate::view::ChipEffect;
use ui::graph_editor::Edge;
use ui::tilo_resource::{
    self, Atom, Derived, Id, Invoke, Keyed, Model, Refusal, Resource as _, SearchEntry, Set,
    resource,
};

use crate::model::{MAX_REPORTS, StorageReport, report_id};
use crate::operator_graph::{
    Connection, MAX_EDGES, MAX_NODES, OperatorKind, OperatorNode, OperatorState, reason,
};
use crate::verification::VerificationRecord;

/// The project as the views read it: every report and receipt parsed, and
/// the operator graph its stored connections resolve to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectState {
    pub reports: BTreeMap<String, StorageReport>,
    pub operator_graph: OperatorState,
    pub verifications: BTreeMap<String, VerificationRecord>,
}

impl ProjectState {
    /// What `project` holds, parsed. Text that no longer parses is left
    /// out: only the import verbs store it, after parsing it.
    pub fn of(project: &Project) -> Self {
        Self {
            reports: project
                .reports
                .iter()
                .filter_map(|(id, json)| Some((id.clone(), StorageReport::parse(&json.0).ok()?)))
                .collect(),
            operator_graph: OperatorState::of(project.nodes.iter(), project.connections.iter()),
            verifications: project
                .verifications
                .iter()
                .filter_map(|(id, json)| {
                    Some((id.clone(), VerificationRecord::read(&json.0).ok()?))
                })
                .collect(),
        }
    }
}

/// A chip design project.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = Project::valid)]
pub struct Project {
    /// Analytical storage reports, the exact imported JSON by its content
    /// identity.
    #[model(readonly)]
    pub reports: BTreeMap<String, Atom>,
    /// Hardware verification receipts, as stored on import, by the content
    /// identity of the imported bytes.
    #[model(readonly)]
    pub verifications: BTreeMap<String, Atom>,
    /// Operators of the structural design.
    pub nodes: Keyed<OperatorNode>,
    /// Connections between operator ports.
    #[model(readonly)]
    pub connections: Set<Connection>,
}

#[resource(
    kind = "chip_design.project",
    label = "Chip Design",
    children = Project::places,
    search = Project::search,
    read_content = Project::read_content
)]
impl Project {
    /// Import analytical report.
    ///
    /// Import reported storage evidence; does not authenticate the model or
    /// validate chip performance. The same report imported twice is kept
    /// once.
    #[action]
    pub fn import_report(
        &mut self,
        #[param("Complete analytical storage report JSON")] json: String,
    ) -> Result<(), Refusal> {
        StorageReport::parse(&json).map_err(Refusal::new)?;
        let id = report_id(&json);
        if self.reports.contains_key(&id) {
            return Ok(());
        }
        if self.reports.len() >= MAX_REPORTS {
            return Err(Refusal::new("project report limit reached"));
        }
        self.reports.insert(id, Atom(json));
        Ok(())
    }

    /// Remove report.
    #[action]
    pub fn remove_report(
        &mut self,
        #[param("Content identity of the report")] report: String,
    ) -> Result<(), Refusal> {
        self.reports
            .remove(&report)
            .map(drop)
            .ok_or_else(|| Refusal::new("report no longer exists"))
    }

    /// Import hardware verification receipt.
    ///
    /// Import reported engineering evidence; does not authenticate artifacts
    /// or establish model quality, silicon measurements, or
    /// manufacturability.
    #[action]
    pub fn import_verification(
        &mut self,
        #[param("Complete immutable hardware verification receipt JSON")] json: String,
    ) -> Result<(), Refusal> {
        let (id, stored) = VerificationRecord::import(&json).map_err(Refusal::new)?;
        if self.verifications.contains_key(&id) {
            return Ok(());
        }
        if self.verifications.len() >= MAX_REPORTS {
            return Err(Refusal::new("project verification limit reached"));
        }
        self.verifications.insert(id, Atom(stored));
        Ok(())
    }

    /// Remove verification receipt.
    #[action]
    pub fn remove_verification(
        &mut self,
        #[param("Shared receipt content identity")] report: String,
    ) -> Result<(), Refusal> {
        self.verifications
            .remove(&report)
            .map(drop)
            .ok_or_else(|| Refusal::new("verification no longer exists"))
    }

    /// Add operator.
    ///
    /// Edit a structural operator design; does not execute or qualify a
    /// netlist. Adding the same node twice keeps it once.
    #[action]
    pub fn add_node(
        &mut self,
        #[param("Stable node identity")] node: String,
        #[param("Operator kind")] kind: OperatorKind,
        #[param(
            "F32 tensor shape, up to four positive dimensions; its last axis is the normalization width"
        )]
        shape: Vec<u16>,
        #[param("Required for matrix weights and linear projection; structural output width")]
        output_width: Option<u32>,
        #[param("Horizontal position")] x: u16,
        #[param("Vertical position")] y: u16,
    ) -> Result<(), Refusal> {
        let node = OperatorNode {
            id: node,
            kind,
            normalization_width: shape.last().copied().unwrap_or(0),
            shape,
            output_width,
            x,
            y,
        };
        if let Some(existing) = self.node(&node.id) {
            return if *existing == node {
                Ok(())
            } else {
                Err(Refusal::new("node identity already exists"))
            };
        }
        if self.nodes.len() >= MAX_NODES {
            return Err(Refusal::new("operator node limit reached"));
        }
        self.nodes.push(node);
        self.nodes.settle();
        Ok(())
    }

    /// Set operator input shape.
    ///
    /// Its last axis sets the normalization width.
    #[action]
    pub fn set_shape(
        &mut self,
        #[param("Operator node identity")] node: String,
        #[param("Up to four positive input dimensions")] shape: Vec<u16>,
    ) -> Result<(), Refusal> {
        let target = self
            .nodes
            .find_mut(&Id::from_raw(node))
            .ok_or_else(|| Refusal::new("unknown node"))?;
        *target = target.with_shape(shape);
        Ok(())
    }

    /// Remove operator.
    ///
    /// Its connections go with it.
    #[action]
    pub fn remove_node(&mut self, #[param("Node identity")] node: String) -> Result<(), Refusal> {
        self.nodes
            .take(&Id::from_raw(node.clone()))
            .ok_or_else(|| Refusal::new("unknown node"))?;
        let attached: Vec<Connection> = self
            .connections
            .iter()
            .filter(|connection| connection.from_node == node || connection.to_node == node)
            .cloned()
            .collect();
        for connection in &attached {
            self.connections.remove(connection);
        }
        Ok(())
    }

    /// Connect operator ports.
    #[action]
    pub fn connect(
        &mut self,
        #[param("Source node")] from_node: String,
        #[param("Output port")] from_port: String,
        #[param("Destination node")] to_node: String,
        #[param("Input port")] to_port: String,
    ) -> Result<(), Refusal> {
        let connection = Connection {
            from_node,
            from_port,
            to_node,
            to_port,
        };
        let state = self.operator_graph();
        let edge = Edge::from(&connection);
        if state.active.contains(&edge) {
            return Ok(());
        }
        if self.connections.len() >= MAX_EDGES {
            return Err(Refusal::new("operator edge limit reached"));
        }
        state
            .graph()
            .connect(edge)
            .map_err(|error| Refusal::new(reason(&error)))?;
        self.connections.insert(connection);
        Ok(())
    }

    /// Disconnect input.
    #[action]
    pub fn disconnect(
        &mut self,
        #[param("Destination node")] node: String,
        #[param("Input port")] port: String,
    ) -> Result<(), Refusal> {
        let has_port = self
            .node(&node)
            .is_some_and(|found| found.visual().inputs.iter().any(|input| input.id == port));
        if !has_port {
            return Err(Refusal::new("unknown input port"));
        }
        let attached: Vec<Connection> = self
            .connections
            .iter()
            .filter(|connection| connection.to_node == node && connection.to_port == port)
            .cloned()
            .collect();
        for connection in &attached {
            self.connections.remove(connection);
        }
        Ok(())
    }
}

impl Project {
    fn valid(&self) -> Result<(), Refusal> {
        if self.nodes.len() > MAX_NODES {
            return Err(Refusal::new("operator node limit reached"));
        }
        if self.connections.len() > MAX_EDGES {
            return Err(Refusal::new("operator edge limit reached"));
        }
        Ok(())
    }

    /// The operator `id`.
    pub fn node(&self, id: &str) -> Option<&OperatorNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// The graph the stored nodes and connections resolve to.
    pub fn operator_graph(&self) -> OperatorState {
        OperatorState::of(self.nodes.iter(), self.connections.iter())
    }

    /// The operator graph, each report and each receipt with the evidence
    /// it holds, as places to inspect.
    fn places(&self) -> Vec<Derived<Self>> {
        let state = ProjectState::of(self);
        std::iter::once(
            Derived::new("Operator graph")
                .of_kind("operator_graph")
                .identified(GRAPH_PLACE)
                .with_effect(&ChipEffect::OperatorGraph),
        )
        .chain(state.reports.iter().map(|(id, report)| {
            Derived::new(report.model.clone())
                .of_kind("report")
                .identified(format!("report:{id}"))
                .with_effect(&ChipEffect::Inspect {
                    report: Some(id.clone()),
                })
                .with(Self::REMOVE_REPORT.bind(project::RemoveReport::paths().report, id.clone()))
        }))
        .chain(state.verifications.iter().map(|(id, record)| {
            Derived::new(record.receipt.evidence.clone())
                .of_kind("verification")
                .identified(format!("verification:{id}"))
                .with_effect(&ChipEffect::Inspect {
                    report: Some(id.clone()),
                })
                .with(
                    Self::REMOVE_VERIFICATION
                        .bind(project::RemoveVerification::paths().report, id.clone()),
                )
        }))
        .chain(
            state
                .verifications
                .iter()
                .flat_map(|(id, record)| flow_places(id, record)),
        )
        .collect()
    }

    fn search(&self) -> Vec<SearchEntry> {
        let state = ProjectState::of(self);
        state
            .reports
            .iter()
            .map(|(id, report)| {
                SearchEntry::new(report.model.clone(), report.revision.clone())
                    .anchored(report_resource_id(id))
            })
            .chain(state.verifications.iter().map(|(id, record)| {
                SearchEntry::new(
                    record.receipt.evidence_flow.as_ref().map_or_else(
                        || record.receipt.evidence.clone(),
                        |flow| flow.title.clone(),
                    ),
                    format!(
                        "{:?} {}",
                        record.receipt.status,
                        record.receipt.excludes.join(" ")
                    ),
                )
                .anchored(verification_resource_id(id))
            }))
            .chain(state.verifications.iter().flat_map(|(report, record)| {
                flow_items(record)
                    .into_iter()
                    .map(move |(kind, id, title)| {
                        SearchEntry::new(title, "reported_unverified")
                            .anchored(crate::evidence_flow_view::resource_id(report, kind, &id))
                    })
            }))
            .collect()
    }

    /// What an agent reads: the project's summary, or one report, receipt,
    /// evidence item or the operator graph, by the id it is published
    /// under.
    fn read_content(&self, item: Option<&Id>) -> Result<serde_json::Value, String> {
        let state = ProjectState::of(self);
        let Some(item) = item else {
            return Ok(serde_json::json!({
                "kind": Self::KIND,
                "operator_graph": {
                    "resource": crate::operator_graph::RESOURCE_ID,
                    "qualification": "structural_design_only",
                    "nodes": state.operator_graph.nodes.len(),
                    "active_edges": state.operator_graph.active.len(),
                    "rejected_candidates": state.operator_graph.rejected.len(),
                },
                "reports": state.reports.iter().map(|(id, report)| serde_json::json!({
                    "resource": report_resource_id(id), "model": report.model,
                    "revision": report.revision, "evidence": report.evidence,
                })).collect::<Vec<_>>(),
                "verifications": state.verifications.iter().map(|(id, record)| serde_json::json!({
                    "resource": verification_resource_id(id), "evidence": record.receipt.evidence,
                    "status": record.receipt.status,
                })).collect::<Vec<_>>(),
            }));
        };
        let place = item
            .as_str()
            .rsplit_once("/derived/")
            .map_or(item.as_str(), |(_, place)| place);
        if place == GRAPH_PLACE {
            let graph = &state.operator_graph;
            return Ok(serde_json::json!({
                "qualification": "structural_design_only",
                "graph": graph.graph(),
                "nodes": graph.nodes,
                "candidates": graph.candidates,
                "active": graph.active,
                "rejected": graph.rejected,
            }));
        }
        if let Some(anchor) = place.strip_prefix("flow:") {
            return crate::evidence_flow_view::item(&state, anchor)
                .ok_or_else(|| "evidence item no longer exists".into());
        }
        if let Some(id) = place.strip_prefix("verification:") {
            let record = state
                .verifications
                .get(id)
                .ok_or("verification no longer exists")?;
            return serde_json::to_value(record).map_err(|error| error.to_string());
        }
        let id = place
            .strip_prefix("report:")
            .ok_or("invalid report resource")?;
        let report = state.reports.get(id).ok_or("report no longer exists")?;
        serde_json::to_value(report).map_err(|error| error.to_string())
    }
}

/// The derived place the operator graph is published as.
const GRAPH_PLACE: &str = "operator-graph";

/// Import `json`: a storage report or a verification receipt, whichever it
/// is.
pub fn import(json: String) -> Result<Invoke<Project>, String> {
    let evidence: serde_json::Value = if json.len() <= crate::model::MAX_REPORT_BYTES {
        serde_json::from_str(&json).map_err(|error| error.to_string())?
    } else {
        return Err("report exceeds the 1 MiB import limit".into());
    };
    if evidence.get("evidence").and_then(serde_json::Value::as_str)
        == Some("analytical_storage_bound")
    {
        StorageReport::parse(&json)?;
        Ok(Project::IMPORT_REPORT.with(json))
    } else {
        VerificationRecord::import(&json)?;
        Ok(Project::IMPORT_VERIFICATION.with(json))
    }
}

/// The id a report is published under.
pub(crate) fn report_resource_id(id: &str) -> String {
    format!(
        "{}/derived/report:{id}",
        <Project as tilo_resource::Resource>::KIND
    )
}

/// The id a verification receipt is published under.
pub(crate) fn verification_resource_id(id: &str) -> String {
    format!(
        "{}/derived/verification:{id}",
        <Project as tilo_resource::Resource>::KIND
    )
}

/// One reported evidence item as a place to inspect: a stage, a macro
/// placement, a subflow reference or an artifact.
fn flow_place(report: &str, kind: &str, id: &str, title: &str) -> Derived<Project> {
    let anchor = crate::evidence_flow_view::anchor(report, kind, id);
    let place = Derived::new(title)
        .of_kind(format!("flow_{kind}"))
        .identified(format!("flow:{anchor}"))
        .with_effect(&ChipEffect::InspectEvidence {
            report: report.into(),
            item: format!("{kind}/{id}"),
        });
    if kind == "subflow" {
        place.with_effect(&ChipEffect::OpenEvidenceSubflow {
            report: report.into(),
            id: id.into(),
        })
    } else {
        place
    }
}

/// Every reported evidence item of `record`: its kind, id and title.
fn flow_items(record: &VerificationRecord) -> Vec<(&'static str, String, String)> {
    let Some(flow) = &record.receipt.evidence_flow else {
        return Vec::new();
    };
    flow.stages
        .iter()
        .flat_map(|stage| {
            std::iter::once(("stage", stage.id.clone(), stage.title.clone())).chain(
                stage.placement.iter().flat_map(|placement| {
                    placement.macros.iter().map(|instance| {
                        (
                            "placement",
                            format!("{}/{}", stage.id, instance.id),
                            instance.name.clone(),
                        )
                    })
                }),
            )
        })
        .chain(
            flow.subflows
                .iter()
                .map(|link| ("subflow", link.id.clone(), link.title.clone())),
        )
        .chain(
            record
                .receipt
                .artifact_sha256
                .keys()
                .map(|path| ("artifact", path.clone(), path.clone())),
        )
        .collect()
}

fn flow_places(report: &str, record: &VerificationRecord) -> Vec<Derived<Project>> {
    flow_items(record)
        .into_iter()
        .map(|(kind, id, title)| flow_place(report, kind, &id, &title))
        .collect()
}

impl From<OperatorNode> for project::AddNode {
    fn from(node: OperatorNode) -> Self {
        Self {
            node: node.id,
            kind: node.kind,
            shape: node.shape,
            output_width: node.output_width,
            x: node.x,
            y: node.y,
        }
    }
}
