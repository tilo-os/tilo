//! The engine's own checks: what each evidence class accepts and refuses, how imported
//! identities are formed, what the device leaves behind, and what a structural design
//! resolves to.

use crate::graph::{ConnectError, Edge, Graph, Port};
use crate::operator::{OperatorKind, OperatorNode};
use crate::verification::VerificationRecord;
use crate::{AttentionCache, StorageReport, call, report_id};

const REPORT: &str = include_str!("../fixtures/storage-report.json");
const CHECKPOINT: &str = include_str!("../fixtures/checkpoint-verification.json");
const MAPPING: &str = include_str!("../fixtures/research-mapping.json");
const SUPERBLOCK: &str = include_str!("../fixtures/q4k-superblock.json");
const FLOW: &str = include_str!("../fixtures/evidence-flow.json");
const RMS: &str = include_str!("../fixtures/operators/rms-norm.json");
const SILU: &str = include_str!("../fixtures/operators/silu.json");
const DELTA: &str = include_str!("../fixtures/operators/gated-delta.json");
const DELTA_GEOMETRY: &str = include_str!("../fixtures/operators/gated-delta-geometry.json");
const SOFTMAX: &str = include_str!("../fixtures/operators/softmax.json");

fn imported(json: &str) -> serde_json::Value {
    let ask = serde_json::json!({"json": json}).to_string();
    serde_json::from_str(&call("import", &ask).unwrap_or_else(|error| panic!("{error}"))).unwrap()
}

fn document(json: &str) -> serde_json::Value {
    imported(json)["facts"].clone()
}

fn refused(json: &str) {
    let ask = serde_json::json!({"json": json}).to_string();
    assert!(call("import", &ask).is_err(), "accepted: {json}");
}

/// The stored report, changed at one path, as JSON.
fn changed(base: &str, edit: impl FnOnce(&mut serde_json::Value)) -> String {
    let mut value: serde_json::Value = serde_json::from_str(base).unwrap();
    edit(&mut value);
    value.to_string()
}

#[test]
fn a_storage_report_keeps_its_provenance_exact_bytes_and_arithmetic() {
    let answer = imported(REPORT);
    assert_eq!(answer["kind"], "report");
    assert_eq!(answer["id"], report_id(REPORT));
    assert_eq!(answer["stored"], REPORT);
    let report = &answer["facts"];
    assert_eq!(report["model"], "Synthetic/Contract-Example");
    assert_eq!(
        report["revision"],
        "ad835591193cff5d2a231485cab35f8654c71287"
    );
    assert_eq!(report["packed_weight_bytes"], 512);
    assert!(
        report["not_estimated"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == "coding_quality")
    );
    assert_eq!(report["assumptions"]["weight_bits"], 4);
}

#[test]
fn a_forged_report_arithmetic_or_moving_revision_is_refused() {
    refused(&changed(REPORT, |report| {
        report["packed_weight_bytes"] = serde_json::json!(1);
    }));
    refused(&changed(REPORT, |report| {
        report["revision"] = serde_json::json!("main");
    }));
    refused(&changed(REPORT, |report| {
        report["limitations"] = serde_json::json!(vec!["annotation"; 100]);
    }));
    refused(&changed(REPORT, |report| {
        report["silicon_area"] = serde_json::json!(1);
    }));
    refused(&" ".repeat(crate::MAX_REPORT_BYTES + 1));
}

#[test]
fn an_unsupported_cache_stays_unknown_and_never_zero() {
    let json = changed(REPORT, |report| {
        report["full_attention_cache"] = serde_json::json!({
            "status": "unsupported",
            "reason": "specialized cache formula unverified",
        });
    });
    let report = StorageReport::parse(&json).unwrap();
    assert!(matches!(
        report.full_attention_cache,
        AttentionCache::Unsupported { .. }
    ));
    let facts = document(&json);
    assert_eq!(facts["full_attention_cache"]["status"], "unsupported");
}

#[test]
fn evidence_that_is_neither_a_report_nor_a_receipt_is_refused() {
    refused("{\"evidence\": \"an_unheard_class\", \"schema_version\": 1}");
    refused("not json at all");
}

#[test]
fn every_evidence_class_of_hardware_receipt_is_taken_in() {
    for (json, evidence) in [
        (CHECKPOINT, "checkpoint_tile_integer_contract"),
        (MAPPING, "research_library_checkpoint_tile"),
        (SUPERBLOCK, "q4k_superblock_residue_rtl"),
        (FLOW, "evidence_flow"),
        (RMS, "bounded_fused_scalar_rms_norm"),
        (SILU, "bounded_scalar_silu_libm_characterization"),
        (DELTA, "bounded_scalar_gated_delta_state_transition"),
        (
            DELTA_GEOMETRY,
            "bounded_scalar_gated_delta_state_transition",
        ),
        (SOFTMAX, "bounded_scalar_masked_softmax_characterization"),
    ] {
        let answer = imported(json);
        assert_eq!(answer["kind"], "verification", "{evidence}");
        assert_eq!(answer["facts"]["receipt"]["evidence"], evidence);
        assert_eq!(answer["facts"]["imported_bytes_blake3"], report_id(json));
        assert_eq!(answer["id"], report_id(answer["stored"].as_str().unwrap()));
    }
}

#[test]
fn a_receipt_that_claims_success_without_its_observations_is_refused() {
    for (json, edit) in [
        (CHECKPOINT, 0_u8),
        (CHECKPOINT, 1),
        (CHECKPOINT, 2),
        (CHECKPOINT, 3),
        (MAPPING, 4),
        (SUPERBLOCK, 5),
    ] {
        let refused = changed(json, |receipt| match edit {
            0 => {
                receipt["checks"] = serde_json::json!({"generic_synthesis": "passed"});
            }
            1 => {
                receipt["excludes"] = serde_json::json!(["coding_quality"]);
            }
            2 => {
                receipt["tools"]["verilator"]["executable_sha256"] = serde_json::json!("bad");
            }
            3 => {
                receipt["failure"] = serde_json::json!("success cannot include failure");
            }
            4 => {
                receipt["manufacturable"] = serde_json::json!(true);
            }
            _ => {
                receipt["generic_cell_count"] = serde_json::json!(1);
            }
        });
        let ask = serde_json::json!({"json": refused}).to_string();
        assert!(call("import", &ask).is_err());
    }
}

#[test]
fn a_failed_or_cancelled_receipt_keeps_its_terminal_status_and_no_unrun_checks() {
    let json = changed(CHECKPOINT, |receipt| {
        receipt["status"] = serde_json::json!("failed");
        receipt["input_validation"] = serde_json::json!("failed");
        receipt["synthetic_activations"] = serde_json::json!(null);
        receipt["sample_manifest_sha256"] = serde_json::json!(null);
        receipt["artifact_validator_sha256"] = serde_json::json!(null);
        receipt["checks"] = serde_json::json!({});
        receipt["failure_stage"] = serde_json::json!("input_validation");
        receipt["failure"] = serde_json::json!("Reported tool execution failed");
        receipt["tools"] = serde_json::json!({});
        receipt["artifact_sha256"] = serde_json::json!({});
    });
    let facts = document(&json);
    assert_eq!(facts["receipt"]["status"], "failed");
    assert_eq!(facts["receipt"]["failure_stage"], "input_validation");
    assert!(facts["receipt"]["checks"].as_array().unwrap().is_empty());
}

#[test]
fn device_paths_never_reach_the_workspace() {
    let json = changed(CHECKPOINT, |receipt| {
        receipt["input_directory"] = serde_json::json!("/private/device/checkpoint");
        receipt["run_directory"] = serde_json::json!("/private/device/run");
        receipt["status"] = serde_json::json!("failed");
        receipt["failure"] = serde_json::json!(
            "Reported /private/device/checkpoint/sample.json failed; see /private/device/run/run.log"
        );
        receipt["checks"] = serde_json::json!({});
        receipt["synthetic_activations"] = serde_json::json!(null);
        receipt["sample_manifest_sha256"] = serde_json::json!(null);
        receipt["artifact_validator_sha256"] = serde_json::json!(null);
        receipt["failure_stage"] = serde_json::json!("input_validation");
        receipt["tools"] = serde_json::json!({});
        receipt["artifact_sha256"] = serde_json::json!({});
    });
    let answer = imported(&json);
    let stored = answer["stored"].as_str().unwrap();
    assert!(!stored.contains("/private/device"), "{stored}");
    assert!(!answer["facts"].to_string().contains("/private/device"));
    let failure = answer["facts"]["receipt"]["failure"].as_str().unwrap();
    assert!(
        failure.contains("<input directory>/sample.json"),
        "{failure}"
    );
    assert!(failure.contains("<run directory>/run.log"), "{failure}");
}

#[test]
fn a_cpu_receipt_is_normalized_with_its_observation_summary() {
    let facts = document(RMS);
    let summary = &facts["receipt"]["operator_summary"];
    assert_eq!(
        summary["original_receipt_sha256"],
        "47d47caf9ce1a08e1146eea8531a591dcd95010ef8edab7617d6743ba9e90eb2"
    );
    assert_eq!(summary["observations"][0]["kind"], "rms_norm");
    assert!(
        !summary["local_source_sha256"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let stored = imported(RMS)["stored"].as_str().unwrap().to_owned();
    assert!(!stored.contains("/lib64/"), "{stored}");
    for json in [SILU, DELTA, SOFTMAX] {
        let facts = document(json);
        assert!(facts["receipt"]["operator_summary"]["observations"][0]["kind"].is_string());
        let stored = imported(json)["stored"].as_str().unwrap().to_owned();
        assert!(!stored.contains("/lib64/"), "{stored}");
    }
}

#[test]
fn a_cpu_claim_that_contradicts_its_own_observations_is_refused() {
    let cases = [
        changed(RMS, |receipt| {
            receipt["status"] = serde_json::json!("failed")
        }),
        changed(RMS, |receipt| {
            receipt["primary_revision"] = serde_json::json!("main");
        }),
        changed(SILU, |receipt| {
            receipt["comparisons"]["total_inputs"] = serde_json::json!(1);
        }),
        changed(DELTA, |receipt| {
            receipt["comparisons"][0]["state_words"] = serde_json::json!(1);
        }),
        changed(SOFTMAX, |receipt| {
            receipt["comparisons"][0]["observed_probability_sum_fraction"] =
                serde_json::json!("1/0");
        }),
    ];
    for json in cases {
        refused(&json);
    }
}

#[test]
fn an_evidence_flow_names_its_stages_and_their_dependencies() {
    let facts = document(FLOW);
    let flow = &facts["receipt"]["evidence_flow"];
    assert_eq!(flow["stages"].as_array().unwrap().len(), 4);
    assert!(!flow["edges"].as_array().unwrap().is_empty());
    assert_eq!(flow["subflows"][0]["id"], "layer-group");
    assert!(flow["stages"][0]["checks"].is_array());
}

/// A reported evidence flow whose physical stage carries a macro placement.
fn flow_with_placement() -> String {
    let hash = "a".repeat(64);
    serde_json::json!({
        "schema_version": 1,
        "evidence": "evidence_flow",
        "status": "passed",
        "checks": {"reported_dependencies": "passed"},
        "declared_toolchain_image": null,
        "tools": {},
        "artifact_sha256": {
            "physical.json": hash,
            "db.h5": hash,
            "report.json": hash,
        },
        "excludes": [
            "artifact_authentication",
            "silicon_measurement",
            "manufacturing",
            "model_quality",
        ],
        "evidence_flow": {
            "title": "A reported flow with a placement",
            "input_identity_sha256": hash,
            "stages": [{
                "id": "physical",
                "title": "Physical",
                "status": "passed",
                "original_receipt_artifact": "physical.json",
                "producer_sha256": hash,
                "input_identity_sha256": hash,
                "checks": {"extraction": "passed"},
                "limitations": ["Reported only; no signoff."],
                "observations": {
                    "scope": "library_physical",
                    "library": "research-library",
                    "clock_constraint_ns": 2.5,
                    "setup_slack_ns": 0.1,
                    "hold_slack_ns": 0.05,
                    "routed_signal_nets": 10,
                    "router_violations": 0,
                    "library_area_um2": 123.5,
                },
                "placement": {
                    "database_artifact": "db.h5",
                    "observation_artifact": "report.json",
                    "dbu_per_micron": 1000,
                    "die_dbu": [0, 0, 100_000, 100_000],
                    "total_instances": 1,
                    "omitted_instances": 0,
                    "macros": [{
                        "id": "m1",
                        "name": "SRAM0",
                        "master": "sram_1rw",
                        "orientation": "R0",
                        "status": "placed",
                        "bounds_dbu": [0, 0, 1000, 2000],
                    }],
                },
            }],
            "edges": [],
            "subflows": [],
        },
    })
    .to_string()
}

#[test]
fn a_reported_flow_keeps_its_placements_and_their_macros() {
    let json = flow_with_placement();
    let facts = document(&json);
    let stage = &facts["receipt"]["evidence_flow"]["stages"][0];
    let placement = &stage["placement"];
    assert_eq!(placement["dbu_per_micron"], 1000);
    assert_eq!(placement["total_instances"], 1);
    assert_eq!(placement["macros"][0]["name"], "SRAM0");
    assert_eq!(
        placement["macros"][0]["bounds_dbu"],
        serde_json::json!([0, 0, 1000, 2000])
    );
}

#[test]
fn a_placement_that_leaves_the_die_or_loses_an_instance_is_refused() {
    let mut value: serde_json::Value = serde_json::from_str(&flow_with_placement()).unwrap();
    value["evidence_flow"]["stages"][0]["placement"]["macros"][0]["bounds_dbu"] =
        serde_json::json!([0, 0, 200_000, 2000]);
    refused(&value.to_string());
    let mut value: serde_json::Value = serde_json::from_str(&flow_with_placement()).unwrap();
    value["evidence_flow"]["stages"][0]["placement"]["omitted_instances"] = serde_json::json!(5);
    refused(&value.to_string());
    let mut value: serde_json::Value = serde_json::from_str(&flow_with_placement()).unwrap();
    value["evidence_flow"]["stages"][0]["placement"]["dbu_per_micron"] = serde_json::json!(0);
    refused(&value.to_string());
}

#[test]
fn an_architecture_study_keeps_its_stage_links_and_variants() {
    let facts = document(CHECKPOINT);
    assert_eq!(
        facts["receipt"]["evidence"],
        "checkpoint_tile_integer_contract"
    );
    assert!(
        facts["receipt"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["name"] == "generic_synthesis" && check["result"] == "passed")
    );
    let json = changed(CHECKPOINT, |receipt| {
        receipt["evidence"] = serde_json::json!("architecture_study");
        receipt["checks"] = serde_json::json!({"input_identity_recorded": "passed"});
        receipt["excludes"] = serde_json::json!([
            "full_model_execution",
            "coding_quality",
            "physical_ppa",
            "manufacturing",
            "payload_authentication",
            "synthesized_netlist_equivalence",
        ]);
        receipt["architecture_study"] = serde_json::json!({
            "stage": "prepare",
            "original_receipt_sha256": "a".repeat(64),
            "producer_sha256": "b".repeat(64),
            "input_identity_sha256": "c".repeat(64),
            "links": {},
            "expected_comparison_words": 64,
            "variants": [],
        });
        receipt["tools"] = serde_json::json!({
            "window": {"version": "prepared", "executable_sha256": "d".repeat(64)},
        });
        receipt["status"] = serde_json::json!("passed");
        receipt["checks"] = serde_json::json!({"input_identity_recorded": "passed"});
        receipt["failure"] = serde_json::Value::Null;
        receipt["artifact_sha256"] = serde_json::json!({"prepared.json": "e".repeat(64)});
        receipt["sample_manifest_sha256"] = serde_json::Value::Null;
        receipt["synthetic_activations"] = serde_json::Value::Null;
        receipt["input_validation"] = serde_json::Value::Null;
        receipt["verifier_sha256"] = serde_json::Value::Null;
        receipt["artifact_validator_sha256"] = serde_json::Value::Null;
        receipt["failure_stage"] = serde_json::Value::Null;
    });
    let facts = document(&json);
    assert_eq!(facts["receipt"]["architecture_study"]["stage"], "prepare");
    assert_eq!(
        facts["receipt"]["architecture_study"]["expected_comparison_words"],
        64
    );
}

#[test]
fn a_research_mapping_keeps_its_declared_scope_and_refuses_a_manufacturing_claim() {
    let facts = document(MAPPING);
    assert_eq!(facts["receipt"]["mapped_cell_count"], 100);
    assert_eq!(facts["receipt"]["reported_library_area"], "123.5");
    assert_eq!(facts["receipt"]["manufacturable"], false);
    assert!(
        facts["receipt"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["name"] == "mapped_netlist_vs_integer_oracle"
                && check["result"] == "passed")
    );
    refused(&changed(MAPPING, |receipt| {
        receipt["reported_library_area"] = serde_json::json!(-1);
    }));
    refused(&changed(MAPPING, |receipt| {
        receipt["mapped_cell_count"] = serde_json::json!(0);
    }));
    refused(&changed(MAPPING, |receipt| {
        receipt["input_sha256"]
            .as_object_mut()
            .unwrap()
            .remove("LICENSE");
    }));
}

#[test]
fn a_superblock_receipt_refuses_incomplete_success() {
    let facts = document(SUPERBLOCK);
    assert_eq!(facts["receipt"]["generic_cell_count"], 9940);
    assert_eq!(facts["receipt"]["external_scalar_C_comparisons"], 32);
    let sources = facts["receipt"]["source_sha256"].as_array().unwrap();
    assert!(sources.iter().any(|source| {
        source["name"] == "harness"
            && source["digest"]
                == "53d12021f98728cd7e92147cecf8b7c2dc5d69d74467b912e33d46b1b1f8c7bc"
    }));
    refused(&changed(SUPERBLOCK, |receipt| {
        receipt["primary_source_sha256"]["ggml-common.h"] = serde_json::json!("a".repeat(64));
    }));
    refused(&changed(SUPERBLOCK, |receipt| {
        receipt["artifact_sha256"]
            .as_object_mut()
            .unwrap()
            .remove("c-output.bin");
    }));
    refused(&changed(SUPERBLOCK, |receipt| {
        receipt["compiler_flags"][3] = serde_json::json!("-ffast-math");
    }));
}

#[test]
fn an_identical_document_has_one_identity_and_its_stored_form_is_stable() {
    let first = imported(REPORT);
    let second = imported(REPORT);
    assert_eq!(first["id"], second["id"]);
    let receipt = imported(CHECKPOINT);
    let stored = receipt["stored"].as_str().unwrap().to_owned();
    let again = VerificationRecord::read(&stored).unwrap();
    assert_eq!(again.imported_bytes_blake3, report_id(CHECKPOINT));
    assert_eq!(receipt["id"], report_id(&stored));
}

#[test]
fn a_report_and_a_receipt_are_distinguished_by_their_evidence_class() {
    assert_eq!(imported(REPORT)["kind"], "report");
    assert_eq!(imported(FLOW)["kind"], "verification");
}

// ---------------------------------------------------------------------------
// The structural design
// ---------------------------------------------------------------------------

fn node(
    id: &str,
    kind: OperatorKind,
    shape: &[u16],
    output_width: Option<u32>,
) -> serde_json::Value {
    let mut value = serde_json::json!({"id": id, "kind": kind, "shape": shape, "x": 0, "y": 0});
    if let Some(width) = output_width {
        value["output_width"] = serde_json::json!(width);
    }
    value
}

fn resolved(nodes: &[serde_json::Value], connections: &[serde_json::Value]) -> serde_json::Value {
    let ask = serde_json::json!({"nodes": nodes, "connections": connections}).to_string();
    serde_json::from_str(&call("graph", &ask).unwrap()).unwrap()
}

fn edge(from: &str, from_port: &str, to: &str, to_port: &str) -> serde_json::Value {
    serde_json::json!({
        "from_node": from,
        "from_port": from_port,
        "to_node": to,
        "to_port": to_port,
    })
}

#[test]
fn every_operator_offers_the_typed_ports_its_shape_gives_it() {
    let answer = resolved(
        &[
            node("x", OperatorKind::Input, &[4, 8], None),
            node("w", OperatorKind::NormWeightInput, &[8], None),
            node("rms", OperatorKind::RmsNorm, &[4, 8], None),
            node("linear", OperatorKind::LinearProjection, &[4, 8], Some(16)),
        ],
        &[],
    );
    let by_id = |id: &str| {
        answer["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == id)
            .cloned()
            .unwrap()
    };
    let input = by_id("x");
    assert!(input["inputs"].as_array().unwrap().is_empty());
    assert_eq!(input["outputs"][0]["id"], "value");
    assert_eq!(input["outputs"][0]["data_type"], "f32[4, 8]");
    let weights = by_id("w");
    assert_eq!(weights["outputs"][0]["data_type"], "f32[8]");
    let linear = by_id("linear");
    assert_eq!(linear["inputs"][1]["data_type"], "f32[16, 8]");
    assert_eq!(linear["outputs"][0]["data_type"], "f32[4, 16]");
    assert_eq!(by_id("rms")["kind"], "rms_norm");
}

#[test]
fn connections_resolve_into_active_edges_and_rejections_with_reasons() {
    let nodes = vec![
        node("x", OperatorKind::Input, &[4, 8], None),
        node("rms", OperatorKind::RmsNorm, &[4, 8], None),
        node("silu", OperatorKind::Silu, &[4, 8], None),
        node("mul", OperatorKind::Multiply, &[4, 8], None),
        node("w", OperatorKind::NormWeightInput, &[8], None),
    ];
    let answer = resolved(
        &nodes,
        &[
            edge("x", "value", "rms", "value"),
            edge("rms", "result", "mul", "left"),
            edge("silu", "result", "mul", "left"),
            edge("rms", "missing", "silu", "value"),
            edge("w", "weights", "rms", "value"),
        ],
    );
    let rejected = answer["rejected"].as_array().unwrap();
    let reason = |from: &str, port: &str| {
        rejected
            .iter()
            .find(|row| row["from_node"] == from && row["from_port"] == port)
            .map(|row| row["reason"].to_string())
            .unwrap()
    };
    assert_eq!(reason("rms", "missing"), "\"unknown_port\"");
    assert_eq!(reason("w", "weights"), "\"type_mismatch\"");
    // Both `rms` and `silu` ask for `mul`'s left input; one holds and the other is refused
    // for it, in the order the connections answer by identity.
    assert_eq!(answer["active"].as_array().unwrap().len(), 2);
    let occupying = rejected
        .iter()
        .filter(|row| row["reason"] == "input_occupied")
        .count();
    assert_eq!(occupying, 1);
    let held = answer["active"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["to_node"] == "mul" && row["to_port"] == "left");
    assert!(held, "{answer}");
}

#[test]
fn a_connection_that_would_close_a_cycle_is_rejected() {
    let nodes = vec![
        node("a", OperatorKind::RmsNorm, &[4, 8], None),
        node("b", OperatorKind::RmsNorm, &[4, 8], None),
        node("c", OperatorKind::RmsNorm, &[4, 8], None),
    ];
    let answer = resolved(
        &nodes,
        &[
            edge("a", "result", "b", "value"),
            edge("b", "result", "c", "value"),
            edge("c", "result", "a", "value"),
        ],
    );
    assert_eq!(answer["active"].as_array().unwrap().len(), 2);
    let rejected = answer["rejected"].as_array().unwrap();
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0]["reason"], "cycle");
}

#[test]
fn a_connection_identity_is_its_content_and_does_not_move() {
    let nodes = vec![
        node("x", OperatorKind::Input, &[4, 8], None),
        node("rms", OperatorKind::RmsNorm, &[4, 8], None),
    ];
    let link = edge("x", "value", "rms", "value");
    let first = resolved(&nodes, std::slice::from_ref(&link));
    let second = resolved(&nodes, &[link]);
    assert_eq!(first["active"][0]["id"], second["active"][0]["id"]);
    assert_eq!(
        first["active"][0]["id"], first["candidates"][0]["id"],
        "the active edge is the candidate that holds"
    );
}

#[test]
fn a_chosen_output_names_the_inputs_it_can_feed() {
    let ask = serde_json::json!({
        "nodes": [
            node("x", OperatorKind::Input, &[4, 8], None),
            node("rms", OperatorKind::RmsNorm, &[4, 8], None),
            node("w", OperatorKind::NormWeightInput, &[8], None),
            node("weighted", OperatorKind::ApplyNormWeight, &[4, 8], None),
        ],
        "connections": [],
        "from": {"node": "x", "port": "value"},
    })
    .to_string();
    let answer: serde_json::Value = serde_json::from_str(&call("graph", &ask).unwrap()).unwrap();
    let targets: Vec<(String, String)> = answer["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|target| {
            (
                target["node"].as_str().unwrap().to_owned(),
                target["port"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        targets,
        [
            ("rms".to_owned(), "value".to_owned()),
            ("weighted".to_owned(), "value".to_owned()),
        ]
    );
}

#[test]
fn an_operator_no_design_can_hold_is_refused() {
    for bad in [
        node("", OperatorKind::Input, &[4, 8], None),
        node("x", OperatorKind::Input, &[], None),
        node("x", OperatorKind::Input, &[0, 8], None),
        node("x", OperatorKind::Input, &[4, 8, 4, 8, 4], None),
        node("x", OperatorKind::MatrixWeightInput, &[8], None),
        node("x", OperatorKind::Input, &[4, 8], Some(8)),
        node("a/b", OperatorKind::Input, &[4, 8], None),
    ] {
        let ask = serde_json::json!({"nodes": [bad]}).to_string();
        assert!(call("graph", &ask).is_err());
    }
    let duplicate = serde_json::json!({
        "nodes": [
            node("x", OperatorKind::Input, &[4, 8], None),
            node("x", OperatorKind::Input, &[4, 8], None),
        ],
    })
    .to_string();
    assert!(call("graph", &duplicate).is_err());
}

#[test]
fn an_unknown_ask_is_refused_and_no_ask_is_a_job() {
    assert!(call("paint", "{}").is_err());
    assert!(crate::start("import", "{}").is_err());
    assert!(crate::status("job-1").is_err());
    assert!(!crate::cancel("job-1"));
}

#[test]
fn the_engine_answers_for_the_one_app_it_belongs_to() {
    assert_eq!(crate::APP, "so.tilo.chip-design");
}

#[test]
fn a_graph_built_by_the_engine_connects_in_dependency_order() {
    let mut graph = Graph::default();
    for id in ["source", "grade"] {
        graph.insert(crate::graph::GraphNode {
            id: id.into(),
            title: id.into(),
            position: crate::graph::GraphPoint::default(),
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
        });
    }
    graph
        .connect(Edge {
            from_node: "source".into(),
            from_port: "out".into(),
            to_node: "grade".into(),
            to_port: "in".into(),
        })
        .unwrap();
    assert_eq!(graph.evaluation_order().unwrap(), ["source", "grade"]);
    assert_eq!(
        OperatorNode::valid(&OperatorNode::default()),
        Err("invalid operator node identity, position, shape or matrix width".into())
    );
    assert_eq!(
        graph.connect(Edge {
            from_node: "missing".into(),
            from_port: "out".into(),
            to_node: "grade".into(),
            to_port: "in".into(),
        }),
        Err(ConnectError::UnknownNode)
    );
}
