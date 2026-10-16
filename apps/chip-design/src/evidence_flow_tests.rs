//! Actual public import, navigation, and refusal paths for detached evidence.
use crate::tests::Parsed as _;
use crate::{ChipDesignShell, VerificationRecord};
use serde_json::{Value, json};
use sha2::Digest;
use ui::framework::testing::TestApp;
fn fixture() -> Value {
    serde_json::from_str(include_str!("../fixtures/evidence-flow.json")).unwrap()
}
fn import(app: &mut TestApp<ChipDesignShell>, value: &Value) -> String {
    let raw = value.to_string();
    app.call("import-verification", json!({"json":raw}))
        .unwrap();
    VerificationRecord::import(&raw).unwrap().0
}
#[test]
fn flow_receipts_can_be_removed_from_the_list_and_detail_and_undone() {
    for detail in [false, true] {
        let mut app = TestApp::<ChipDesignShell>::new().unwrap();
        let id = import(&mut app, &fixture());
        if detail {
            app.call("inspect", json!({"report": id})).unwrap();
        }
        app.harness().resize((1600, 1400)).unwrap();
        app.settle();
        app.harness()
            .click_named(&format!("chip-remove-verification-{id}"))
            .expect("flow receipt must offer the same removal action as other receipts");
        app.settle();
        assert!(app.parsed().verifications.is_empty());
        assert!(app.undo());
        assert!(app.parsed().verifications.contains_key(&id));
    }
}

#[test]
fn public_flow_stage_artifact_search_and_view_preserve_scope() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    assert!(app.parsed().verifications.is_empty());
    let id = import(&mut app, &fixture());
    let child = crate::evidence_flow_view::resource_id(&id, "stage", "physical");
    app.call_on(&child, "inspect-evidence", json!({})).unwrap();
    let read = app.call("read", json!({"item":child})).unwrap().unwrap();
    assert_eq!(read["custody"], "reported_unverified");
    assert_eq!(read["content"]["observations"]["scope"], "library_physical");
    assert_eq!(read["content"]["observations"]["clock_constraint_ns"], 6);
    let artifact = crate::evidence_flow_view::resource_id(&id, "artifact", "physical.json");
    app.call_on(&artifact, "inspect-evidence", json!({}))
        .unwrap();
    let read = app.call("read", json!({"item":artifact})).unwrap().unwrap();
    assert_eq!(read["content"]["sha256"], "a".repeat(64));
    app.call_on(&child, "inspect-evidence", json!({})).unwrap();
    app.harness().resize((1600, 1400)).unwrap();
    app.settle();
    let targets = app.harness().targets();
    let button = targets
        .get("flow-stage-physical")
        .expect("retained graphical stage button");
    assert!(
        button.width() > 40.0 && button.height() > 20.0,
        "graph controls must occupy real geometry: {button:?}"
    );
    let rendered = app.harness().inspection_json().unwrap();
    for key in [
        "chip-flow-disclaimer",
        "chip-flow-clock-constraint",
        "chip-flow-setup",
    ] {
        assert!(
            rendered.contains(
                &serde_json::to_string(&ui::i18n::I18nManager::lookup_plain(key)).unwrap()
            ),
            "missing {key}"
        );
    }
    let hits = <crate::Project as ui::tilo_resource::Resource>::search(&app.state());
    assert!(hits.iter().any(|hit| hit.anchor.as_deref()
        == Some(&crate::evidence_flow_view::resource_id(
            &id, "stage", "physical"
        ))));
    app.harness().click_named("flow-stage-cpu").unwrap();
    app.settle();
    let comparison = ui::i18n::I18nManager::lookup_plain("chip-flow-comparison");
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains(&serde_json::to_string(&comparison).unwrap()),
        "real graph hit-testing must invoke the same stage inspector"
    );
    assert!(app.undo());
    assert!(app.parsed().verifications.is_empty());
}
#[test]
fn invalid_scope_status_dependency_and_bounds_refuse_public_import() {
    let mutations: Vec<fn(&mut Value)> = vec![
        |v| v["evidence_flow"]["stages"][0]["observations"]["comparison"] = json!("exact"),
        |v| v["evidence_flow"]["stages"][1]["observations"]["clock_hz"] = json!(1_000_000),
        |v| v["evidence_flow"]["stages"][1]["status"] = json!("failed"),
        |v| v["evidence_flow"]["edges"][0]["to"] = json!("absent"),
        |v| v["evidence_flow"]["edges"][0]["artifact"] = json!("absent.json"),
        |v| {
            let reverse = json!({"from":"rtl","to":"cpu","kind":"data","artifact":"rtl.json"});
            v["evidence_flow"]["edges"]
                .as_array_mut()
                .unwrap()
                .push(reverse);
        },
        |v| v["artifact_sha256"]["../outside.json"] = json!("a".repeat(64)),
        |v| v["evidence_flow"]["stages"][3]["observations"]["manufacturable"] = json!(true),
        |v| {
            let stages = v["evidence_flow"]["stages"].as_array_mut().unwrap();
            let first = stages[0].clone();
            stages.extend(std::iter::repeat_n(first, 29));
        },
    ];
    for mutate in mutations {
        let mut app = TestApp::<ChipDesignShell>::new().unwrap();
        let mut value = fixture();
        mutate(&mut value);
        assert!(
            app.call("import-verification", json!({"json":value.to_string()}))
                .is_err()
        );
        assert!(app.parsed().verifications.is_empty());
    }
}
#[test]
fn referenced_subflow_resolves_only_exact_imported_summary_bytes() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    let mut child = fixture();
    child["evidence_flow"]["title"] = json!("Synthetic layer detail");
    child["evidence_flow"]["subflows"] = json!([]);
    let raw = child.to_string();
    let hash = hex::encode(sha2::Sha256::digest(raw.as_bytes()));
    let mut root = fixture();
    root["artifact_sha256"]["next.json"] = json!(hash);
    let root_id = import(&mut app, &root);
    let link = crate::evidence_flow_view::resource_id(&root_id, "subflow", "layer-group");
    app.call_on(&link, "inspect-evidence", json!({})).unwrap();
    assert!(
        app.call_on(&link, "open-evidence-subflow", json!({}))
            .is_err()
    );
    app.settle();
    let missing = ui::i18n::I18nManager::lookup_plain("chip-flow-not-imported");
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains(&serde_json::to_string(&missing).unwrap())
    );
    let child_id = import(&mut app, &child);
    app.settle();
    let open = ui::i18n::I18nManager::lookup_plain("chip-flow-open-subflow");
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains(&serde_json::to_string(&open).unwrap())
    );
    let resolved = app.call("read", json!({"item":link})).unwrap().unwrap();
    assert_eq!(
        resolved["content"]["resolved_resource"],
        crate::resource::verification_resource_id(&child_id)
    );
    app.call_on(&link, "open-evidence-subflow", json!({}))
        .unwrap();
    assert_eq!(app.parsed().verifications.len(), 2);
}

#[test]
fn expected_refusal_is_visible_without_completed_outputs() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    let mut value = fixture();
    let observation = &mut value["evidence_flow"]["stages"][1]["observations"];
    observation["outcome"] = json!("expected_refusal");
    observation["accepted_outputs"] = json!(0);
    observation["refused_inputs"] = json!(1);
    let id = import(&mut app, &value);
    let child = crate::evidence_flow_view::resource_id(&id, "stage", "rtl");
    let read = app.call("read", json!({"item":child})).unwrap().unwrap();
    assert_eq!(
        read["content"]["observations"]["outcome"],
        "expected_refusal"
    );
    assert_eq!(read["content"]["observations"]["accepted_outputs"], 0);
    value["evidence_flow"]["stages"][1]["observations"]["outcome"] = json!("completed");
    assert!(
        app.call("import-verification", json!({"json":value.to_string()}))
            .is_err()
    );
}

#[test]
fn project_can_retain_root_and_full_layer_flows_but_still_enforces_capacity() {
    let mut project = ui::tilo_resource::Replica::<crate::Project>::new();
    for layer in 0..128 {
        let mut value = fixture();
        value["evidence_flow"]["title"] = json!(format!("Synthetic bounded layer {layer}"));
        project
            .apply(crate::Project::IMPORT_VERIFICATION.with(value.to_string()))
            .unwrap();
    }
    assert_eq!(project.state().unwrap().verifications.len(), 128);
    let mut overflow = fixture();
    overflow["evidence_flow"]["title"] = json!("Synthetic capacity overflow");
    assert!(
        project
            .apply(crate::Project::IMPORT_VERIFICATION.with(overflow.to_string()))
            .is_err()
    );
    assert_eq!(project.state().unwrap().verifications.len(), 128);
}

#[test]
fn thirty_two_unique_graph_nodes_import_but_thirty_three_refuse() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    let mut value = fixture();
    value["evidence_flow"]["subflows"] = json!([]);
    let template = value["evidence_flow"]["stages"][0].clone();
    let stages = value["evidence_flow"]["stages"].as_array_mut().unwrap();
    for n in 4..32 {
        let mut stage = template.clone();
        stage["id"] = json!(format!("stage-{n}"));
        stages.push(stage);
    }
    import(&mut app, &value);
    let mut last = template;
    last["id"] = json!("stage-32");
    value["evidence_flow"]["stages"]
        .as_array_mut()
        .unwrap()
        .push(last);
    assert!(
        app.call("import-verification", json!({"json":value.to_string()}))
            .is_err()
    );
    assert_eq!(app.parsed().verifications.len(), 1);
}

fn contains_target(node: &Value, target: &str) -> bool {
    node["test_id"] == target
        || node["children"]
            .as_array()
            .is_some_and(|children| children.iter().any(|child| contains_target(child, target)))
}
fn card_bottom(node: &Value, target: &str) -> Option<f64> {
    if node["widget"] == "Card" && contains_target(node, target) {
        return Some(node["rect"][1].as_f64()? + node["rect"][3].as_f64()?);
    }
    node["children"]
        .as_array()?
        .iter()
        .find_map(|child| card_bottom(child, target))
}
#[test]
fn measured_graph_rows_do_not_overlap_at_two_hundred_percent_text() {
    let mut app =
        TestApp::<ChipDesignShell>::capture_at(Some("evidence-flow"), (1600, 1600)).unwrap();
    app.harness().set_theme(
        ui::Theme::named("high-contrast")
            .unwrap()
            .with_text_scale(2.0),
    );
    app.settle();
    let tree: Value = serde_json::from_str(&app.harness().inspection_json().unwrap()).unwrap();
    let targets = app.harness().targets();
    let subflow = targets.get("flow-subflow-layer-group").unwrap();
    for target in [
        "flow-stage-cpu",
        "flow-stage-rtl",
        "flow-stage-synth",
        "flow-stage-physical",
    ] {
        let bottom = card_bottom(&tree["root"], target).expect("actual retained stage card");
        assert!(
            subflow.y0 >= bottom,
            "measured subflow row overlaps {target}: {} < {bottom}",
            subflow.y0
        );
    }
}

#[test]
fn public_host_search_exposes_actual_indexed_evidence_stage() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    import(&mut app, &fixture());
    let address = ui::tilo_actions::ActionAddress::new("tilo.host:search", "open");
    app.harness()
        .invoke_action(
            &address,
            json!({"query":"Synthetic CPU"}).as_object().unwrap(),
            false,
            None,
        )
        .unwrap();
    app.settle();
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("Synthetic CPU comparison")
    );
    assert_eq!(app.parsed().verifications.len(), 1);
    let audit = app.harness().native_app_audit();
    assert!(
        audit.accessibility.issues.is_empty(),
        "{:?}",
        audit.accessibility
    );
    assert!(audit.issues.is_empty(), "{:?}", audit.issues);
}
