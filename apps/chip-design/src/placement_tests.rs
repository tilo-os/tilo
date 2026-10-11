//! Public import, resource navigation and real retained placement geometry.
use crate::tests::Parsed as _;
use crate::{ChipDesignShell, Project as ChipProject, VerificationRecord};
use serde_json::{Value, json};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Resource;

fn fixture() -> Value {
    let mut receipt: Value =
        serde_json::from_str(include_str!("../fixtures/evidence-flow.json")).unwrap();
    receipt["artifact_sha256"]["routed.odb"] = json!("a".repeat(64));
    receipt["artifact_sha256"]["placement.txt"] = json!("b".repeat(64));
    let stage = receipt["evidence_flow"]["stages"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|stage| stage["id"] == "physical")
        .unwrap();
    stage["placement"] = json!({
        "database_artifact":"routed.odb", "observation_artifact":"placement.txt",
        "dbu_per_micron":1000, "die_dbu":[0,0,1_000_000,1_000_000],
        "total_instances":12, "omitted_instances":10,
        "macros":[
            {"id":"macro-a", "name":"Memory A", "master":"Example SRAM", "orientation":"R0",
             "status":"locked", "bounds_dbu":[100_000,100_000,600_000,200_000]},
            {"id":"macro-b", "name":"Memory B", "master":"Example SRAM", "orientation":"MX",
             "status":"placed", "bounds_dbu":[100_000,700_000,600_000,800_000]}
        ]
    });
    receipt
}

fn physical(value: &mut Value) -> &mut Value {
    &mut value["evidence_flow"]["stages"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|stage| stage["id"] == "physical")
        .unwrap()["placement"]
}

#[test]
fn placement_public_read_search_geometry_and_click_share_the_resource() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    assert!(app.parsed().verifications.is_empty());
    let raw = fixture().to_string();
    app.call("import-verification", json!({"json":raw}))
        .unwrap();
    let id = VerificationRecord::import(&raw).unwrap().0;
    let child = crate::evidence_flow_view::resource_id(&id, "placement", "physical/macro-a");
    app.call_on(&child, "inspect-evidence", json!({})).unwrap();
    let read = app.call("read", json!({"item":child})).unwrap().unwrap();
    assert_eq!(read["custody"], "reported_unverified");
    assert_eq!(
        read["content"]["macro"]["bounds_dbu"],
        json!([100_000, 100_000, 600_000, 200_000])
    );
    assert_eq!(read["content"]["placement"]["omitted_instances"], 10);
    let anchor = child.clone();
    assert!(
        ChipProject::search(&app.state())
            .iter()
            .any(|hit| hit.anchor.as_ref() == Some(&anchor))
    );
    app.harness().resize((1600, 3000)).unwrap();
    app.settle();
    let targets = app.harness().targets();
    let lower = targets.get("placement-physical-macro-a").unwrap();
    let upper = targets.get("placement-physical-macro-b").unwrap();
    assert!(
        lower.y0 > upper.y0,
        "physical lower-left origin must invert display Y"
    );
    assert!(
        (lower.width() - 450.0).abs() < 1.0,
        "reported footprint width must remain proportional: {lower:?}"
    );
    assert!(
        (lower.height() - 90.0).abs() < 1.0,
        "reported footprint height must remain proportional: {lower:?}"
    );
    app.harness()
        .click_named("placement-physical-macro-b")
        .unwrap();
    app.settle();
    let rendered = app.harness().inspection_json().unwrap();
    assert!(
        rendered.contains("MX"),
        "real macro control must select its inspector"
    );
    assert!(app.undo());
    assert!(app.parsed().verifications.is_empty());
}

#[test]
fn malformed_placement_refuses_atomically_through_public_import() {
    let mutations: Vec<fn(&mut Value)> = vec![
        |value| physical(value)["dbu_per_micron"] = json!(0),
        |value| physical(value)["die_dbu"] = json!([0, 0, 0, 1_000_000]),
        |value| physical(value)["omitted_instances"] = json!(11),
        |value| physical(value)["database_artifact"] = json!("missing.odb"),
        |value| physical(value)["macros"][0]["bounds_dbu"] = json!([-1, 0, 600_000, 200_000]),
        |value| physical(value)["macros"][0]["bounds_dbu"] = json!([0, 0, 0, 200_000]),
        |value| physical(value)["macros"][1]["id"] = json!("macro-a"),
        |value| physical(value)["macros"][1]["name"] = json!("Memory A"),
        |value| physical(value)["macros"][0]["orientation"] = json!("invented"),
        |value| physical(value)["macros"][0]["status"] = json!("unplaced"),
        |value| physical(value)["macros"][0]["id"] = json!("../../escape"),
    ];
    for mutate in mutations {
        let mut value = fixture();
        mutate(&mut value);
        let mut app = TestApp::<ChipDesignShell>::new().unwrap();
        assert!(
            app.call("import-verification", json!({"json":value.to_string()}))
                .is_err()
        );
        assert!(app.parsed().verifications.is_empty());
    }
}
