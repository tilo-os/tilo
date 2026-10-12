//! Model-independent, detached summaries of staged architecture comparisons.
use super::{
    CheckStatus, ReceiptStatus, VerificationReceipt, digest, hashes, identity_name, unique_map,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Prepare,
    Numerical,
    Fault,
    Synthesis,
    Aggregate,
}

impl Stage {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Numerical => "numerical",
            Self::Fault => "fault",
            Self::Synthesis => "synthesis",
            Self::Aggregate => "aggregate",
        }
    }
    fn checks(&self) -> &'static [&'static str] {
        match self {
            Self::Prepare => &["input_identity_recorded"],
            Self::Numerical => &["all_variants_complete_outputs", "protocol_controls"],
            Self::Fault => &["wrong_coefficient_rejected", "wrong_row_rejected"],
            Self::Synthesis => &["generic_structure_observed"],
            Self::Aggregate => &["linked_stages_match_inputs"],
        }
    }
    fn links(&self) -> &'static [&'static str] {
        match self {
            Self::Prepare => &[],
            Self::Numerical | Self::Synthesis => &["prepare"],
            Self::Fault => &["prepare", "numerical"],
            Self::Aggregate => &["prepare", "numerical", "fault", "synthesis"],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageLink {
    #[serde(rename = "receipt_sha256")]
    pub receipt: String,
    #[serde(rename = "producer_sha256")]
    pub producer: String,
    #[serde(rename = "input_identity_sha256")]
    pub input_identity: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Numerical {
    pub cold_program_cycles: u64,
    pub vector_load_cycles: u64,
    pub warm_cycles: u64,
    pub accepted_weight_bytes: u64,
    pub accepted_descriptor_bytes: u64,
    pub cache_reads: u64,
    pub quantizer_packets: u64,
    pub comparison_words: u64,
    pub output_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Synthesis {
    pub generic_logic_cells: u64,
    pub activation_cache_bits: u64,
    pub programmed_weight_bits: u64,
    pub fixed_initialized_bits: u64,
    pub licensed_memory_macro: bool,
    pub synthesized_netlist_equivalence: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memories: Vec<LogicalMemory>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalMemory {
    pub name: String,
    pub width_bits: u64,
    pub depth_words: u64,
    pub bits: u64,
    pub read_ports: u64,
    pub clocked_read_ports: u64,
    pub write_ports: u64,
    pub fully_initialized: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variant {
    pub identity: String,
    pub numerical: Option<Numerical>,
    pub synthesis: Option<Synthesis>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchitectureStudy {
    pub stage: Stage,
    pub original_receipt_sha256: String,
    pub producer_sha256: String,
    pub input_identity_sha256: String,
    #[serde(deserialize_with = "unique_map")]
    pub links: BTreeMap<String, StageLink>,
    pub expected_comparison_words: u64,
    pub variants: Vec<Variant>,
}

const EXCLUSIONS: &[&str] = &[
    "full_model_execution",
    "coding_quality",
    "physical_ppa",
    "manufacturing",
    "payload_authentication",
    "synthesized_netlist_equivalence",
];

pub(super) fn validate(receipt: &VerificationReceipt) -> Result<(), String> {
    let study = receipt
        .architecture_study
        .as_ref()
        .ok_or("architecture study missing")?;
    // Reject fields of unrelated receipt protocols; never silently reinterpret them.
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
        "architecture_study",
    ];
    if receipt.schema_version != 1
        || value
            .as_object()
            .is_none_or(|fields| fields.keys().any(|key| !allowed.contains(&key.as_str())))
        || [&receipt.input_directory, &receipt.run_directory]
            .into_iter()
            .any(|path| path.as_ref().is_some_and(|value| !super::text(value)))
        || !digest(&study.original_receipt_sha256)
        || !digest(&study.producer_sha256)
        || !digest(&study.input_identity_sha256)
        || study.expected_comparison_words == 0
        || !hashes(&receipt.artifact_sha256)
        || receipt.artifact_sha256.is_empty()
        || receipt.excludes.len() > 32
        || receipt.excludes.iter().any(|name| !identity_name(name))
        || EXCLUSIONS
            .iter()
            .any(|name| !receipt.excludes.iter().any(|value| value == name))
    {
        return Err("invalid architecture study identity, scope or envelope".into());
    }
    validate_terminal(receipt, study)?;
    validate_links(receipt, study)?;
    validate_variants(receipt, study)
}

fn validate_terminal(
    receipt: &VerificationReceipt,
    study: &ArchitectureStudy,
) -> Result<(), String> {
    if receipt
        .checks
        .keys()
        .any(|name| !study.stage.checks().contains(&name.as_str()))
        || receipt.tools.is_empty()
        || receipt.tools.len() > 8
        || receipt.tools.iter().any(|(name, tool)| {
            !tool_name(name) || !super::text(&tool.version) || !digest(&tool.executable_sha256)
        })
        || receipt
            .declared_toolchain_image
            .as_ref()
            .is_some_and(|image| {
                image
                    .strip_prefix("sha256:")
                    .is_none_or(|hash| !digest(hash))
            })
    {
        return Err("invalid architecture study checks or tools".into());
    }
    match receipt.status {
        ReceiptStatus::Passed
            if receipt.failure.is_none()
                && study
                    .stage
                    .checks()
                    .iter()
                    .all(|name| receipt.checks.get(*name) == Some(&CheckStatus::Passed)) =>
        {
            Ok(())
        }
        ReceiptStatus::Failed | ReceiptStatus::Cancelled
            if receipt
                .failure
                .as_ref()
                .is_some_and(|value| super::text(value)) =>
        {
            Ok(())
        }
        _ => Err("architecture study terminal claim is incomplete".into()),
    }
}

fn tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
}

fn validate_links(receipt: &VerificationReceipt, study: &ArchitectureStudy) -> Result<(), String> {
    if study
        .links
        .keys()
        .any(|name| !study.stage.links().contains(&name.as_str()))
        || receipt.status == ReceiptStatus::Passed
            && study
                .stage
                .links()
                .iter()
                .any(|name| !study.links.contains_key(*name))
        || study.links.values().any(|link| {
            !digest(&link.receipt)
                || !digest(&link.producer)
                || link.input_identity != study.input_identity_sha256
        })
    {
        return Err("architecture stage links do not describe the same inputs".into());
    }
    Ok(())
}

fn validate_variants(
    receipt: &VerificationReceipt,
    study: &ArchitectureStudy,
) -> Result<(), String> {
    let numerical = matches!(study.stage, Stage::Numerical | Stage::Aggregate);
    let synthesis = matches!(study.stage, Stage::Synthesis | Stage::Aggregate);
    if study.variants.len() > 16
        || (!numerical && !synthesis && !study.variants.is_empty())
        || receipt.status == ReceiptStatus::Passed
            && (numerical || synthesis)
            && study.variants.len() < 2
    {
        return Err("architecture comparison has invalid variant coverage".into());
    }
    let mut identities = std::collections::BTreeSet::new();
    let mut output = None;
    for variant in &study.variants {
        if !identity_name(&variant.identity)
            || !identities.insert(&variant.identity)
            || variant.numerical.is_some() != numerical
            || variant.synthesis.is_some() != synthesis
        {
            return Err("invalid architecture variant identity or stage fields".into());
        }
        if let Some(observation) = &variant.numerical {
            if observation.warm_cycles == 0
                || observation.vector_load_cycles == 0
                || observation.comparison_words == 0
                || observation.comparison_words > study.expected_comparison_words
                || !digest(&observation.output_sha256)
                || receipt.status == ReceiptStatus::Passed
                    && (observation.comparison_words != study.expected_comparison_words
                        || output.is_some_and(|prior| {
                            prior
                                != (
                                    observation.comparison_words,
                                    observation.output_sha256.as_str(),
                                )
                        }))
            {
                return Err(
                    "architecture variants lack matching complete output declarations".into(),
                );
            }
            output = Some((
                observation.comparison_words,
                observation.output_sha256.as_str(),
            ));
        }
        if let Some(observation) = &variant.synthesis
            && (observation.generic_logic_cells == 0
                || observation.licensed_memory_macro
                || observation.synthesized_netlist_equivalence)
        {
            return Err(
                "generic synthesis cannot assert physical memory or netlist qualification".into(),
            );
        }
        if let Some(observation) = &variant.synthesis {
            validate_memories(&observation.memories)?;
        }
    }
    Ok(())
}

fn validate_memories(memories: &[LogicalMemory]) -> Result<(), String> {
    let mut names = std::collections::BTreeSet::new();
    if memories.len() > 32
        || memories.iter().any(|memory| {
            !super::text(&memory.name)
                || memory.name.len() > 512
                || memory.name.chars().any(char::is_control)
                || !names.insert(&memory.name)
                || memory.width_bits == 0
                || memory.depth_words == 0
                || memory.width_bits.checked_mul(memory.depth_words) != Some(memory.bits)
                || memory.read_ports == 0 && memory.write_ports == 0
                || memory.read_ports > 65536
                || memory.write_ports > 65536
                || memory.clocked_read_ports > memory.read_ports
        })
    {
        return Err("invalid reported logical memory geometry or ports".into());
    }
    Ok(())
}
