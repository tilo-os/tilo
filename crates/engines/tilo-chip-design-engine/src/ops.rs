//! What the Chip Design app asks its engine, read from the JSON it wrote.
//!
//! Two asks: take in one imported engineering document, or resolve the structural operator
//! design the app holds. Both answer JSON the app reads into its own records.

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::Value;

use crate::graph::{ConnectionTarget, connection_targets};
use crate::model::{MAX_REPORT_BYTES, StorageReport, report_id};
use crate::operator::{Connection, OperatorNode, OperatorState};
use crate::verification::VerificationRecord;

/// One imported document, as the app hands it over.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportAsk {
    json: String,
}

/// The structural design the app holds, as it asks about it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphAsk {
    #[serde(default)]
    nodes: Vec<NodeSpec>,
    #[serde(default)]
    connections: Vec<EdgeSpec>,
    /// The output a person chose, whose compatible inputs the answer names.
    #[serde(default)]
    from: Option<OutputSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputSpec {
    node: String,
    port: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeSpec {
    id: String,
    kind: crate::operator::OperatorKind,
    shape: Vec<u16>,
    #[serde(default)]
    output_width: u32,
    #[serde(default)]
    x: u16,
    #[serde(default)]
    y: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EdgeSpec {
    from_node: String,
    from_port: String,
    to_node: String,
    to_port: String,
}

fn refused(error: &serde_json::Error) -> String {
    format!("the ask is not one this engine reads: {error}")
}

/// Take in one imported document: a storage report or a hardware verification receipt.
///
/// The answer names what it is, the identity of the imported bytes, the document to keep, and
/// the document as the app reads it.
///
/// # Errors
///
/// The document is neither, or it fails the bounded checks its own evidence class declares.
pub fn import(input: &str) -> Result<String, String> {
    let ask: ImportAsk = serde_json::from_str(input).map_err(|error| refused(&error))?;
    let json = &ask.json;
    if json.len() > MAX_REPORT_BYTES {
        return Err("report exceeds the 1 MiB import limit".into());
    }
    let evidence: serde_json::Value =
        serde_json::from_str(json).map_err(|error| error.to_string())?;
    let storage = evidence.get("evidence").and_then(serde_json::Value::as_str)
        == Some("analytical_storage_bound");
    if storage {
        let report = StorageReport::parse(json)?;
        Ok(serde_json::json!({
            "kind": "report",
            "id": report_id(json),
            "stored": json,
            "facts": crate::facts::report_facts(&report),
        })
        .to_string())
    } else {
        let (id, stored) = VerificationRecord::import(json)?;
        let record = VerificationRecord::read(&stored)?;
        Ok(serde_json::json!({
            "kind": "verification",
            "id": id,
            "stored": stored,
            "facts": crate::facts::receipt_facts(&record),
        })
        .to_string())
    }
}

/// The graph the stored operators and connections make: every connection that still connects,
/// and the ones that conflict, with why.
///
/// # Errors
///
/// An operator is not one a structural design can hold.
pub fn graph(input: &str) -> Result<String, String> {
    let ask: GraphAsk = serde_json::from_str(input).map_err(|error| refused(&error))?;
    let mut nodes = Vec::with_capacity(ask.nodes.len());
    let mut seen = BTreeSet::new();
    for spec in &ask.nodes {
        if !seen.insert(spec.id.clone()) {
            return Err("two operators answer by one identity".into());
        }
        let node = OperatorNode {
            id: spec.id.clone(),
            kind: spec.kind,
            normalization_width: spec.shape.last().copied().unwrap_or(0),
            shape: spec.shape.clone(),
            output_width: (spec.output_width > 0).then_some(spec.output_width),
            x: spec.x,
            y: spec.y,
        };
        node.valid()?;
        nodes.push(node);
    }
    let connections: Vec<Connection> = ask
        .connections
        .iter()
        .map(|edge| Connection {
            from_node: edge.from_node.clone(),
            from_port: edge.from_port.clone(),
            to_node: edge.to_node.clone(),
            to_port: edge.to_port.clone(),
        })
        .collect();
    let state = OperatorState::of(&nodes, &connections);
    let mut ordered = nodes;
    ordered.sort_by(|a, b| a.id.cmp(&b.id));
    let nodes: Vec<serde_json::Value> = ordered
        .iter()
        .map(|node| {
            let visual = node.visual();
            serde_json::json!({
                "id": node.id,
                "kind": node.kind,
                "inputs": ports(visual.inputs),
                "outputs": ports(visual.outputs),
            })
        })
        .collect();
    let candidates: Vec<serde_json::Value> = state
        .candidates
        .iter()
        .map(|(id, edge)| edge_json(id, edge))
        .collect();
    let active: Vec<serde_json::Value> = state
        .active
        .iter()
        .map(|edge| edge_json(&Connection::from(edge.clone()).id(), edge))
        .collect();
    let rejected: Vec<serde_json::Value> = state
        .rejected
        .iter()
        .map(|rejected| {
            let mut value = edge_json(&rejected.id, &rejected.edge);
            if let Value::Object(fields) = &mut value {
                fields.insert("reason".into(), Value::String(rejected.reason.clone()));
            }
            value
        })
        .collect();
    let targets: Vec<serde_json::Value> = ask.from.map_or_else(Vec::new, |from| {
        let graph = state.graph();
        connection_targets(&graph, &from.node, &from.port)
            .into_iter()
            .map(|ConnectionTarget { node, port }| serde_json::json!({"node": node, "port": port}))
            .collect()
    });
    Ok(serde_json::json!({
        "nodes": nodes,
        "candidates": candidates,
        "active": active,
        "rejected": rejected,
        "targets": targets,
    })
    .to_string())
}

fn ports(ports: Vec<crate::graph::Port>) -> Vec<serde_json::Value> {
    ports
        .into_iter()
        .map(|port| {
            serde_json::json!({
                "id": port.id,
                "label": port.label,
                "data_type": port.data_type,
            })
        })
        .collect()
}

fn edge_json(id: &str, edge: &crate::graph::Edge) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "from_node": edge.from_node,
        "from_port": edge.from_port,
        "to_node": edge.to_node,
        "to_port": edge.to_port,
    })
}
