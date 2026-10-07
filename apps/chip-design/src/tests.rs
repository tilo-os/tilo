#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use ui::framework::testing::TestApp;
use ui::tilo_resource::{ActionRef, Invoke};

use crate::{ChipDesignShell, ChipProject, ProjectAction};

const REPORT: &str = include_str!("../fixtures/storage-report.json");
const IMPORT: ActionRef<ChipProject, String> = ActionRef::new("import-report", &["json"]);
const REMOVE: ActionRef<ChipProject, String> = ActionRef::new("remove-report", &["report"]);

#[test]
fn semantic_import_preserves_provenance_and_exact_storage_and_is_undoable() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    assert!(app.state().reports.is_empty(), "live projects start empty");
    app.invoke(IMPORT.with(REPORT.to_owned())).unwrap();
    let state = app.state();
    let id = crate::model::report_id(REPORT);
    let report = state
        .reports
        .get(&id)
        .expect("semantic import published the report");
    assert_eq!(report.model, "Qwen/Qwen3.8-27B");
    assert_eq!(report.revision, "1d4bf0f2ff6012fd82039f2fa52739d0dd7c60c0");
    assert_eq!(report.packed_weight_bytes, 13_500_000_000);
    assert!(
        report
            .not_estimated
            .iter()
            .any(|item| item == "coding_quality")
    );
    assert!(app.undo());
    assert!(app.state().reports.is_empty());
}

#[test]
fn repeated_import_is_idempotent_and_remove_uses_the_published_action() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    app.invoke(IMPORT.with(REPORT.to_owned())).unwrap();
    app.invoke(IMPORT.with(REPORT.to_owned())).unwrap();
    assert_eq!(app.state().reports.len(), 1);
    app.invoke(REMOVE.with(crate::model::report_id(REPORT)))
        .unwrap();
    assert!(app.state().reports.is_empty());
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
        let invoke = Invoke::apply(&ProjectAction::ImportReport {
            json: report.to_string(),
        })
        .unwrap();
        assert!(app.invoke(invoke).is_err());
        assert!(app.state().reports.is_empty());
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
        app.state()
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
    for scenario in ["empty", "storage-report", "checkpoint-verification"] {
        let mut app = TestApp::<ChipDesignShell>::capture(Some(scenario)).unwrap();
        assert_eq!(
            app.state().reports.len(),
            usize::from(scenario == "storage-report")
        );
        assert_eq!(
            app.state().verifications.len(),
            usize::from(scenario == "checkpoint-verification")
        );
        let audit = app.harness().native_app_audit();
        assert!(audit.is_certified(), "{scenario}: {audit:#?}");
    }
    let live = TestApp::<ChipDesignShell>::new().unwrap().state();
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
    assert_eq!(app.state().reports.len(), 1);
    assert_eq!(recorder.pick_requests().len(), 1);

    recorder.respond_to_pick(ui::portal::PortalOutcome::Cancelled);
    assert!(app.effect(&crate::view::ChipEffect::ImportFile).is_err());
    app.settle();
    assert_eq!(
        app.state().reports.len(),
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
    let invoke = Invoke::apply(&ProjectAction::ImportReport {
        json: report.to_string(),
    })
    .unwrap();
    assert!(app.invoke(invoke).is_err());
    assert!(app.state().reports.is_empty());
}

#[test]
fn concurrent_first_imports_share_the_initialized_collection_and_both_survive_merge() {
    use ui::automerge::{ROOT, ReadDoc};
    use ui::tilo_actions::{EncodedAction, LocalTransport};
    use ui::tilo_resource::Resource;

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
    let action = ProjectAction::ImportReport { json: second };
    let encoded = EncodedAction::encode(ChipProject::ACTION_TYPE, &action).unwrap();
    ui::tilo_actions::dispatch(&mut peer, &action, encoded, &mut LocalTransport::new()).unwrap();
    app.merge_peer(&mut peer).unwrap();
    let state = app.state();
    assert_eq!(state.reports.len(), 2);
    assert!(state.reports.contains_key(&first_id));
    assert!(state.reports.contains_key(&second_id));
    assert_eq!(
        ChipProject::read(&peer).unwrap(),
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
    assert_eq!(content["model"], "Qwen/Qwen3.8-27B");
    assert_eq!(content["packed_weight_bytes"], 13_500_000_000_u64);
    app.call_on(&child, "remove-report", serde_json::json!({}))
        .unwrap();
    assert!(app.state().reports.is_empty());
    assert!(
        app.call_on(&child, "inspect", serde_json::json!({}))
            .is_err(),
        "a stale child is refused"
    );
}

const IMPORT_VERIFICATION: ActionRef<ChipProject, String> =
    ActionRef::new("import-verification", &["json"]);

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
            app.read_document(|doc| {
                use ui::automerge::ReadDoc;
                let (value, _) = doc
                    .get(ui::automerge::ROOT, format!("verification:{id}"))
                    .unwrap()
                    .unwrap();
                assert!(!value.as_str().unwrap().contains("/private/device"));
            });
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
    assert_eq!(app.state().verifications.len(), 4);
    let id = app.state().verifications.keys().next().unwrap().clone();
    let child = crate::resource::verification_resource_id(&id);
    app.call_on(&child, "remove-verification", serde_json::json!({}))
        .unwrap();
    assert_eq!(app.state().verifications.len(), 3);
    assert!(
        app.call_on(&child, "inspect", serde_json::json!({}))
            .is_err()
    );
    assert!(app.undo());
    assert_eq!(app.state().verifications.len(), 4);
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
        let action = Invoke::apply(&ProjectAction::ImportVerification { json: raw }).unwrap();
        assert!(
            app.invoke(action).is_err(),
            "typed apply must validate receipts too"
        );
        assert!(app.state().verifications.is_empty());
    }
}

#[test]
fn receipt_first_imports_merge_idempotently_and_keep_original_byte_identity() {
    use ui::tilo_actions::{EncodedAction, LocalTransport};
    use ui::tilo_resource::Resource;
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    let mut peer = app.peer_document();
    let first = receipt("checkpoint_tile_integer_contract", true).to_string();
    let second = receipt("checkpoint_tile_integer_contract", false).to_string();
    app.invoke(IMPORT_VERIFICATION.with(first.clone())).unwrap();
    app.invoke(IMPORT_VERIFICATION.with(first)).unwrap();
    let action = ProjectAction::ImportVerification { json: second };
    let encoded = EncodedAction::encode(ChipProject::ACTION_TYPE, &action).unwrap();
    ui::tilo_actions::dispatch(&mut peer, &action, encoded, &mut LocalTransport::new()).unwrap();
    app.merge_peer(&mut peer).unwrap();
    assert_eq!(app.state().verifications.len(), 2);
    assert_eq!(ChipProject::read(&peer).unwrap(), app.state());
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
    assert_eq!(app.state().verifications.len(), 1);
    assert!(app.state().reports.is_empty());
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
    let state = app.state();
    let record = state.verifications.values().next().unwrap();
    assert_eq!(record.receipt.status, crate::ReceiptStatus::Cancelled);
    assert!(record.receipt.checks.is_empty());
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "cancelled receipt: {audit:#?}");
}

#[test]
fn existing_storage_only_documents_import_receipts_without_schema_migration() {
    use ui::automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable};
    use ui::tilo_actions::{EncodedAction, LocalTransport};
    use ui::tilo_resource::Resource;
    let mut doc = AutoCommit::new();
    let reports = doc.put_object(ROOT, "reports", ObjType::Map).unwrap();
    doc.put(reports, crate::model::report_id(REPORT), REPORT)
        .unwrap();
    doc.commit();
    let old_state = ChipProject::read(&doc).unwrap();
    assert_eq!(old_state.reports.len(), 1);
    assert!(old_state.verifications.is_empty());
    for passed in [true, false] {
        let action = ProjectAction::ImportVerification {
            json: receipt("checkpoint_tile_integer_contract", passed).to_string(),
        };
        let encoded = EncodedAction::encode(ChipProject::ACTION_TYPE, &action).unwrap();
        ui::tilo_actions::dispatch(&mut doc, &action, encoded, &mut LocalTransport::new()).unwrap();
    }
    let state = ChipProject::read(&doc).unwrap();
    assert_eq!(state.reports.len(), 1);
    assert_eq!(state.verifications.len(), 2);
}
