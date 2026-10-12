//! Public resource and retained-view checks of model-independent staged evidence.
use crate::tests::Parsed as _;
use crate::{ChipDesignShell, VerificationRecord};
use serde_json::{Value, json};
use ui::framework::testing::TestApp;

fn example(stage: &str) -> Value {
    let hash = "a".repeat(64);
    let link = json!({"receipt_sha256":hash,"producer_sha256":hash,"input_identity_sha256":hash});
    let names: &[&str] = match stage {
        "prepare" => &[],
        "numerical" | "synthesis" => &["prepare"],
        "fault" => &["prepare", "numerical"],
        _ => &["prepare", "numerical", "fault", "synthesis"],
    };
    let checks: &[&str] = match stage {
        "prepare" => &["input_identity_recorded"],
        "numerical" => &["all_variants_complete_outputs", "protocol_controls"],
        "fault" => &["wrong_coefficient_rejected", "wrong_row_rejected"],
        "synthesis" => &["generic_structure_observed"],
        _ => &["linked_stages_match_inputs"],
    };
    let numerical = json!({"cold_program_cycles":400,"vector_load_cycles":200,
        "warm_cycles":3000,"accepted_weight_bytes":256,"accepted_descriptor_bytes":0,
        "cache_reads":64,"quantizer_packets":2,"comparison_words":8,"output_sha256":hash});
    let synthesis = json!({"generic_logic_cells":100,"activation_cache_bits":1024,
        "programmed_weight_bits":2048,"fixed_initialized_bits":0,
        "licensed_memory_macro":false,"synthesized_netlist_equivalence":false});
    let variants =
        if matches!(stage, "numerical" | "synthesis" | "aggregate") {
            ["streamed", "resident"].into_iter().map(|identity| json!({
            "identity":identity,"numerical":(stage != "synthesis").then_some(&numerical),
            "synthesis":(stage != "numerical").then_some(&synthesis)
        })).collect()
        } else {
            vec![]
        };
    json!({"schema_version":1,"evidence":"architecture_study","status":"passed",
        "checks":checks.iter().map(|name| ((*name).to_owned(), "passed")).collect::<std::collections::BTreeMap<_,_>>(),
        "declared_toolchain_image":format!("sha256:{hash}"),
        "tools":{"g++":{"version":"synthetic compiler identity","executable_sha256":hash}},
        "artifact_sha256":{"stage-summary.json":hash},
        "excludes":["full_model_execution","coding_quality","physical_ppa","manufacturing",
            "payload_authentication","synthesized_netlist_equivalence"],
        "architecture_study":{"stage":stage,"original_receipt_sha256":hash,
            "producer_sha256":hash,"input_identity_sha256":hash,"expected_comparison_words":8,
            "links":names.iter().map(|name| ((*name).to_owned(),link.clone())).collect::<std::collections::BTreeMap<_,_>>(),
            "variants":variants}})
}

#[test]
fn public_import_inspect_and_read_preserve_reported_comparison_and_native_view() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    let raw = example("aggregate").to_string();
    app.call("import-verification", json!({"json":raw}))
        .unwrap();
    let id = VerificationRecord::import(&raw).unwrap().0;
    let child = crate::resource::verification_resource_id(&id);
    app.call_on(&child, "inspect", json!({})).unwrap();
    let content = app.call("read", json!({"item":child})).unwrap().unwrap();
    assert_eq!(
        content["receipt"]["architecture_study"]["stage"],
        "aggregate"
    );
    assert_eq!(
        content["receipt"]["architecture_study"]["variants"][0]["numerical"]["warm_cycles"],
        3000
    );
    assert_eq!(
        content["receipt"]["architecture_study"]["variants"][1]["synthesis"]["licensed_memory_macro"],
        false
    );
    app.harness().resize((1600, 1400)).unwrap();
    app.settle();
    let rendered = app.harness().inspection_json().unwrap();
    for key in [
        "chip-study-disclaimer",
        "chip-study-warm-cycles",
        "chip-study-programmed-bits",
    ] {
        let expected = ui::i18n::I18nManager::lookup_plain(key);
        assert!(
            rendered.contains(&serde_json::to_string(&expected).unwrap()),
            "missing retained property {key}"
        );
    }
    assert!(app.undo());
    assert!(app.parsed().verifications.is_empty());
}

#[test]
fn each_stage_is_distinct_and_failed_partial_numerical_evidence_stays_failed() {
    for stage in ["prepare", "numerical", "fault", "synthesis", "aggregate"] {
        VerificationRecord::import(&example(stage).to_string()).unwrap();
    }
    let mut failed = example("numerical");
    failed["status"] = json!("failed");
    failed["failure"] = json!("second variant failed compilation");
    failed["checks"] = json!({});
    failed["architecture_study"]["variants"]
        .as_array_mut()
        .unwrap()
        .pop();
    let (_, stored) = VerificationRecord::import(&failed.to_string()).unwrap();
    let record: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(record["receipt"]["status"], "failed");
    assert_eq!(
        record["receipt"]["architecture_study"]["variants"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    failed["status"] = json!("passed");
    failed.as_object_mut().unwrap().remove("failure");
    assert!(VerificationRecord::import(&failed.to_string()).is_err());
}

#[test]
fn contradictory_stage_links_outputs_and_physical_claims_refuse_without_mutation() {
    let original = example("aggregate");
    let mut invalid = Vec::new();
    let mut value = original.clone();
    value["architecture_study"]["links"]["prepare"]["input_identity_sha256"] =
        json!("b".repeat(64));
    invalid.push(value);
    let mut value = original.clone();
    value["architecture_study"]["links"]
        .as_object_mut()
        .unwrap()
        .remove("fault");
    invalid.push(value);
    let mut value = original.clone();
    value["architecture_study"]["variants"][1]["numerical"]["output_sha256"] =
        json!("b".repeat(64));
    invalid.push(value);
    let mut value = original.clone();
    value["architecture_study"]["variants"][0]["numerical"]["comparison_words"] = json!(7);
    invalid.push(value);
    let mut value = original.clone();
    value["architecture_study"]["variants"][0]["synthesis"]["licensed_memory_macro"] = json!(true);
    invalid.push(value);
    let mut value = original.clone();
    value["architecture_study"]["variants"][0]["synthesis"]["synthesized_netlist_equivalence"] =
        json!(true);
    invalid.push(value);
    let mut value = original.clone();
    value["architecture_study"]["stage"] = json!("synthesis");
    invalid.push(value);
    let mut value = original.clone();
    value["excludes"] = json!([]);
    invalid.push(value);
    let mut value = original;
    value["manufacturable"] = json!(true);
    invalid.push(value);
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    for value in invalid {
        assert!(
            app.call("import-verification", json!({"json":value.to_string()}))
                .is_err()
        );
        assert!(app.parsed().verifications.is_empty());
    }
}

#[test]
fn duplicate_stage_link_and_oversized_variant_set_are_rejected() {
    let value = example("aggregate");
    let raw = value.to_string().replacen(
        "\"links\":{",
        &format!(
            "\"links\":{{\"prepare\":{},",
            value["architecture_study"]["links"]["prepare"]
        ),
        1,
    );
    assert!(VerificationRecord::import(&raw).is_err());
    let mut value = value;
    let first = value["architecture_study"]["variants"][0].clone();
    value["architecture_study"]["variants"] = json!(vec![first; 17]);
    assert!(VerificationRecord::import(&value.to_string()).is_err());
}

#[test]
fn failed_study_removes_declared_device_paths_before_shared_storage() {
    let mut value = example("prepare");
    value["status"] = json!("failed");
    value["checks"] = json!({});
    value["input_directory"] = json!("/private/input");
    value["run_directory"] = json!("/private/run");
    value["failure"] = json!("read /private/input/model; compiler failed in /private/run/output");
    let (_, stored) = VerificationRecord::import(&value.to_string()).unwrap();
    assert!(!stored.contains("/private/"));
    assert!(stored.contains("<input directory>"));
    assert!(stored.contains("<run directory>"));
    let _: VerificationRecord = VerificationRecord::read(&stored).unwrap();
}

fn memory_report() -> Value {
    let mut report = example("aggregate");
    for (index, ports) in [513, 1].into_iter().enumerate() {
        report["architecture_study"]["variants"][index]["synthesis"]["memories"] = json!([{
            "name":"quantizer.words", "width_bits":32, "depth_words":256,
            "bits":8192, "read_ports":1, "clocked_read_ports":1,
            "write_ports":ports, "fully_initialized":false
        }]);
    }
    report
}

#[test]
fn public_memory_report_preserves_geometry_ports_and_native_properties() {
    let report = memory_report();
    let raw = report.to_string();
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    app.call("import-verification", json!({"json":raw}))
        .unwrap();
    let id = VerificationRecord::import(&raw).unwrap().0;
    let child = crate::resource::verification_resource_id(&id);
    app.call_on(&child, "inspect", json!({})).unwrap();
    let read = app.call("read", json!({"item":child})).unwrap().unwrap();
    for index in 0..2 {
        assert_eq!(
            read["receipt"]["architecture_study"]["variants"][index]["synthesis"]["memories"],
            report["architecture_study"]["variants"][index]["synthesis"]["memories"]
        );
    }
    app.harness().resize((1600, 2600)).unwrap();
    app.settle();
    let inspection = app.harness().inspection_json().unwrap();
    for key in [
        "chip-study-memory-name",
        "chip-study-memory-width",
        "chip-study-memory-depth",
        "chip-study-memory-bits",
        "chip-study-memory-read-ports",
        "chip-study-memory-clocked-reads",
        "chip-study-memory-write-ports",
        "chip-study-memory-initialized",
        "chip-value-no",
    ] {
        let label = ui::i18n::I18nManager::lookup_plain(key);
        assert!(
            inspection.contains(&serde_json::to_string(&label).unwrap()),
            "missing {key}"
        );
    }
    assert!(inspection.contains("quantizer.words"));
    assert!(app.undo());
    assert!(app.parsed().verifications.is_empty());
}

#[test]
fn invalid_memory_geometry_ports_and_inventory_refuse_atomically() {
    let original = memory_report();
    let mut invalid = Vec::new();
    for (field, value) in [
        ("width_bits", json!(0)),
        ("depth_words", json!(0)),
        ("bits", json!(8191)),
        ("depth_words", json!(u64::MAX)),
        ("clocked_read_ports", json!(2)),
        ("write_ports", json!(65537)),
        ("name", json!("bad\nname")),
        ("fully_initialized", json!("false")),
    ] {
        let mut value_report = original.clone();
        value_report["architecture_study"]["variants"][0]["synthesis"]["memories"][0][field] =
            value;
        invalid.push(value_report);
    }
    for count in [2, 33] {
        let mut value = original.clone();
        let memory = value["architecture_study"]["variants"][0]["synthesis"]["memories"][0].clone();
        let memories: Vec<_> = (0..count)
            .map(|index| {
                let mut item = memory.clone();
                if count == 33 {
                    item["name"] = json!(format!("words-{index}"));
                }
                item
            })
            .collect();
        value["architecture_study"]["variants"][0]["synthesis"]["memories"] = json!(memories);
        invalid.push(value);
    }
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    for value in invalid {
        assert!(
            app.call("import-verification", json!({"json":value.to_string()}))
                .is_err()
        );
        assert!(app.parsed().verifications.is_empty());
    }
    // A ROM may have asynchronous reads and no writes.
    let mut rom = original;
    rom["architecture_study"]["variants"][0]["synthesis"]["memories"][0]["clocked_read_ports"] =
        json!(0);
    rom["architecture_study"]["variants"][0]["synthesis"]["memories"][0]["write_ports"] = json!(0);
    rom["architecture_study"]["variants"][0]["synthesis"]["memories"][0]["fully_initialized"] =
        json!(true);
    VerificationRecord::import(&rom.to_string()).unwrap();
}
