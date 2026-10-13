use ui::framework::testing::TestApp;
use ui::tilo_resource::{ActionRef, Doc, Invoke, Replica, Resource as _};

use crate::{ChipDesignShell, Project as ChipProject, ProjectState};

const REPORT: &str = include_str!("../fixtures/storage-report.json");

/// What a test app's project holds, parsed as the views read it.
pub(crate) trait Parsed {
    fn parsed(&self) -> ProjectState;
}

impl Parsed for TestApp<ChipDesignShell> {
    fn parsed(&self) -> ProjectState {
        ProjectState::of(&self.state())
    }
}

/// Invoke the operator graph's `action` with `args` as an agent does: the
/// project's verb, or the field edit of a matrix node's output width.
pub(crate) fn graph(
    app: &mut TestApp<ChipDesignShell>,
    action: &str,
    args: serde_json::Value,
) -> Result<Option<serde_json::Value>, ui::tilo_actions::DoorError> {
    if action == "set-output-width" {
        let node = args["node"].as_str().unwrap_or_default();
        let width = args["width"]
            .as_u64()
            .and_then(|width| u32::try_from(width).ok());
        let edit = ChipProject::paths()
            .nodes
            .item(&ui::tilo_resource::Id::from_raw(node))
            .output_width
            .set(width);
        return app.call("field-edit", serde_json::json!({ "edit": edit }));
    }
    app.call(action, args)
}

/// Apply `invoke` to a peer's copy of the project, as that peer would.
pub(crate) fn on_peer(peer: &mut Doc, invoke: Invoke<ChipProject>) -> ProjectState {
    let mut replica = Replica::<ChipProject>::from_doc(peer.clone());
    replica.apply(invoke).unwrap();
    *peer = replica.doc().clone();
    ProjectState::of(&replica.state().unwrap())
}
const IMPORT: ActionRef<ChipProject, crate::resource::project::ImportReport> =
    ChipProject::IMPORT_REPORT;
const REMOVE: ActionRef<ChipProject, crate::resource::project::RemoveReport> =
    ChipProject::REMOVE_REPORT;

#[test]
fn semantic_import_preserves_provenance_and_exact_storage_and_is_undoable() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    assert!(app.parsed().reports.is_empty(), "live projects start empty");
    app.invoke(IMPORT.with(REPORT.to_owned())).unwrap();
    let state = app.parsed();
    let id = crate::model::report_id(REPORT);
    let report = state
        .reports
        .get(&id)
        .expect("semantic import published the report");
    assert_eq!(report.model, "Synthetic/Contract-Example");
    assert_eq!(report.revision, "ad835591193cff5d2a231485cab35f8654c71287");
    assert_eq!(report.packed_weight_bytes, 512);
    assert!(
        report
            .not_estimated
            .iter()
            .any(|item| item == "coding_quality")
    );
    assert!(app.undo());
    assert!(app.parsed().reports.is_empty());
}

#[test]
fn repeated_import_is_idempotent_and_remove_uses_the_published_action() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    app.invoke(IMPORT.with(REPORT.to_owned())).unwrap();
    app.invoke(IMPORT.with(REPORT.to_owned())).unwrap();
    assert_eq!(app.parsed().reports.len(), 1);
    app.invoke(REMOVE.with(crate::model::report_id(REPORT)))
        .unwrap();
    assert!(app.parsed().reports.is_empty());
}

#[test]
fn forged_storage_and_moving_revision_are_refused_on_the_production_door() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    for (field, value) in [
        ("packed_weight_bytes", serde_json::json!(1)),
        ("revision", serde_json::json!("main")),
    ] {
        let mut report: serde_json::Value = serde_json::from_str(REPORT).unwrap();
        report.as_object_mut().unwrap().insert(field.into(), value);
        // Exercise the typed apply door too: a caller cannot bypass validation
        // by encoding an action instead of using the named declaration.
        let invoke = ChipProject::IMPORT_REPORT.with(report.to_string());
        assert!(app.invoke(invoke).is_err());
        assert!(app.parsed().reports.is_empty());
    }
}

#[test]
fn unsupported_cache_stays_unknown_and_is_never_displayed_as_zero() {
    let mut report: serde_json::Value = serde_json::from_str(REPORT).unwrap();
    report.as_object_mut().unwrap().insert("full_attention_cache".into(),
        serde_json::json!({"status":"unsupported", "reason":"specialized cache formula unverified"}));
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    app.invoke(IMPORT.with(report.to_string())).unwrap();
    assert!(matches!(
        app.parsed()
            .reports
            .values()
            .next()
            .unwrap()
            .full_attention_cache,
        crate::AttentionCache::Unsupported { .. }
    ));
}

#[test]
fn captures_use_the_production_surface_and_never_seed_live_records() {
    for scenario in [
        "empty",
        "storage-report",
        "checkpoint-verification",
        "research-mapping",
        "q4k-superblock",
    ] {
        let mut app = TestApp::<ChipDesignShell>::capture(Some(scenario)).unwrap();
        assert_eq!(
            app.parsed().reports.len(),
            usize::from(scenario == "storage-report")
        );
        assert_eq!(
            app.parsed().verifications.len(),
            usize::from(matches!(
                scenario,
                "checkpoint-verification" | "research-mapping" | "q4k-superblock"
            ))
        );
        let audit = app.harness().native_app_audit();
        assert!(audit.is_certified(), "{scenario}: {audit:#?}");
    }
    let live = TestApp::<ChipDesignShell>::new().unwrap().parsed();
    assert!(live.reports.is_empty());
    assert!(live.verifications.is_empty());
}

#[test]
fn consented_file_import_and_cancellation_use_the_framework_job_path() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("storage-report.json");
    std::fs::write(&path, REPORT).unwrap();
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(path));
    app.effect(&crate::view::ChipEffect::ImportFile).unwrap();
    app.settle();
    assert_eq!(app.parsed().reports.len(), 1);
    assert_eq!(recorder.pick_requests().len(), 1);

    recorder.respond_to_pick(ui::portal::PortalOutcome::Cancelled);
    assert!(app.effect(&crate::view::ChipEffect::ImportFile).is_err());
    app.settle();
    assert_eq!(
        app.parsed().reports.len(),
        1,
        "cancelled jobs fabricate no evidence"
    );
}

#[test]
fn oversized_annotations_are_refused_before_building_report_widgets() {
    let mut report: serde_json::Value = serde_json::from_str(REPORT).unwrap();
    report.as_object_mut().unwrap().insert(
        "limitations".into(),
        serde_json::json!(vec!["annotation"; 100]),
    );
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    let invoke = ChipProject::IMPORT_REPORT.with(report.to_string());
    assert!(app.invoke(invoke).is_err());
    assert!(app.parsed().reports.is_empty());
}

#[test]
fn concurrent_first_imports_share_the_initialized_collection_and_both_survive_merge() {
    use ui::automerge::{ROOT, ReadDoc};

    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    let mut peer = app.peer_document();
    let mut second: serde_json::Value = serde_json::from_str(REPORT).unwrap();
    second
        .get_mut("limitations")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!(
            "Independent collaborator comparison report."
        ));
    let second = second.to_string();
    let first_id = crate::model::report_id(REPORT);
    let second_id = crate::model::report_id(&second);
    app.invoke(IMPORT.with(REPORT.to_owned())).unwrap();
    on_peer(&mut peer, ChipProject::IMPORT_REPORT.with(second));
    app.merge_peer(&mut peer).unwrap();
    let state = app.parsed();
    assert_eq!(state.reports.len(), 2);
    assert!(state.reports.contains_key(&first_id));
    assert!(state.reports.contains_key(&second_id));
    assert_eq!(
        ProjectState::of(&ChipProject::read(&peer).unwrap()),
        state,
        "both replicas converge"
    );
    assert_eq!(
        app.read_document(|doc| doc.get_all(ROOT, "reports").unwrap().len()),
        1,
        "the first imports must not create conflicting map objects"
    );
}

#[test]
fn report_children_publish_inspection_removal_and_typed_reads_on_the_production_door() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    app.invoke(IMPORT.with(REPORT.to_owned())).unwrap();
    let id = crate::model::report_id(REPORT);
    let child = crate::resource::report_resource_id(&id);
    let surface = app.agent_surface();
    for action in ["inspect", "remove-report"] {
        assert!(
            surface
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["resource"] == child && row["action"] == action),
            "report child offers {action}: {surface}"
        );
    }
    app.call_on(&child, "inspect", serde_json::json!({}))
        .unwrap();
    let content = app
        .call("read", serde_json::json!({"item": child}))
        .unwrap()
        .unwrap();
    assert_eq!(content["model"], "Synthetic/Contract-Example");
    assert_eq!(content["packed_weight_bytes"], 512_u64);
    app.call_on(&child, "remove-report", serde_json::json!({}))
        .unwrap();
    assert!(app.parsed().reports.is_empty());
    assert!(
        app.call_on(&child, "inspect", serde_json::json!({}))
            .is_err(),
        "a stale child is refused"
    );
}

const IMPORT_VERIFICATION: ActionRef<ChipProject, crate::resource::project::ImportVerification> =
    ChipProject::IMPORT_VERIFICATION;

fn receipt(evidence: &str, passed: bool) -> serde_json::Value {
    let digest = "a".repeat(64);
    let mut value = serde_json::json!({
        "schema_version": 1, "evidence": evidence,
        "status": if passed { "passed" } else { "failed" },
        "checks": {}, "declared_toolchain_image": null,
        "tools": {}, "artifact_sha256": {}, "excludes": [],
    });
    if evidence == "checkpoint_tile_integer_contract" {
        value["verifier_sha256"] = serde_json::json!(digest);
        value["input_directory"] = serde_json::json!("/private/device/checkpoint");
        value["run_directory"] = serde_json::json!("/private/device/run");
        value["input_validation"] = serde_json::json!(if passed { "passed" } else { "failed" });
        value["excludes"] = serde_json::json!([
            "full_checkpoint_integrity",
            "group_rescaling",
            "activation_quantization",
            "full_model_execution",
            "coding_quality",
            "physical_ppa"
        ]);
        if passed {
            value["synthetic_activations"] = serde_json::json!(true);
            value["artifact_validator_sha256"] = serde_json::json!(digest);
            value["sample_manifest_sha256"] = serde_json::json!(digest);
            value["checks"] = serde_json::json!({"generic_synthesis":"passed",
                "netlist_vs_integer_oracle":"passed", "rtl_vs_integer_oracle":"passed"});
        } else {
            value["failure_stage"] = serde_json::json!("input_validation");
        }
    } else {
        value["synthetic_coefficients"] = serde_json::json!(true);
        value["lanes"] = serde_json::json!(4);
        value["group_size"] = serde_json::json!(128);
        value["input_sha256"] = serde_json::json!({
            "rtl/integer_group_tile.sv":digest, "integer_reference.py":digest, "verify_tile.py":digest,
            "generated/fixed_linear_fixture.sv":digest, "generated/tb.sv":digest, "generated/vectors.json":digest,
        });
        value["excludes"] = serde_json::json!([
            "model_quality",
            "checkpoint_weight_validation",
            "group_rescaling",
            "full_model_execution",
            "process_mapping",
            "physical_area",
            "power",
            "timing_closure"
        ]);
        if passed {
            value["checks"] = serde_json::json!({"generic_synthesis":"passed",
                "rtl_vs_integer_oracle":"passed", "synthesized_netlist_vs_integer_oracle":"passed"});
        }
    }
    if passed {
        value["tools"] = serde_json::json!({
            "verilator":{"version":"test Verilator identity", "executable_sha256":digest},
            "yosys":{"version":"test Yosys identity", "executable_sha256":digest},
        });
        value["artifact_sha256"] = serde_json::json!({"synthesized.v":digest});
    } else {
        value["failure"] = if evidence == "checkpoint_tile_integer_contract" {
            serde_json::json!("Reported input failed at /private/device/checkpoint/sample.json")
        } else {
            serde_json::json!("Reported tool execution failed")
        };
    }
    value
}

#[test]
fn hardware_receipts_publish_typed_checks_identities_and_sanitized_shared_reads() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    for evidence in ["checkpoint_tile_integer_contract", "rtl_contract_check"] {
        for passed in [true, false] {
            let raw = receipt(evidence, passed).to_string();
            app.invoke(IMPORT_VERIFICATION.with(raw.clone())).unwrap();
            let id = crate::VerificationRecord::import(&raw).unwrap().0;
            let child = crate::resource::verification_resource_id(&id);
            app.call_on(&child, "inspect", serde_json::json!({}))
                .unwrap();
            let content = app
                .call("read", serde_json::json!({"item":child}))
                .unwrap()
                .unwrap();
            assert_eq!(
                content["imported_bytes_blake3"],
                crate::model::report_id(&raw)
            );
            assert_eq!(
                content["receipt"]["status"],
                if passed { "passed" } else { "failed" }
            );
            assert_eq!(content["receipt"]["evidence"], evidence);
            assert!(!content.to_string().contains("/private/device"));
            assert!(
                !app.state().verifications[&id].0.contains("/private/device"),
                "the stored receipt holds no device path"
            );
            if passed {
                assert_eq!(content["receipt"]["checks"]["generic_synthesis"], "passed");
                assert_eq!(
                    content["receipt"]["tools"]["yosys"]["executable_sha256"],
                    "a".repeat(64)
                );
            }
            let audit = app.harness().native_app_audit();
            assert!(
                audit.is_certified(),
                "receipt UI/action certification: {audit:#?}"
            );
        }
    }
    assert_eq!(app.parsed().verifications.len(), 4);
    let id = app.parsed().verifications.keys().next().unwrap().clone();
    let child = crate::resource::verification_resource_id(&id);
    app.call_on(&child, "remove-verification", serde_json::json!({}))
        .unwrap();
    assert_eq!(app.parsed().verifications.len(), 3);
    assert!(
        app.call_on(&child, "inspect", serde_json::json!({}))
            .is_err()
    );
    assert!(app.undo());
    assert_eq!(app.parsed().verifications.len(), 4);
}

#[test]
fn receipt_validation_refuses_false_success_malformed_identities_and_dropped_exclusions() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    let good = receipt("checkpoint_tile_integer_contract", true);
    let mut invalid = Vec::new();
    for (key, replacement) in [
        ("status", serde_json::json!("running")),
        ("checks", serde_json::json!({"generic_synthesis":"passed"})),
        ("verifier_sha256", serde_json::json!("moving-source")),
        (
            "artifact_validator_sha256",
            serde_json::json!("invalid-validator-identity"),
        ),
        ("excludes", serde_json::json!(["coding_quality"])),
        ("synthetic_activations", serde_json::json!(false)),
        (
            "declared_toolchain_image",
            serde_json::json!("image:latest"),
        ),
        (
            "failure",
            serde_json::json!("success cannot include failure"),
        ),
    ] {
        let mut candidate = good.clone();
        candidate[key] = replacement;
        invalid.push(candidate.to_string());
    }
    let mut candidate = good.clone();
    candidate["tools"]["verilator"]["executable_sha256"] = serde_json::json!("bad");
    invalid.push(candidate.to_string());
    let mut candidate = good.clone();
    candidate["silicon_area"] = serde_json::json!(1);
    invalid.push(candidate.to_string());
    let mut candidate = receipt("checkpoint_tile_integer_contract", false);
    candidate["checks"] = serde_json::json!({"generic_synthesis":"passed"});
    invalid.push(candidate.to_string());
    invalid.push(good.to_string().replace(
        "\"checks\":{",
        "\"checks\":{\"generic_synthesis\":\"failed\",",
    ));
    invalid.push(" ".repeat(crate::model::MAX_REPORT_BYTES + 1));
    for raw in invalid {
        let action = ChipProject::IMPORT_VERIFICATION.with(raw);
        assert!(
            app.invoke(action).is_err(),
            "typed apply must validate receipts too"
        );
        assert!(app.parsed().verifications.is_empty());
    }
}

#[test]
fn receipt_first_imports_merge_idempotently_and_keep_original_byte_identity() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    let mut peer = app.peer_document();
    let first = receipt("checkpoint_tile_integer_contract", true).to_string();
    let second = receipt("checkpoint_tile_integer_contract", false).to_string();
    app.invoke(IMPORT_VERIFICATION.with(first.clone())).unwrap();
    app.invoke(IMPORT_VERIFICATION.with(first)).unwrap();
    on_peer(&mut peer, ChipProject::IMPORT_VERIFICATION.with(second));
    app.merge_peer(&mut peer).unwrap();
    assert_eq!(app.parsed().verifications.len(), 2);
    assert_eq!(
        ProjectState::of(&ChipProject::read(&peer).unwrap()),
        app.parsed()
    );
}

#[test]
fn consented_receipt_file_import_uses_the_existing_bounded_job() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("verification.json");
    std::fs::write(
        &path,
        receipt("checkpoint_tile_integer_contract", false).to_string(),
    )
    .unwrap();
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(path));
    app.effect(&crate::view::ChipEffect::ImportFile).unwrap();
    app.settle();
    assert_eq!(app.parsed().verifications.len(), 1);
    assert!(app.parsed().reports.is_empty());
    assert_eq!(
        recorder.pick_requests()[0].contents_byte_limit(),
        Some(crate::model::MAX_REPORT_BYTES)
    );
}

#[test]
fn cancelled_checkpoint_receipt_retains_terminal_status_and_no_unrun_checks() {
    let mut raw = receipt("checkpoint_tile_integer_contract", false);
    raw["status"] = serde_json::json!("cancelled");
    raw["failure"] = serde_json::json!("");
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    app.invoke(IMPORT_VERIFICATION.with(raw.to_string()))
        .unwrap();
    let state = app.parsed();
    let record = state.verifications.values().next().unwrap();
    assert_eq!(record.receipt.status, crate::ReceiptStatus::Cancelled);
    assert!(record.receipt.checks.is_empty());
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "cancelled receipt: {audit:#?}");
}

#[test]
fn a_project_holding_only_reports_imports_receipts() {
    let mut project = Replica::<ChipProject>::new();
    project
        .apply(ChipProject::IMPORT_REPORT.with(REPORT))
        .unwrap();
    for passed in [true, false] {
        project
            .apply(
                ChipProject::IMPORT_VERIFICATION
                    .with(receipt("checkpoint_tile_integer_contract", passed).to_string()),
            )
            .unwrap();
    }
    let state = ProjectState::of(&project.state().unwrap());
    assert_eq!(state.reports.len(), 1);
    assert_eq!(state.verifications.len(), 2);
}

#[test]
fn synthetic_mapping_contract_is_visible_through_the_semantic_resource() {
    let raw = include_str!("../fixtures/research-mapping.json");
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    app.invoke(IMPORT_VERIFICATION.with(raw.to_owned()))
        .unwrap();
    let id = crate::VerificationRecord::import(raw).unwrap().0;
    let child = crate::resource::verification_resource_id(&id);
    app.call_on(&child, "inspect", serde_json::json!({}))
        .unwrap();
    let content = app
        .call("read", serde_json::json!({"item":child}))
        .unwrap()
        .unwrap();
    assert_eq!(content["receipt"]["mapped_cell_count"], 100);
    assert_eq!(content["receipt"]["reported_library_area"], 123.5);
    assert_eq!(content["receipt"]["manufacturable"], false);
    assert_eq!(
        content["receipt"]["checks"]["mapped_netlist_vs_integer_oracle"],
        "passed"
    );
    let audit = app.harness().native_app_audit();
    assert!(
        audit.is_certified(),
        "research mapping semantic surface: {audit:#?}"
    );
    assert!(app.undo());
    assert!(app.parsed().verifications.is_empty());
}

#[test]
fn mapping_claims_refuse_manufacturing_false_area_and_missing_observations() {
    let original: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/research-mapping.json")).unwrap();
    let mut cases = Vec::new();
    for (key, value) in [
        ("manufacturable", serde_json::json!(true)),
        ("reported_library_area", serde_json::json!(-1)),
        ("mapped_cell_count", serde_json::json!(0)),
        ("abc_delay_target_ps", serde_json::json!(0)),
    ] {
        let mut changed = original.clone();
        changed[key] = value;
        cases.push(changed);
    }
    let mut changed = original.clone();
    changed["checks"]
        .as_object_mut()
        .unwrap()
        .remove("library_mapping");
    cases.push(changed);
    let mut changed = original;
    changed["input_sha256"]
        .as_object_mut()
        .unwrap()
        .remove("LICENSE");
    cases.push(changed);
    let mut changed = receipt("checkpoint_tile_integer_contract", true);
    changed["reported_library_area"] = serde_json::json!(123.5);
    cases.push(changed);
    for raw in cases {
        let mut app = TestApp::<ChipDesignShell>::new().unwrap();
        let action = ChipProject::IMPORT_VERIFICATION.with(raw.to_string());
        assert!(app.invoke(action).is_err());
        assert!(app.parsed().verifications.is_empty());
    }
}

#[test]
fn mapping_library_and_delay_are_declared_inputs_with_strict_bounds() {
    let mut raw: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/research-mapping.json")).unwrap();
    let inputs = raw["input_sha256"].as_object_mut().unwrap();
    let hash = inputs.remove("synthetic_standard_cells.lib").unwrap();
    inputs.insert("another_declared_library.LIB".into(), hash);
    raw["abc_delay_target_ps"] = 2500.into();
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    app.invoke(IMPORT_VERIFICATION.with(raw.to_string()))
        .unwrap();
    let record = app.parsed().verifications.values().next().unwrap().clone();
    assert_eq!(record.receipt.abc_delay_target_ps, Some(2500));
    assert!(
        record
            .receipt
            .input_sha256
            .unwrap()
            .contains_key("another_declared_library.LIB")
    );
    let mut duplicate = raw.clone();
    duplicate["input_sha256"]["second_library.lib"] = "a".repeat(64).into();
    let mut absent = raw.clone();
    absent["input_sha256"]
        .as_object_mut()
        .unwrap()
        .remove("another_declared_library.LIB");
    let mut oversized = raw;
    oversized["abc_delay_target_ps"] = 1_000_000_001u64.into();
    for invalid in [duplicate, absent, oversized] {
        let mut rejected = TestApp::<ChipDesignShell>::new().unwrap();
        assert!(
            rejected
                .invoke(IMPORT_VERIFICATION.with(invalid.to_string()))
                .is_err()
        );
        assert!(rejected.parsed().verifications.is_empty());
    }
}

#[test]
fn rms_dimensions_are_declared_and_all_observation_counts_must_match() {
    for width in [3u64, 5120, 65536] {
        let mut raw: serde_json::Value = serde_json::from_str(CPU_RECEIPTS[0]).unwrap();
        raw["sample"]["tensor"]["elements"] = width.into();
        raw["sample"]["tensor"]["shape_ggml_order"] = serde_json::json!([width]);
        raw["sample"]["tensor"]["payload_bytes"] = (width * 4).into();
        raw["sample"]["weight_bytes"] = (width * 4).into();
        for case in raw["comparisons"].as_array_mut().unwrap() {
            case["fp32_comparisons"] = width.into();
        }
        let mut app = TestApp::<ChipDesignShell>::new().unwrap();
        app.invoke(IMPORT_VERIFICATION.with(raw.to_string()))
            .unwrap();
        let mut count = raw.clone();
        count["comparisons"][0]["fp32_comparisons"] = (width - 1).into();
        let mut shape = raw.clone();
        shape["sample"]["tensor"]["shape_ggml_order"] = serde_json::json!([width, 1]);
        let mut unsupported = raw;
        unsupported["sample"]["tensor"]["elements"] = 65537.into();
        for invalid in [count, shape, unsupported] {
            let mut rejected = TestApp::<ChipDesignShell>::new().unwrap();
            assert!(
                rejected
                    .invoke(IMPORT_VERIFICATION.with(invalid.to_string()))
                    .is_err()
            );
            assert!(rejected.parsed().verifications.is_empty());
        }
    }
}

fn partial_mapping_receipt(stage: &str, cancelled: bool) -> serde_json::Value {
    let mut raw: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/research-mapping.json")).unwrap();
    raw["status"] = serde_json::json!(if cancelled { "cancelled" } else { "failed" });
    raw["failure_stage"] = serde_json::json!(stage);
    raw["failure"] = serde_json::json!(if cancelled {
        ""
    } else {
        "Reported execution failed"
    });
    raw["checks"] = match stage {
        "input_validation" => serde_json::json!({}),
        "mapping" => serde_json::json!({"input_validation":"passed"}),
        _ => serde_json::json!({"input_validation":"passed", "library_mapping":"passed"}),
    };
    if stage == "input_validation" {
        raw.as_object_mut().unwrap().remove("input_sha256");
    }
    if stage == "mapped_simulation" {
        for name in ["mapped-compile.log", "mapped-simulation.log"] {
            raw["artifact_sha256"].as_object_mut().unwrap().remove(name);
        }
    } else {
        raw["tools"] = serde_json::json!({});
        raw["artifact_sha256"] = serde_json::json!({});
        raw.as_object_mut().unwrap().remove("mapped_cell_count");
        raw.as_object_mut().unwrap().remove("reported_library_area");
    }
    raw
}

#[test]
fn mapping_partial_failures_and_cancellations_preserve_only_reported_milestones() {
    for stage in ["input_validation", "mapping", "mapped_simulation"] {
        for cancelled in [false, true] {
            let raw = partial_mapping_receipt(stage, cancelled).to_string();
            let mut app = TestApp::<ChipDesignShell>::new().unwrap();
            app.invoke(IMPORT_VERIFICATION.with(raw.clone())).unwrap();
            let id = crate::VerificationRecord::import(&raw).unwrap().0;
            let child = crate::resource::verification_resource_id(&id);
            let content = app
                .call("read", serde_json::json!({"item":child}))
                .unwrap()
                .unwrap();
            assert_eq!(content["receipt"]["failure_stage"], stage);
            assert_eq!(
                content["receipt"]["status"],
                if cancelled { "cancelled" } else { "failed" }
            );
            assert!(content["receipt"]["checks"]["mapped_netlist_vs_integer_oracle"].is_null());
            assert_eq!(
                content["receipt"]["mapped_cell_count"].is_null(),
                stage != "mapped_simulation"
            );
            assert_eq!(
                content["receipt"]["reported_library_area"].is_null(),
                stage != "mapped_simulation"
            );
            assert!(app.harness().native_app_audit().is_certified());
        }
    }
}

#[test]
fn mapping_partial_claims_require_prior_checks_and_observation_identities() {
    let mut cases = Vec::new();
    for check in ["passed", "failed"] {
        let mut raw = partial_mapping_receipt("mapping", false);
        raw["checks"]["mapped_netlist_vs_integer_oracle"] = serde_json::json!(check);
        cases.push(raw);
    }
    for key in ["tools", "artifact_sha256"] {
        let mut raw = partial_mapping_receipt("mapped_simulation", true);
        raw[key] = serde_json::json!({});
        cases.push(raw);
    }
    for name in ["vectors.json", "synthetic_standard_cells.lib"] {
        let mut raw = partial_mapping_receipt("mapping", false);
        raw["input_sha256"].as_object_mut().unwrap().remove(name);
        cases.push(raw);
    }
    for name in [
        "mapped.json",
        "stat.json",
        "library_cells.v",
        "library_cells.json",
        "map.ys",
        "mapping.log",
        "yosys-version.log",
    ] {
        let mut raw = partial_mapping_receipt("mapped_simulation", false);
        raw["artifact_sha256"].as_object_mut().unwrap().remove(name);
        cases.push(raw);
    }
    for name in ["mapped-compile.log", "mapped-simulation.log"] {
        let mut raw: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/research-mapping.json")).unwrap();
        raw["artifact_sha256"].as_object_mut().unwrap().remove(name);
        cases.push(raw);
    }
    for stage in [
        serde_json::Value::Null,
        serde_json::json!("input_validation"),
        serde_json::json!("mapping"),
    ] {
        let mut raw = partial_mapping_receipt("mapped_simulation", false);
        raw["failure_stage"] = stage;
        if raw["failure_stage"] == "mapping" {
            raw["checks"]["mapped_netlist_vs_integer_oracle"] = serde_json::json!("passed");
        }
        cases.push(raw);
    }
    for name in [
        "/private/device/stat.json",
        "../stat.json",
        "private/../../stat.json",
        "stat\n.json",
    ] {
        let mut raw = partial_mapping_receipt("mapped_simulation", false);
        raw["artifact_sha256"][name] = serde_json::json!("a".repeat(64));
        cases.push(raw);
    }
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    for raw in cases {
        let action = ChipProject::IMPORT_VERIFICATION.with(raw.to_string());
        assert!(
            app.invoke(action).is_err(),
            "unsupported partial claim: {raw}"
        );
        assert!(app.parsed().verifications.is_empty());
    }
}

#[test]
fn mapping_known_device_paths_are_removed_from_collaborative_resource_reads() {
    let mut raw = partial_mapping_receipt("mapped_simulation", false);
    raw["input_directory"] = serde_json::json!("/private/device/input");
    raw["run_directory"] = serde_json::json!("/private/device/run");
    raw["failure"] = serde_json::json!(
        "Reported /private/device/input/sample.json failed; see /private/device/run/mapped-compile.log"
    );
    let raw = raw.to_string();
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    app.invoke(IMPORT_VERIFICATION.with(raw.clone())).unwrap();
    let id = crate::VerificationRecord::import(&raw).unwrap().0;
    let child = crate::resource::verification_resource_id(&id);
    let content = app
        .call("read", serde_json::json!({"item":child}))
        .unwrap()
        .unwrap();
    assert!(!content.to_string().contains("/private/device"));
    assert!(content["receipt"]["input_directory"].is_null());
    assert!(content["receipt"]["run_directory"].is_null());
    assert!(
        content["receipt"]["failure"]
            .as_str()
            .unwrap()
            .contains("<run directory>/mapped-compile.log")
    );
    assert!(
        !app.state().verifications[&id].0.contains("/private/device"),
        "the stored receipt holds no device path"
    );
}

#[test]
fn synthetic_superblock_contract_is_shared_without_losing_scalar_scope() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    let json = include_str!("../fixtures/q4k-superblock.json");
    app.call("import-verification", serde_json::json!({"json":json}))
        .unwrap();
    let id = app.parsed().verifications.keys().next().unwrap().clone();
    let child = crate::resource::verification_resource_id(&id);
    app.call_on(&child, "inspect", serde_json::json!({}))
        .unwrap();
    let content = app
        .call("read", serde_json::json!({"item":child}))
        .unwrap()
        .unwrap();
    assert_eq!(content["receipt"]["generic_cell_count"], 9940);
    assert_eq!(content["receipt"]["external_scalar_C_comparisons"], 32);
    assert_eq!(
        content["receipt"]["tools"]["gcc"]["executable_sha256"],
        "1f28c67c201081aaf051bdf1efdfa559f98b4ec4ddcb6f6f236ac5cd6c1f7db6"
    );
    assert_eq!(
        content["receipt"]["source_sha256"]["harness"],
        "53d12021f98728cd7e92147cecf8b7c2dc5d69d74467b912e33d46b1b1f8c7bc"
    );
    assert!(
        content["receipt"]["excludes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "FP32_rescaler_RTL")
    );
    let audit = app.harness().native_app_audit();
    assert!(
        audit.is_certified(),
        "superblock UI/action certification: {audit:#?}"
    );
}

#[test]
fn superblock_receipts_refuse_incomplete_or_contradictory_success() {
    let good: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/q4k-superblock.json")).unwrap();
    let mut cases = Vec::new();
    let mut raw = good.clone();
    raw["primary_source_sha256"]["ggml-common.h"] = "a".repeat(64).into();
    cases.push(raw);
    let mut raw = good.clone();
    raw["primary_source_sha256"]
        .as_object_mut()
        .unwrap()
        .remove("cpu-quants.c");
    cases.push(raw);
    let mut raw = good.clone();
    raw["generic_cell_count"] = 1.into();
    cases.push(raw);
    let mut raw = good.clone();
    raw["external_scalar_C_comparisons"] = 31.into();
    cases.push(raw);
    let mut raw = good.clone();
    raw["checks"]
        .as_object_mut()
        .unwrap()
        .remove("pinned_primary_source");
    cases.push(raw);
    let mut raw = good.clone();
    raw["tools"].as_object_mut().unwrap().remove("gcc");
    cases.push(raw);
    let mut raw = good.clone();
    raw["artifact_sha256"]
        .as_object_mut()
        .unwrap()
        .remove("c-output.bin");
    cases.push(raw);
    let mut raw = good.clone();
    raw["sample_sha256"] = "0".repeat(64).into();
    cases.push(raw);
    let mut raw = good.clone();
    raw["compiler_flags"][3] = "-ffast-math".into();
    cases.push(raw);
    let mut raw = good.clone();
    raw["excludes"].as_array_mut().unwrap().remove(0);
    cases.push(raw);
    let mut raw = good.clone();
    raw["mapped_cell_count"] = 9940.into();
    cases.push(raw);
    let mut raw = good;
    raw["status"] = "failed".into();
    raw["error"] = "/private/path/failure".into();
    cases.push(raw);
    for raw in cases {
        let mut app = TestApp::<ChipDesignShell>::new().unwrap();
        assert!(
            app.call(
                "import-verification",
                serde_json::json!({"json":raw.to_string()})
            )
            .is_err()
        );
        assert!(app.parsed().verifications.is_empty());
    }
    let duplicate = include_str!("../fixtures/q4k-superblock.json").replace(
        r#""$_ANDNOT_": 2367"#,
        r#""$_ANDNOT_": 2367, "$_ANDNOT_": 2367"#,
    );
    assert!(crate::VerificationRecord::import(&duplicate).is_err());
    let mut foreign = receipt("checkpoint_tile_integer_contract", true);
    foreign["generic_cell_count"] = 9940.into();
    assert!(crate::VerificationRecord::import(&foreign.to_string()).is_err());
}

const CPU_RECEIPTS: [&str; 4] = [
    include_str!("../fixtures/operators/rms-norm.json"),
    include_str!("../fixtures/operators/silu.json"),
    include_str!("../fixtures/operators/gated-delta.json"),
    include_str!("../fixtures/operators/softmax.json"),
];

#[test]
fn cpu_operator_receipts_use_the_public_import_read_open_and_reopen_door() {
    use ui::search::SearchProvider as _;
    let expected_hashes = [
        "47d47caf9ce1a08e1146eea8531a591dcd95010ef8edab7617d6743ba9e90eb2",
        "40ad8f94c56b2c46d14dc737536d05391302154d48b1207df872ddb755868346",
        "11c4a70d543b904af791dc1e9e52b5dcf638b74b2d0f356c0b872897854a1f97",
        "1daac51863ed5455eb407260ef6c3d7961effb44aaef66c940987609ead9702e",
    ];
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    assert!(app.parsed().verifications.is_empty());
    for (json, expected_hash) in CPU_RECEIPTS.into_iter().zip(expected_hashes) {
        app.call("import-verification", serde_json::json!({"json":json}))
            .unwrap();
        let (id, stored) = crate::VerificationRecord::import(json).unwrap();
        let record = crate::VerificationRecord::read(&stored).unwrap();
        let summary = record.receipt.operator_summary.as_ref().unwrap();
        assert_eq!(summary.original_receipt_sha256, expected_hash);
        assert_eq!(
            record.imported_bytes_blake3,
            blake3::hash(json.as_bytes()).to_hex().as_str()
        );
        assert!(!summary.primary_source_sha256.is_empty());
        assert!(!summary.local_source_sha256.is_empty());
        assert!(!stored.contains("/lib64/"));
        let child = crate::resource::verification_resource_id(&id);
        let content = app
            .call("read", serde_json::json!({"item":child}))
            .unwrap()
            .unwrap();
        assert_eq!(
            content["receipt"]["operator_summary"]["original_receipt_sha256"],
            summary.original_receipt_sha256
        );
        app.call_on(&child, "inspect", serde_json::json!({}))
            .unwrap();
        let entries = <ChipProject as ui::tilo_resource::Resource>::search(&app.state());
        let provider = ui::framework::testing::search_source::<ChipProject>(entries);
        let hits = provider.search(&record.receipt.evidence);
        assert!(hits.iter().any(|hit| hit.title == record.receipt.evidence));

        let audit = app.harness().native_app_audit();
        assert!(
            audit.is_certified(),
            "CPU receipt public surface: {audit:#?}"
        );
        app.call("import-verification", serde_json::json!({"json":json}))
            .unwrap();
    }
    assert_eq!(app.parsed().verifications.len(), 4);
    let expected = app.parsed().verifications;
    let reopened = app.reopen().unwrap();
    assert_eq!(reopened.parsed().verifications, expected);
}

#[test]
fn cpu_operator_receipts_reject_malformed_or_contradictory_public_claims() {
    let mut cases = Vec::new();
    for json in CPU_RECEIPTS {
        let good: serde_json::Value = serde_json::from_str(json).unwrap();
        for (name, value) in [
            ("status", serde_json::json!("failed")),
            ("primary_revision", serde_json::json!("main")),
            ("compiler_executable_sha256", serde_json::json!("bad")),
            (
                "compiler_version",
                serde_json::json!("/private/secret/compiler"),
            ),
            ("error", serde_json::json!("/private/secret/error")),
            ("comparisons", serde_json::json!([])),
        ] {
            let mut raw = good.clone();
            raw[name] = value;
            cases.push(raw.to_string());
        }
        let mut raw = good.clone();
        raw["excludes"].as_array_mut().unwrap().pop();
        cases.push(raw.to_string());
        let mut raw = good.clone();
        raw["compiler_flags"]
            .as_array_mut()
            .unwrap()
            .push("-ffast-math".into());
        cases.push(raw.to_string());
        let mut raw = good.clone();
        let first = raw["checks"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();
        raw["checks"].as_object_mut().unwrap().remove(&first);
        cases.push(raw.to_string());
        // Preserve duplicate JSON keys in the actual input, not a Value map that
        // has already discarded the malformed producer identity.
        cases.push(json.replacen(
            "\"primary_revision\":",
            "\"primary_revision\":\"main\",\"primary_revision\":",
            1,
        ));
    }
    cases.push(CPU_RECEIPTS[1].replacen(
        "\"arithmetic\":",
        &format!("\"arithmetic\":\"{}\",\"arithmetic\":", "0".repeat(64)),
        1,
    ));
    let mut library: serde_json::Value = serde_json::from_str(CPU_RECEIPTS[1]).unwrap();
    library["observed_linked_library_sha256"]["/private/device/libm.so.6"] = "0".repeat(64).into();
    cases.push(library.to_string());
    let mut norm: serde_json::Value = serde_json::from_str(CPU_RECEIPTS[0]).unwrap();
    norm["sample"]["tensor"]["payload_bytes"] = 1.into();
    cases.push(norm.to_string());
    let mut silu: serde_json::Value = serde_json::from_str(CPU_RECEIPTS[1]).unwrap();
    silu["comparisons"]["total_inputs"] = 1.into();
    cases.push(silu.to_string());
    let mut delta: serde_json::Value = serde_json::from_str(CPU_RECEIPTS[2]).unwrap();
    delta["comparisons"][0]["state_words"] = 1.into();
    cases.push(delta.to_string());
    let mut softmax: serde_json::Value = serde_json::from_str(CPU_RECEIPTS[3]).unwrap();
    softmax["comparisons"][0]["observed_probability_sum_fraction"] = "1/0".into();
    cases.push(softmax.to_string());
    let mut softmax: serde_json::Value = serde_json::from_str(CPU_RECEIPTS[3]).unwrap();
    softmax["comparisons"][0]["primary_sha256"] = "0".repeat(64).into();
    cases.push(softmax.to_string());
    let mut silu: serde_json::Value = serde_json::from_str(CPU_RECEIPTS[1]).unwrap();
    silu["artifact_sha256"]
        .as_object_mut()
        .unwrap()
        .remove("oracle");
    cases.push(silu.to_string());
    let mut delta: serde_json::Value = serde_json::from_str(CPU_RECEIPTS[2]).unwrap();
    delta["artifact_sha256"]["case-0-reference.f32"] = "0".repeat(64).into();
    cases.push(delta.to_string());
    let (_, envelope) = crate::VerificationRecord::import(CPU_RECEIPTS[0]).unwrap();
    let record: serde_json::Value = serde_json::from_str(&envelope).unwrap();
    let mut foreign = record["receipt"].clone();
    foreign["input_directory"] = "/private/device".into();
    cases.push(foreign.to_string());
    for json in cases {
        let mut app = TestApp::<ChipDesignShell>::new().unwrap();
        assert!(
            app.call("import-verification", serde_json::json!({"json":json}))
                .is_err()
        );
        assert!(app.parsed().verifications.is_empty());
    }
}

#[test]
fn generic_gated_delta_dimensions_and_candidate_artifacts_are_consistent() {
    const RAW: &str = include_str!("../fixtures/operators/gated-delta-geometry.json");
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    app.invoke(IMPORT_VERIFICATION.with(RAW.to_owned()))
        .unwrap();
    let state = app.parsed();
    let record = state.verifications.values().next().unwrap();
    let summary = record.receipt.operator_summary.as_ref().unwrap();
    assert_eq!(
        summary.original_receipt_sha256,
        "96cb30b57302798d5310242d4328cbbaa4c294c9ad292ce0e133988c14aaf65e"
    );
    let crate::verification::Observations::GatedDelta(cases) = &summary.observations else {
        panic!("expected typed GDN observations");
    };
    assert_eq!(cases.len(), 4);
    assert_eq!(cases[3].attention_words, 128);
    assert_eq!(cases[3].state_words, 2048);
    let id = app.parsed().verifications.keys().next().unwrap().clone();
    let item = crate::resource::verification_resource_id(&id);
    let read = app
        .call("read", serde_json::json!({"item":item}))
        .unwrap()
        .unwrap();
    assert_eq!(
        read["receipt"]["operator_summary"]["original_receipt_sha256"],
        "96cb30b57302798d5310242d4328cbbaa4c294c9ad292ce0e133988c14aaf65e"
    );
    let mut absent: serde_json::Value = serde_json::from_str(RAW).unwrap();
    absent["comparisons"].as_array_mut().unwrap().pop().unwrap();
    let mut both: serde_json::Value = serde_json::from_str(RAW).unwrap();
    both["excludes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!("complete_recurrent_block"));
    let mut wrong_target: serde_json::Value = serde_json::from_str(RAW).unwrap();
    wrong_target["comparisons"][3]["config"]["key_heads"] = serde_json::json!(24);
    for raw in [absent, both, wrong_target] {
        let mut rejected = TestApp::<ChipDesignShell>::new().unwrap();
        assert!(
            rejected
                .invoke(IMPORT_VERIFICATION.with(raw.to_string()))
                .is_err()
        );
        assert!(rejected.parsed().verifications.is_empty());
    }
}

/// What an agent does in the project, one call each, reads as the app's
/// sentences.
#[test]
fn an_agent_designs_operators_one_call_each_and_history_reads_it() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let kind = <ChipProject as ui::tilo_resource::Resource>::KIND;
    let mut call = |action: &str, args: serde_json::Value| {
        app.call_by(agent.clone(), kind, action, args).unwrap();
    };
    call("import-report", serde_json::json!({ "json": REPORT }));
    for (node, kind) in [("x", "rms_norm"), ("y", "silu")] {
        call(
            "add-node",
            serde_json::json!({"node":node,"kind":kind,"shape":[4,8],"x":0,"y":0}),
        );
    }
    call(
        "connect",
        serde_json::json!({"from_node":"x","from_port":"result","to_node":"y","to_port":"value"}),
    );
    call(
        "field-edit",
        serde_json::json!({
            "edit": ChipProject::paths().nodes.item(&ui::tilo_resource::Id::from_raw("y")).x.set(280)
        }),
    );
    call("disconnect", serde_json::json!({"node":"y","port":"value"}));
    call("remove-node", serde_json::json!({"node":"x"}));
    let state = app.parsed();
    assert_eq!(state.reports.len(), 1);
    assert_eq!(state.operator_graph.nodes["y"].x, 280);
    assert!(!state.operator_graph.nodes.contains_key("x"));

    let sentences = app.sentences(app.history().iter().filter(|entry| {
        entry
            .invoker
            .as_ref()
            .is_some_and(|invoker| invoker.actor == "agent:claude")
    }));
    assert_eq!(
        sentences,
        [
            "Imported an analytical storage report",
            "Added the operator x",
            "Added the operator y",
            "Connected x to y",
            "Moved an operator",
            "Disconnected the value input of y",
            "Removed the operator x",
        ]
    );
}
