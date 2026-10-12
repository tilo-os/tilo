//! Real public resource edits of matrix metadata; no weight allocation or execution.
use crate::operator_graph::RESOURCE_ID;
use crate::tests::Parsed as _;
use crate::{ChipDesignShell, OperatorNode, Project as ChipProject};
use ui::framework::testing::TestApp;

fn add(
    app: &mut TestApp<ChipDesignShell>,
    id: &str,
    kind: &str,
    shape: &[u16],
    output: Option<u32>,
) -> Result<Option<serde_json::Value>, ui::tilo_actions::DoorError> {
    let mut args = serde_json::json!({
        "node":id,"kind":kind,"shape":shape,
        "x":0,"y":0
    });
    if let Some(width) = output {
        args["output_width"] = width.into();
    }
    crate::tests::graph(app, "add-node", args)
}

fn connect(app: &mut TestApp<ChipDesignShell>, source: &str, port: &str, dest: &str, input: &str) {
    crate::tests::graph(
        app,
        "connect",
        serde_json::json!({
            "from_node":source,"from_port":port,"to_node":dest,"to_port":input
        }),
    )
    .unwrap();
}

#[test]
fn public_matrix_projection_preserves_large_geometry_without_payloads() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    add(&mut app, "activation", "input", &[2, 2048], None).unwrap();
    add(
        &mut app,
        "weights",
        "matrix_weight_input",
        &[2048],
        Some(98_304),
    )
    .unwrap();
    add(
        &mut app,
        "projection",
        "linear_projection",
        &[2, 2048],
        Some(98_304),
    )
    .unwrap();
    connect(&mut app, "activation", "value", "projection", "value");
    connect(&mut app, "weights", "weights", "projection", "weights");
    let content = app
        .call("read", serde_json::json!({"item":RESOURCE_ID}))
        .unwrap()
        .unwrap();
    assert_eq!(content["qualification"], "structural_design_only");
    assert_eq!(
        content["graph"]["nodes"]["weights"]["outputs"][0]["data_type"],
        "f32[98304, 2048]"
    );
    assert_eq!(
        content["graph"]["nodes"]["projection"]["outputs"][0]["data_type"],
        "f32[2, 98304]"
    );
    assert_eq!(app.parsed().operator_graph.active.len(), 2);
    assert_eq!(app.parsed().operator_graph.nodes["weights"].shape, [2048]);
    assert!(
        serde_json::to_vec(&app.parsed().operator_graph)
            .unwrap()
            .len()
            < 4096
    );
    assert!(app.undo());
    assert_eq!(app.parsed().operator_graph.active.len(), 1);
}

#[test]
fn projection_resize_keeps_rejected_candidate_and_undo_restores_connection() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    add(
        &mut app,
        "weights",
        "matrix_weight_input",
        &[2048],
        Some(98_304),
    )
    .unwrap();
    add(
        &mut app,
        "projection",
        "linear_projection",
        &[2048],
        Some(98_304),
    )
    .unwrap();
    connect(&mut app, "weights", "weights", "projection", "weights");
    crate::tests::graph(
        &mut app,
        "set-output-width",
        serde_json::json!({"node":"projection","width":65536}),
    )
    .unwrap();
    let state = &app.parsed().operator_graph;
    assert!(state.active.is_empty());
    assert_eq!(state.candidates.len(), 1);
    assert_eq!(state.rejected.len(), 1);
    assert_eq!(state.rejected[0].reason, "type_mismatch");
    assert_eq!(state.nodes["projection"].output_width, Some(65536));
    assert!(app.undo());
    assert_eq!(app.parsed().operator_graph.active.len(), 1);
    assert!(app.parsed().operator_graph.rejected.is_empty());
    assert_eq!(
        app.parsed().operator_graph.nodes["projection"].output_width,
        Some(98_304)
    );
}

#[test]
fn matrix_metadata_bounds_refuse_bad_public_edits_without_mutation() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    for (kind, shape, width) in [
        ("linear_projection", vec![8], None),
        ("matrix_weight_input", vec![2, 8], Some(8)),
        ("matrix_weight_input", vec![8], Some(0)),
        ("linear_projection", vec![8], Some(1_048_577)),
        ("linear_projection", vec![16384], Some(262_145)),
        ("input", vec![8], Some(8)),
    ] {
        assert!(add(&mut app, "bad", kind, &shape, width).is_err());
        assert!(app.parsed().operator_graph.nodes.is_empty());
    }
    add(
        &mut app,
        "boundary",
        "matrix_weight_input",
        &[16384],
        Some(262_144),
    )
    .unwrap();
    assert!(
        crate::tests::graph(
            &mut app,
            "set-output-width",
            serde_json::json!({"node":"boundary","width":262_145})
        )
        .is_err()
    );
    assert_eq!(
        app.parsed().operator_graph.nodes["boundary"].output_width,
        Some(262_144)
    );
    assert!(
        crate::tests::graph(
            &mut app,
            "set-output-width",
            serde_json::json!({"node":"missing","width":8})
        )
        .is_err()
    );
}

#[test]
fn legacy_node_json_remains_loadable_and_omits_matrix_metadata() {
    let legacy =
        r#"{"id":"legacy","kind":"input","shape":[4,8],"normalization_width":8,"x":0,"y":0}"#;
    let node: OperatorNode = serde_json::from_str(legacy).unwrap();
    assert_eq!(node.output_width, None);
    assert!(
        serde_json::to_value(&node)
            .unwrap()
            .get("output_width")
            .is_none()
    );
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    app.invoke(ChipProject::ADD_NODE.with(node)).unwrap();
    assert_eq!(app.parsed().operator_graph.nodes["legacy"].shape, [4, 8]);
    assert!(
        crate::tests::graph(
            &mut app,
            "set-output-width",
            serde_json::json!({"node":"legacy","width":8})
        )
        .is_err()
    );
    assert_eq!(
        app.parsed().operator_graph.nodes["legacy"].output_width,
        None
    );
}

#[test]
fn inspector_input_invokes_the_same_matrix_edit_and_system_undo() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    add(
        &mut app,
        "weights",
        "matrix_weight_input",
        &[2048],
        Some(8192),
    )
    .unwrap();
    app.effect(&crate::view::ChipEffect::InspectOperator {
        node: Some("weights".into()),
    })
    .unwrap();
    app.harness().resize((1600, 1400)).unwrap();
    app.settle();
    let expected =
        ui::i18n::I18nManager::format("chip-tensor-shape", &[("shape", "[8192, 2048]".into())]);
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains(&serde_json::to_string(&expected).unwrap())
    );
    app.harness()
        .click_named("chip-matrix-output-width-weights")
        .unwrap();
    app.harness().set_modifiers(&["ctrl".into()]);
    app.harness().key("a");
    app.harness().set_modifiers(&[]);
    app.harness().commit_text("16384");
    app.settle();
    assert_eq!(
        app.parsed().operator_graph.nodes["weights"].output_width,
        Some(16384)
    );
    assert!(app.undo());
    assert_eq!(
        app.parsed().operator_graph.nodes["weights"].output_width,
        Some(8192)
    );
}

#[test]
fn public_input_reshape_retypes_ports_and_preserves_legacy_bounds() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    add(
        &mut app,
        "weights",
        "matrix_weight_input",
        &[2048],
        Some(8192),
    )
    .unwrap();
    add(
        &mut app,
        "projection",
        "linear_projection",
        &[2048],
        Some(8192),
    )
    .unwrap();
    connect(&mut app, "weights", "weights", "projection", "weights");
    crate::tests::graph(
        &mut app,
        "set-shape",
        serde_json::json!({"node":"weights","shape":[4096]}),
    )
    .unwrap();
    assert_eq!(
        app.parsed().operator_graph.nodes["weights"].normalization_width,
        4096
    );
    assert_eq!(
        app.parsed().operator_graph.rejected[0].reason,
        "type_mismatch"
    );
    for shape in [vec![], vec![0], vec![2, 32769], vec![2, 4096]] {
        assert!(
            crate::tests::graph(
                &mut app,
                "set-shape",
                serde_json::json!({"node":"weights","shape":shape})
            )
            .is_err()
        );
        assert_eq!(app.parsed().operator_graph.nodes["weights"].shape, [4096]);
    }
    assert!(app.undo());
    assert_eq!(
        app.parsed().operator_graph.nodes["weights"].normalization_width,
        2048
    );
    assert_eq!(app.parsed().operator_graph.active.len(), 1);
}

#[test]
fn inspector_shape_field_edits_input_geometry_through_the_resource() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    add(&mut app, "weights", "matrix_weight_input", &[8], Some(8192)).unwrap();
    app.effect(&crate::view::ChipEffect::InspectOperator {
        node: Some("weights".into()),
    })
    .unwrap();
    app.harness().resize((1600, 1400)).unwrap();
    app.settle();
    app.harness()
        .click_named("chip-operator-shape-weights")
        .unwrap();
    app.harness().set_modifiers(&["ctrl".into()]);
    app.harness().key("a");
    app.harness().set_modifiers(&[]);
    app.harness().commit_text("[4096]");
    app.settle();
    assert_eq!(app.parsed().operator_graph.nodes["weights"].shape, [4096]);
    assert_eq!(
        app.parsed().operator_graph.nodes["weights"].normalization_width,
        4096
    );
    assert_eq!(
        app.parsed().operator_graph.nodes["weights"].output_width,
        Some(8192)
    );
    assert!(app.undo());
    assert_eq!(app.parsed().operator_graph.nodes["weights"].shape, [8]);
}
