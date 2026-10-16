//! Detached evidence dependencies, never an executable or authenticated netlist.
use super::{
    CheckStatus, ReceiptStatus, VerificationReceipt, digest, hashes, identity_name, text,
    unique_map,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceFlow {
    pub title: String,
    pub input_identity_sha256: String,
    pub stages: Vec<FlowStage>,
    pub edges: Vec<Dependency>,
    pub subflows: Vec<Subflow>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowStage {
    pub id: String,
    pub title: String,
    pub status: ReceiptStatus,
    pub original_receipt_artifact: String,
    pub producer_sha256: String,
    pub input_identity_sha256: String,
    #[serde(deserialize_with = "unique_map")]
    pub checks: BTreeMap<String, CheckStatus>,
    pub limitations: Vec<String>,
    pub observations: Option<Observation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<super::placement::Placement>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum Observation {
    CpuReference {
        comparison: Comparison,
        compared_words: u64,
        differing_words: u64,
        output_sha256: String,
    },
    RtlSimulation {
        outcome: SimulationOutcome,
        refused_inputs: u64,
        cycles: u64,
        accepted_inputs: u64,
        accepted_outputs: u64,
        output_sha256: String,
    },
    GenericSynthesis {
        logic_cells: u64,
        logical_memory_bits: u64,
    },
    LibraryPhysical {
        library: String,
        clock_constraint_ns: serde_json::Number,
        setup_slack_ns: serde_json::Number,
        hold_slack_ns: serde_json::Number,
        routed_signal_nets: u64,
        router_violations: u64,
        library_area_um2: serde_json::Number,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    Exact,
    Characterization,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimulationOutcome {
    Completed,
    ExpectedRefusal,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub from: String,
    pub to: String,
    pub kind: DependencyKind,
    pub artifact: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    Data,
    Build,
    Verification,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subflow {
    pub id: String,
    pub title: String,
    pub summary_artifact: String,
}

pub(super) fn validate(receipt: &VerificationReceipt) -> Result<(), String> {
    let flow = receipt
        .evidence_flow
        .as_ref()
        .ok_or("evidence flow missing")?;
    validate_envelope(receipt, flow)?;
    let mut ids = BTreeSet::new();
    for stage in &flow.stages {
        if !id(&stage.id)
            || !ids.insert(stage.id.clone())
            || !label(&stage.title)
            || !digest(&stage.producer_sha256)
            || !digest(&stage.input_identity_sha256)
            || !receipt
                .artifact_sha256
                .contains_key(&stage.original_receipt_artifact)
            || stage.limitations.is_empty()
            || stage.limitations.len() > 32
            || !stage.limitations.iter().all(|value| text(value))
        {
            return Err("invalid stage identity or provenance".into());
        }
        terminal(&stage.status, &stage.checks)?;
        if let Some(observation) = &stage.observations {
            observation.validate()?;
        }
        if let Some(placement) = &stage.placement {
            if !matches!(
                stage.observations,
                Some(Observation::LibraryPhysical { .. })
            ) {
                return Err("macro placement requires a reported physical stage".into());
            }
            placement.validate(receipt)?;
        }
    }
    for link in &flow.subflows {
        if !id(&link.id)
            || !ids.insert(link.id.clone())
            || !label(&link.title)
            || !receipt.artifact_sha256.contains_key(&link.summary_artifact)
        {
            return Err("invalid subflow reference".into());
        }
    }
    if receipt.status == ReceiptStatus::Passed
        && flow
            .stages
            .iter()
            .any(|stage| stage.status != ReceiptStatus::Passed)
    {
        return Err("passed flow contains incomplete stages".into());
    }
    dependencies(flow, &ids, &receipt.artifact_sha256)
}
fn validate_envelope(receipt: &VerificationReceipt, flow: &EvidenceFlow) -> Result<(), String> {
    let value = serde_json::to_value(receipt).map_err(|error| error.to_string())?;
    let allowed = [
        "schema_version",
        "evidence",
        "status",
        "checks",
        "declared_toolchain_image",
        "tools",
        "artifact_sha256",
        "excludes",
        "failure",
        "evidence_flow",
    ];
    if value
        .as_object()
        .is_none_or(|object| object.keys().any(|key| !allowed.contains(&key.as_str())))
    {
        return Err("unrelated evidence flow fields".into());
    }
    if receipt.schema_version != 1
        || !hashes(&receipt.artifact_sha256)
        || receipt.artifact_sha256.is_empty()
        || !digest(&flow.input_identity_sha256)
        || !label(&flow.title)
        || flow.stages.len().saturating_add(flow.subflows.len()) > 32
        || flow.stages.is_empty() && flow.subflows.is_empty()
        || flow.edges.len() > 64
    {
        return Err("invalid bounded evidence flow".into());
    }
    if receipt.excludes.len() > 32
        || !receipt.excludes.iter().all(|value| text(value))
        || ![
            "artifact_authentication",
            "silicon_measurement",
            "manufacturing",
            "model_quality",
        ]
        .iter()
        .all(|name| receipt.excludes.iter().any(|value| value == name))
    {
        return Err("evidence flow custody exclusions missing".into());
    }
    if receipt.tools.len() > 32
        || receipt.tools.iter().any(|(name, tool)| {
            !identity_name(name) || !text(&tool.version) || !digest(&tool.executable_sha256)
        })
    {
        return Err("invalid reported tool identity".into());
    }
    terminal(&receipt.status, &receipt.checks)?;
    Ok(())
}
fn label(value: &str) -> bool {
    text(value) && value.len() <= 160
}
fn id(value: &str) -> bool {
    identity_name(value) && value.len() <= 64 && !value.contains('/')
}
fn terminal(status: &ReceiptStatus, checks: &BTreeMap<String, CheckStatus>) -> Result<(), String> {
    if checks.len() > 32
        || checks.keys().any(|name| !identity_name(name))
        || *status == ReceiptStatus::Passed
            && (checks.is_empty() || checks.values().any(|check| *check != CheckStatus::Passed))
        || *status == ReceiptStatus::Failed
            && !checks.values().any(|check| *check == CheckStatus::Failed)
    {
        return Err("stage status contradicts observed checks".into());
    }
    Ok(())
}
impl Observation {
    pub fn key(&self) -> &'static str {
        match self {
            Self::CpuReference { .. } => "cpu_reference",
            Self::RtlSimulation { .. } => "rtl_simulation",
            Self::GenericSynthesis { .. } => "generic_synthesis",
            Self::LibraryPhysical { .. } => "library_physical",
        }
    }
    fn validate(&self) -> Result<(), String> {
        let valid = match self {
            Self::CpuReference {
                comparison,
                compared_words,
                differing_words,
                output_sha256,
            } => {
                *compared_words > 0
                    && differing_words <= compared_words
                    && (*comparison != Comparison::Exact || *differing_words == 0)
                    && digest(output_sha256)
            }
            Self::RtlSimulation {
                outcome,
                refused_inputs,
                cycles,
                accepted_inputs,
                accepted_outputs,
                output_sha256,
            } => {
                *cycles > 0
                    && match outcome {
                        SimulationOutcome::Completed => {
                            *accepted_inputs > 0 && *accepted_outputs > 0 && *refused_inputs == 0
                        }
                        SimulationOutcome::ExpectedRefusal => {
                            *accepted_outputs == 0 && *refused_inputs > 0
                        }
                    }
                    && digest(output_sha256)
            }
            Self::GenericSynthesis { logic_cells, .. } => *logic_cells > 0,
            Self::LibraryPhysical {
                library,
                clock_constraint_ns,
                setup_slack_ns,
                hold_slack_ns,
                routed_signal_nets,
                library_area_um2,
                ..
            } => {
                label(library)
                    && positive(clock_constraint_ns)
                    && positive(library_area_um2)
                    && finite(setup_slack_ns)
                    && finite(hold_slack_ns)
                    && *routed_signal_nets > 0
            }
        };
        if !valid {
            return Err("invalid scoped observations".into());
        }
        Ok(())
    }
}
fn finite(value: &serde_json::Number) -> bool {
    value.as_f64().is_some_and(f64::is_finite)
}
fn positive(value: &serde_json::Number) -> bool {
    value
        .as_f64()
        .is_some_and(|value| value.is_finite() && value > 0.0)
}
fn dependencies(
    flow: &EvidenceFlow,
    ids: &BTreeSet<String>,
    artifacts: &BTreeMap<String, String>,
) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for edge in &flow.edges {
        if edge.from == edge.to
            || !ids.contains(&edge.from)
            || !ids.contains(&edge.to)
            || !artifacts.contains_key(&edge.artifact)
            || !seen.insert((&edge.from, &edge.to, &edge.artifact))
        {
            return Err("dangling or repeated evidence dependency".into());
        }
    }
    let mut remaining = ids.clone();
    while !remaining.is_empty() {
        let roots: Vec<_> = remaining
            .iter()
            .filter(|node| {
                !flow
                    .edges
                    .iter()
                    .any(|edge| &edge.to == *node && remaining.contains(&edge.from))
            })
            .cloned()
            .collect();
        if roots.is_empty() {
            return Err("cyclic evidence dependencies".into());
        }
        for root in roots {
            remaining.remove(&root);
        }
    }
    Ok(())
}
