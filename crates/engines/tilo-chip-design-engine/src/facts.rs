//! The documents the app reads, as the app's own types name them.
//!
//! The Rust structures serialize as the store keeps them; an app has no business reading a
//! store's shape. So what crosses back is the same claims under the names the app declares:
//! maps become lists of `name` and value, and an absent claim is absent rather than null, so
//! the app can say whether a receipt ever reported it.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::model::{AttentionCache, StorageReport};
use crate::verification::flow::{EvidenceFlow, FlowStage, Observation};
use crate::verification::placement::Placement;
use crate::verification::study::{ArchitectureStudy, Numerical, Synthesis};
use crate::verification::{
    CheckStatus, Observations, OperatorSummary, ToolIdentity, VerificationReceipt,
    VerificationRecord,
};

/// One report, as the app keeps it.
#[must_use]
pub fn report_facts(report: &StorageReport) -> Value {
    let cache = match &report.full_attention_cache {
        AttentionCache::PartialBound {
            full_attention_layers,
            kv_elements,
            packed_bytes,
            excludes,
        } => json!({
            "status": "partial_bound",
            "full_attention_layers": full_attention_layers,
            "kv_elements": kv_elements,
            "packed_bytes": packed_bytes,
            "excludes": excludes,
        }),
        AttentionCache::Unsupported { reason } => json!({
            "status": "unsupported",
            "reason": reason,
        }),
    };
    json!({
        "schema_version": report.schema_version,
        "model": report.model,
        "revision": report.revision,
        "config_sha256": report.config_sha256,
        "evidence": report.evidence,
        "batch_size": report.batch_size,
        "assumptions": {
            "logical_parameters": report.assumptions.logical_parameters,
            "weight_bits": report.assumptions.weight_bits,
            "kv_bits": report.assumptions.kv_bits,
            "context_tokens": report.assumptions.context_tokens,
        },
        "packed_weight_bytes": report.packed_weight_bytes,
        "full_attention_cache": cache,
        "not_estimated": report.not_estimated,
        "limitations": report.limitations,
    })
}

/// One verification receipt, as the app keeps it.
#[must_use]
pub fn receipt_facts(record: &VerificationRecord) -> Value {
    let mut facts = Map::new();
    facts.insert(
        "imported_bytes_sha256".into(),
        record
            .imported_bytes_sha256
            .clone()
            .map_or(Value::Null, Value::String),
    );
    facts.insert(
        "imported_bytes_blake3".into(),
        json!(record.imported_bytes_blake3),
    );
    facts.insert("receipt".into(), receipt_json(&record.receipt));
    Value::Object(facts)
}

fn receipt_json(receipt: &VerificationReceipt) -> Value {
    let mut value = Map::new();
    value.insert("schema_version".into(), json!(receipt.schema_version));
    value.insert("evidence".into(), json!(receipt.evidence));
    value.insert("status".into(), json!(receipt.status));
    value.insert("checks".into(), checks(&receipt.checks));
    value.insert("tools".into(), tools(&receipt.tools));
    value.insert("artifact_sha256".into(), digests(&receipt.artifact_sha256));
    value.insert("excludes".into(), json!(receipt.excludes));
    if let Some(image) = &receipt.declared_toolchain_image {
        value.insert("declared_toolchain_image".into(), json!(image));
    }
    if let Some(failure) = &receipt.failure {
        value.insert("failure".into(), json!(failure));
    }
    if let Some(stage) = &receipt.failure_stage {
        value.insert("failure_stage".into(), json!(stage));
    }
    if let Some(validation) = &receipt.input_validation {
        value.insert("input_validation".into(), json!(validation));
    }
    optional(&mut value, "lanes", receipt.lanes);
    optional(&mut value, "group_size", receipt.group_size);
    if let Some(hash) = &receipt.sample_manifest_sha256 {
        value.insert("sample_manifest_sha256".into(), json!(hash));
    }
    if let Some(hash) = &receipt.verifier_sha256 {
        value.insert("verifier_sha256".into(), json!(hash));
    }
    if let Some(hash) = &receipt.artifact_validator_sha256 {
        value.insert("artifact_validator_sha256".into(), json!(hash));
    }
    optional(
        &mut value,
        "synthetic_activations",
        receipt.synthetic_activations,
    );
    if let Some(hash) = &receipt.mapper_sha256 {
        value.insert("mapper_sha256".into(), json!(hash));
    }
    if let Some(revision) = &receipt.library_revision {
        value.insert("library_revision".into(), json!(revision));
    }
    optional(&mut value, "manufacturable", receipt.manufacturable);
    optional(&mut value, "mapped_cell_count", receipt.mapped_cell_count);
    if let Some(area) = &receipt.reported_library_area {
        value.insert("reported_library_area".into(), json!(area.to_string()));
    }
    optional(
        &mut value,
        "abc_delay_target_ps",
        receipt.abc_delay_target_ps,
    );
    optional(&mut value, "sample_sha256", receipt.sample_sha256.as_ref());
    optional(
        &mut value,
        "sample_receipt_sha256",
        receipt.sample_receipt_sha256.as_ref(),
    );
    optional(
        &mut value,
        "rtl_source_sha256",
        receipt.rtl_source_sha256.as_ref(),
    );
    if let Some(inputs) = &receipt.input_sha256 {
        value.insert("input_sha256".into(), digests(inputs));
    }
    if let Some(sources) = &receipt.primary_source_sha256 {
        value.insert("primary_source_sha256".into(), digests(sources));
    }
    if let Some(sources) = &receipt.source_sha256 {
        value.insert("source_sha256".into(), digests(sources));
    }
    if let Some(protocol) = &receipt.real_protocol {
        value.insert("real_protocol".into(), json!(protocol));
    }
    if let Some(flags) = &receipt.compiler_flags {
        value.insert("compiler_flags".into(), json!(flags));
    }
    optional(
        &mut value,
        "external_scalar_C_comparisons",
        receipt.external_scalar_c_comparisons,
    );
    if let Some(instrumentation) = &receipt.c_instrumentation {
        value.insert("C_instrumentation".into(), json!(instrumentation));
    }
    if let Some(cells) = &receipt.generic_cells {
        value.insert("generic_cells".into(), counts(cells));
    }
    optional(&mut value, "generic_cell_count", receipt.generic_cell_count);
    optional(
        &mut value,
        "excluded_metadata_cells",
        receipt.excluded_metadata_cells,
    );
    protocols(&mut value, receipt);
    Value::Object(value)
}

/// The reported protocol a receipt speaks, when it speaks one of the three.
fn protocols(value: &mut Map<String, Value>, receipt: &VerificationReceipt) {
    if let Some(operator) = &receipt.operator_summary {
        value.insert("operator_summary".into(), operator_json(operator));
    }
    if let Some(study) = &receipt.architecture_study {
        value.insert("architecture_study".into(), study_json(study));
    }
    if let Some(flow) = &receipt.evidence_flow {
        value.insert("evidence_flow".into(), flow_json(flow));
    }
}

fn operator_json(operator: &OperatorSummary) -> Value {
    json!({
        "original_receipt_sha256": operator.original_receipt_sha256,
        "primary_revision": operator.primary_revision,
        "primary_source_sha256": digests(&operator.primary_source_sha256),
        "local_source_sha256": digests(&operator.local_source_sha256),
        "linked_library_sha256": digests(&operator.linked_library_sha256),
        "compiler_version": operator.compiler_version,
        "compiler_executable_sha256": operator.compiler_executable_sha256,
        "compiler_flags": operator.compiler_flags,
        "observations": observations(&operator.observations),
    })
}

fn observations(observations: &Observations) -> Value {
    match observations {
        Observations::RmsNorm(cases) => Value::Array(
            cases
                .iter()
                .map(|case| {
                    json!({
                        "kind": "rms_norm",
                        "profile": case.profile,
                        "fp32_comparisons": case.fp32_comparisons,
                        "output_sha256": case.c_output_sha256,
                    })
                })
                .collect(),
        ),
        Observations::Silu(case) => json!([{
            "kind": "silu",
            "total_inputs": case.total_inputs,
            "exp_differences": case.exp_differences,
            "max_exp_ulp": case.max_exp_ulp,
        }]),
        Observations::GatedDelta(cases) => Value::Array(
            cases
                .iter()
                .map(|case| {
                    json!({
                        "kind": "gated_delta",
                        "heads": case.config.heads,
                        "tokens": case.config.tokens,
                        "width": case.config.width,
                        "attention_words": case.attention_words,
                        "state_words": case.state_words,
                    })
                })
                .collect(),
        ),
        Observations::Softmax(cases) => Value::Array(
            cases
                .iter()
                .map(|case| {
                    json!({
                        "kind": "softmax",
                        "name": case.name,
                        "width": case.width,
                        "masked": case.masked,
                        "differing_words": case.differing_words,
                        "max_ulp": case.max_ulp,
                    })
                })
                .collect(),
        ),
    }
}

fn study_json(study: &ArchitectureStudy) -> Value {
    let links: Vec<Value> = study
        .links
        .iter()
        .map(|(name, link)| {
            json!({
                "name": name,
                "receipt_sha256": link.receipt,
                "producer_sha256": link.producer,
                "input_identity_sha256": link.input_identity,
            })
        })
        .collect();
    let variants: Vec<Value> = study
        .variants
        .iter()
        .map(|variant| {
            let mut value = Map::new();
            value.insert("identity".into(), json!(variant.identity));
            if let Some(numerical) = &variant.numerical {
                value.insert("numerical".into(), numerical_json(numerical));
            }
            if let Some(synthesis) = &variant.synthesis {
                value.insert("synthesis".into(), synthesis_json(synthesis));
            }
            Value::Object(value)
        })
        .collect();
    json!({
        "stage": study.stage,
        "original_receipt_sha256": study.original_receipt_sha256,
        "producer_sha256": study.producer_sha256,
        "input_identity_sha256": study.input_identity_sha256,
        "expected_comparison_words": study.expected_comparison_words,
        "links": links,
        "variants": variants,
    })
}

fn numerical_json(numerical: &Numerical) -> Value {
    json!({
        "cold_program_cycles": numerical.cold_program_cycles,
        "vector_load_cycles": numerical.vector_load_cycles,
        "warm_cycles": numerical.warm_cycles,
        "accepted_weight_bytes": numerical.accepted_weight_bytes,
        "accepted_descriptor_bytes": numerical.accepted_descriptor_bytes,
        "cache_reads": numerical.cache_reads,
        "quantizer_packets": numerical.quantizer_packets,
        "comparison_words": numerical.comparison_words,
        "output_sha256": numerical.output_sha256,
    })
}

fn synthesis_json(synthesis: &Synthesis) -> Value {
    let memories: Vec<Value> = synthesis
        .memories
        .iter()
        .map(|memory| {
            json!({
                "name": memory.name,
                "width_bits": memory.width_bits,
                "depth_words": memory.depth_words,
                "bits": memory.bits,
                "read_ports": memory.read_ports,
                "clocked_read_ports": memory.clocked_read_ports,
                "write_ports": memory.write_ports,
                "fully_initialized": memory.fully_initialized,
            })
        })
        .collect();
    json!({
        "generic_logic_cells": synthesis.generic_logic_cells,
        "activation_cache_bits": synthesis.activation_cache_bits,
        "programmed_weight_bits": synthesis.programmed_weight_bits,
        "fixed_initialized_bits": synthesis.fixed_initialized_bits,
        "memories": memories,
    })
}

fn flow_json(flow: &EvidenceFlow) -> Value {
    let stages: Vec<Value> = flow.stages.iter().map(stage_json).collect();
    let edges: Vec<Value> = flow
        .edges
        .iter()
        .map(|edge| {
            json!({
                "from": edge.from,
                "to": edge.to,
                "kind": edge.kind,
                "artifact": edge.artifact,
            })
        })
        .collect();
    let subflows: Vec<Value> = flow
        .subflows
        .iter()
        .map(|link| {
            json!({
                "id": link.id,
                "title": link.title,
                "summary_artifact": link.summary_artifact,
            })
        })
        .collect();
    json!({
        "title": flow.title,
        "input_identity_sha256": flow.input_identity_sha256,
        "stages": stages,
        "edges": edges,
        "subflows": subflows,
    })
}

fn stage_json(stage: &FlowStage) -> Value {
    let mut value = Map::new();
    value.insert("id".into(), json!(stage.id));
    value.insert("title".into(), json!(stage.title));
    value.insert("status".into(), json!(stage.status));
    value.insert(
        "original_receipt_artifact".into(),
        json!(stage.original_receipt_artifact),
    );
    value.insert("producer_sha256".into(), json!(stage.producer_sha256));
    value.insert(
        "input_identity_sha256".into(),
        json!(stage.input_identity_sha256),
    );
    value.insert("checks".into(), checks(&stage.checks));
    value.insert("limitations".into(), json!(stage.limitations));
    if let Some(observation) = &stage.observations {
        value.insert("observations".into(), observation_json(observation));
    }
    if let Some(placement) = &stage.placement {
        value.insert("placement".into(), placement_json(placement));
    }
    Value::Object(value)
}

fn observation_json(observation: &Observation) -> Value {
    match observation {
        Observation::CpuReference {
            comparison,
            compared_words,
            differing_words,
            output_sha256,
        } => json!({
            "scope": "cpu_reference",
            "comparison": comparison,
            "compared_words": compared_words,
            "differing_words": differing_words,
            "output_sha256": output_sha256,
        }),
        Observation::RtlSimulation {
            outcome,
            refused_inputs,
            cycles,
            accepted_inputs,
            accepted_outputs,
            output_sha256,
        } => json!({
            "scope": "rtl_simulation",
            "outcome": outcome,
            "refused_inputs": refused_inputs,
            "cycles": cycles,
            "accepted_inputs": accepted_inputs,
            "accepted_outputs": accepted_outputs,
            "output_sha256": output_sha256,
        }),
        Observation::GenericSynthesis {
            logic_cells,
            logical_memory_bits,
        } => json!({
            "scope": "generic_synthesis",
            "logic_cells": logic_cells,
            "logical_memory_bits": logical_memory_bits,
        }),
        Observation::LibraryPhysical {
            library,
            clock_constraint_ns,
            setup_slack_ns,
            hold_slack_ns,
            routed_signal_nets,
            router_violations,
            library_area_um2,
        } => json!({
            "scope": "library_physical",
            "library": library,
            "clock_constraint_ns": clock_constraint_ns.to_string(),
            "setup_slack_ns": setup_slack_ns.to_string(),
            "hold_slack_ns": hold_slack_ns.to_string(),
            "routed_signal_nets": routed_signal_nets,
            "router_violations": router_violations,
            "library_area_um2": library_area_um2.to_string(),
        }),
    }
}

fn placement_json(placement: &Placement) -> Value {
    let macros: Vec<Value> = placement
        .macros
        .iter()
        .map(|item| {
            json!({
                "id": item.id,
                "name": item.name,
                "master": item.master,
                "orientation": item.orientation,
                "status": item.status,
                "bounds_dbu": item.bounds_dbu,
            })
        })
        .collect();
    json!({
        "database_artifact": placement.database_artifact,
        "observation_artifact": placement.observation_artifact,
        "dbu_per_micron": placement.dbu_per_micron,
        "die_dbu": placement.die_dbu,
        "total_instances": placement.total_instances,
        "omitted_instances": placement.omitted_instances,
        "macros": macros,
    })
}

fn checks(values: &BTreeMap<String, CheckStatus>) -> Value {
    Value::Array(
        values
            .iter()
            .map(|(name, result)| json!({"name": name, "result": result}))
            .collect(),
    )
}

fn tools(values: &BTreeMap<String, ToolIdentity>) -> Value {
    Value::Array(
        values
            .iter()
            .map(|(name, tool)| {
                json!({
                    "name": name,
                    "version": tool.version,
                    "digest": tool.executable_sha256,
                })
            })
            .collect(),
    )
}

fn digests(values: &BTreeMap<String, String>) -> Value {
    Value::Array(
        values
            .iter()
            .map(|(name, sha256)| json!({"name": name, "digest": sha256}))
            .collect(),
    )
}

fn counts(values: &BTreeMap<String, u64>) -> Value {
    Value::Array(
        values
            .iter()
            .map(|(name, count)| json!({"name": name, "count": count}))
            .collect(),
    )
}

/// Insert `key` only when the receipt reported it.
fn optional<T: Serialize>(value: &mut Map<String, Value>, key: &str, reported: Option<T>) {
    if let Some(reported) = reported {
        value.insert(key.to_owned(), json!(reported));
    }
}
