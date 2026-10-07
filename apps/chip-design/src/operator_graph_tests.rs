use crate::operator_graph::RESOURCE_ID;
use crate::{
    ChipDesignShell, ChipProject, OperatorAction, OperatorGraph, OperatorKind, OperatorNode,
    ProjectAction,
};
use ui::framework::testing::TestApp;
use ui::graph_editor::Edge;
use ui::tilo_actions::{EncodedAction, LocalTransport};
use ui::tilo_resource::{ActionRef, Invoke, Resource};

type AddParams = (String, String, Vec<u16>, u16, u16, u16);

fn add(id: &str, kind: OperatorKind) -> OperatorAction {
    OperatorAction::AddNode {
        node: OperatorNode {
            id: id.into(),
            kind,
            shape: if kind == OperatorKind::NormWeightInput {
                vec![8]
            } else {
                vec![4, 8]
            },
            normalization_width: 8,
            output_width: None,
            x: 0,
            y: 0,
        },
    }
}
fn edge(from: &str, port: &str, to: &str, input: &str) -> Edge {
    Edge {
        from_node: from.into(),
        from_port: port.into(),
        to_node: to.into(),
        to_port: input.into(),
    }
}
fn apply(app: &mut TestApp<ChipDesignShell>, action: OperatorAction) {
    app.invoke(Invoke::apply(&ProjectAction::EditGraph { edit: action }).unwrap())
        .unwrap();
}

#[test]
fn live_graph_starts_empty_and_child_semantic_edits_are_undoable() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    assert!(app.state().operator_graph.nodes.is_empty());
    assert!(
        ChipProject::children(&app.state())
            .iter()
            .any(|child| child.id.as_str() == RESOURCE_ID)
    );
    let add: ActionRef<OperatorGraph, AddParams> = ActionRef::new(
        "add-node",
        &["node", "kind", "shape", "normalization_width", "x", "y"],
    );
    app.invoke_child(
        RESOURCE_ID,
        add.with(("input".into(), "input".into(), vec![4, 8], 8, 32, 32)),
    )
    .unwrap();
    assert_eq!(app.state().operator_graph.nodes["input"].shape, vec![4, 8]);
    let content = app
        .call("read", serde_json::json!({"item":RESOURCE_ID}))
        .unwrap()
        .unwrap();
    assert_eq!(content["qualification"], "structural_design_only");
    assert_eq!(
        content["graph"]["nodes"]["input"]["outputs"][0]["data_type"],
        "f32[4, 8]"
    );
    app.call_on(RESOURCE_ID, "operator-graph", serde_json::json!({}))
        .unwrap();
    assert!(app.undo());
    assert!(app.state().operator_graph.nodes.is_empty());
}

#[test]
fn generic_graph_admits_large_tensor_dimensions_without_model_specific_limits() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    for (id, shape, width) in [
        ("projection", vec![5120], 5120),
        ("wide", vec![65535], 65535),
        ("matrix", vec![2, 32768], 32768),
    ] {
        apply(
            &mut app,
            OperatorAction::AddNode {
                node: OperatorNode {
                    id: id.into(),
                    kind: OperatorKind::Input,
                    shape: shape.clone(),
                    normalization_width: width,
                    output_width: None,
                    x: 0,
                    y: 0,
                },
            },
        );
        assert_eq!(app.state().operator_graph.nodes[id].shape, shape);
    }
    let too_many = OperatorAction::AddNode {
        node: OperatorNode {
            id: "over-budget".into(),
            kind: OperatorKind::Input,
            shape: vec![2, 32769],
            normalization_width: 32769,
            output_width: None,
            x: 0,
            y: 0,
        },
    };
    assert!(
        app.invoke_child::<OperatorGraph>(RESOURCE_ID, Invoke::apply(&too_many).unwrap())
            .is_err()
    );
    assert_eq!(app.state().operator_graph.nodes.len(), 3);
}

#[test]
fn structural_connections_validate_types_cycles_and_sequential_occupancy() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    for (id, kind) in [
        ("a", OperatorKind::RmsNorm),
        ("b", OperatorKind::Silu),
        ("weights", OperatorKind::NormWeightInput),
    ] {
        apply(&mut app, add(id, kind));
    }
    apply(
        &mut app,
        OperatorAction::Connect {
            edge: edge("a", "result", "b", "value"),
        },
    );
    for invalid in [
        edge("b", "result", "a", "value"),
        edge("weights", "weights", "a", "value"),
        edge("missing", "result", "a", "value"),
        edge("a", "missing", "b", "value"),
    ] {
        assert!(
            app.invoke_child::<OperatorGraph>(
                RESOURCE_ID,
                Invoke::apply(&OperatorAction::Connect { edge: invalid }).unwrap()
            )
            .is_err()
        );
    }
    assert_eq!(app.state().operator_graph.active.len(), 1);
    apply(
        &mut app,
        OperatorAction::Disconnect {
            node: "b".into(),
            port: "value".into(),
        },
    );
    assert!(app.state().operator_graph.active.is_empty());
    assert!(app.undo());
    assert_eq!(app.state().operator_graph.active.len(), 1);
}

#[test]
fn concurrent_occupied_input_keeps_both_candidates_and_converges() {
    let mut left = TestApp::<ChipDesignShell>::new().unwrap();
    for (id, kind) in [
        ("a", OperatorKind::Input),
        ("b", OperatorKind::GateInput),
        ("sink", OperatorKind::ProjectionInput),
    ] {
        apply(&mut left, add(id, kind));
    }
    let mut peer = left.peer_document();
    apply(
        &mut left,
        OperatorAction::Connect {
            edge: edge("a", "value", "sink", "value"),
        },
    );
    let action = ProjectAction::EditGraph {
        edit: OperatorAction::Connect {
            edge: edge("b", "value", "sink", "value"),
        },
    };
    let encoded = EncodedAction::encode(ChipProject::ACTION_TYPE, &action).unwrap();
    ui::tilo_actions::dispatch(&mut peer, &action, encoded, &mut LocalTransport::new()).unwrap();
    left.merge_peer(&mut peer).unwrap();
    let state = left.state().operator_graph;
    assert_eq!(state.candidates.len(), 2);
    assert_eq!(state.active.len(), 1);
    assert_eq!(state.rejected.len(), 1);
    assert_eq!(state.rejected[0].reason, "input_occupied");
    assert_eq!(OperatorGraph::read(&peer).unwrap(), state);
    apply(
        &mut left,
        OperatorAction::Disconnect {
            node: "sink".into(),
            port: "value".into(),
        },
    );
    assert!(left.state().operator_graph.candidates.is_empty());
    assert!(left.undo());
    assert_eq!(left.state().operator_graph, state);
}

#[test]
fn merged_cycle_has_visible_rejection_without_unreadable_state() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    apply(&mut app, add("a", OperatorKind::RmsNorm));
    apply(&mut app, add("b", OperatorKind::Silu));
    let mut peer = app.peer_document();
    apply(
        &mut app,
        OperatorAction::Connect {
            edge: edge("a", "result", "b", "value"),
        },
    );
    let action = ProjectAction::EditGraph {
        edit: OperatorAction::Connect {
            edge: edge("b", "result", "a", "value"),
        },
    };
    let encoded = EncodedAction::encode(ChipProject::ACTION_TYPE, &action).unwrap();
    ui::tilo_actions::dispatch(&mut peer, &action, encoded, &mut LocalTransport::new()).unwrap();
    app.merge_peer(&mut peer).unwrap();
    assert_eq!(app.state().operator_graph.candidates.len(), 2);
    assert_eq!(app.state().operator_graph.rejected[0].reason, "cycle");
    assert!(
        app.state()
            .operator_graph
            .graph()
            .evaluation_order()
            .is_ok()
    );
}

#[test]
fn generic_shots_seed_only_capture_and_render_bound_controls() {
    let mut capture = TestApp::<ChipDesignShell>::capture(Some("operator-graph")).unwrap();
    assert_eq!(capture.state().operator_graph.nodes.len(), 8);
    assert_eq!(capture.state().operator_graph.active.len(), 7);
    assert!(
        capture
            .state()
            .operator_graph
            .nodes
            .values()
            .all(|node| node.shape
                == if node.kind == OperatorKind::NormWeightInput {
                    vec![8]
                } else {
                    vec![4, 8]
                })
    );
    assert!(
        capture
            .harness()
            .targets()
            .contains_key("chip-port-out-input-value")
    );
    let audit = capture.harness().native_app_audit();
    assert!(
        audit.is_certified(),
        "operator graph semantic audit: {audit:#?}"
    );
    assert!(
        TestApp::<ChipDesignShell>::new()
            .unwrap()
            .state()
            .operator_graph
            .nodes
            .is_empty()
    );
}

#[test]
fn pointer_ports_submit_the_same_typed_connection_and_undo() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    let mut source = add("source", OperatorKind::Input);
    let mut sink = add("sink", OperatorKind::ProjectionInput);
    if let OperatorAction::AddNode { node } = &mut source {
        node.x = 0;
        node.y = 0;
    }
    if let OperatorAction::AddNode { node } = &mut sink {
        node.x = 280;
        node.y = 0;
    }
    apply(&mut app, source);
    apply(&mut app, sink);
    app.effect(&crate::view::ChipEffect::OperatorGraph).unwrap();
    app.settle();
    app.harness()
        .click_named("chip-port-out-source-value")
        .unwrap();
    app.settle();
    app.harness()
        .click_named("chip-port-in-sink-value")
        .unwrap();
    app.settle();
    assert_eq!(
        app.state().operator_graph.active,
        vec![edge("source", "value", "sink", "value")]
    );
    assert!(app.undo());
    assert!(app.state().operator_graph.active.is_empty());
}

#[test]
fn shape_and_normalization_parameters_refuse_invalid_sequential_edits() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    for (shape, width) in [
        (vec![], 8),
        (vec![4, 0], 0),
        (vec![4, 8], 4),
        (vec![2048, 2048], 2048),
        (vec![2, 2, 2, 2, 2], 2),
    ] {
        let action = OperatorAction::AddNode {
            node: OperatorNode {
                id: "bad".into(),
                kind: OperatorKind::RmsNorm,
                shape,
                normalization_width: width,
                output_width: None,
                x: 0,
                y: 0,
            },
        };
        assert!(
            app.invoke_child::<OperatorGraph>(RESOURCE_ID, Invoke::apply(&action).unwrap())
                .is_err()
        );
        assert!(app.state().operator_graph.nodes.is_empty());
    }
}

#[test]
fn residual_add_public_ports_refuse_shape_mismatch_and_undo_connections() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    for (node, kind) in [("residual", "input"), ("branch", "input"), ("sum", "add")] {
        app.call_on(
            RESOURCE_ID,
            "add-node",
            serde_json::json!({"node":node,"kind":kind,"shape":[4,8],
                "normalization_width":8,"x":0,"y":0}),
        )
        .unwrap();
    }
    for (source, target) in [("residual", "left"), ("branch", "right")] {
        app.call_on(
            RESOURCE_ID,
            "connect",
            serde_json::json!({"from_node":source,"from_port":"value",
                "to_node":"sum","to_port":target}),
        )
        .unwrap();
    }
    assert_eq!(app.state().operator_graph.active.len(), 2);
    let content = app
        .call("read", serde_json::json!({"item":RESOURCE_ID}))
        .unwrap()
        .unwrap();
    assert_eq!(content["nodes"]["sum"]["kind"], "add");
    assert_eq!(
        content["graph"]["nodes"]["sum"]["inputs"][0]["data_type"],
        "f32[4, 8]"
    );
    assert_eq!(
        content["graph"]["nodes"]["sum"]["outputs"][0]["id"],
        "result"
    );
    assert!(app.undo());
    assert_eq!(app.state().operator_graph.active.len(), 1);
    app.call_on(
        RESOURCE_ID,
        "set-shape",
        serde_json::json!({"node":"branch","shape":[8]}),
    )
    .unwrap();
    let before = app.state().operator_graph.candidates.len();
    assert!(
        app.call_on(
            RESOURCE_ID,
            "connect",
            serde_json::json!({
                "from_node":"branch","from_port":"value","to_node":"sum","to_port":"right"
            })
        )
        .is_err()
    );
    assert_eq!(app.state().operator_graph.candidates.len(), before);
    let wire = serde_json::to_string(&app.state().operator_graph.nodes["sum"]).unwrap();
    let decoded: OperatorNode = serde_json::from_str(&wire).unwrap();
    assert_eq!(decoded.kind, OperatorKind::Add);
}
