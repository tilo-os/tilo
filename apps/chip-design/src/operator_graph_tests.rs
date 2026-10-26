use crate::operator_graph::RESOURCE_ID;
use crate::tests::{Parsed as _, on_peer};
use crate::{ChipDesignShell, OperatorKind, OperatorNode, Project as ChipProject, ProjectState};
use ui::framework::testing::TestApp;
use ui::graph_editor::Edge;
use ui::tilo_resource::{Invoke, Resource as _};

fn node(id: &str, kind: OperatorKind) -> OperatorNode {
    OperatorNode {
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
    }
}

fn add(id: &str, kind: OperatorKind) -> Invoke<ChipProject> {
    ChipProject::ADD_NODE.with(node(id, kind))
}

fn connect(edge: Edge) -> Invoke<ChipProject> {
    ChipProject::CONNECT.with(crate::resource::project::Connect {
        from_node: edge.from_node,
        from_port: edge.from_port,
        to_node: edge.to_node,
        to_port: edge.to_port,
    })
}

fn disconnect(node: &str, port: &str) -> Invoke<ChipProject> {
    ChipProject::DISCONNECT.with(crate::resource::project::Disconnect {
        node: node.into(),
        port: port.into(),
    })
}
fn edge(from: &str, port: &str, to: &str, input: &str) -> Edge {
    Edge {
        from_node: from.into(),
        from_port: port.into(),
        to_node: to.into(),
        to_port: input.into(),
    }
}
fn apply(app: &mut TestApp<ChipDesignShell>, invoke: Invoke<ChipProject>) {
    app.invoke(invoke).unwrap();
}

#[test]
fn live_graph_starts_empty_and_child_semantic_edits_are_undoable() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    assert!(app.parsed().operator_graph.nodes.is_empty());
    assert!(
        ChipProject::children(&app.state())
            .iter()
            .any(|child| child.id.as_str() == RESOURCE_ID)
    );
    app.call(
        "add-node",
        serde_json::json!({"node":"input","kind":"input","shape":[4,8],"x":32,"y":32}),
    )
    .unwrap();
    assert_eq!(app.parsed().operator_graph.nodes["input"].shape, vec![4, 8]);
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
    assert!(app.parsed().operator_graph.nodes.is_empty());
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
            ChipProject::ADD_NODE.with(OperatorNode {
                id: id.into(),
                kind: OperatorKind::Input,
                shape: shape.clone(),
                normalization_width: width,
                output_width: None,
                x: 0,
                y: 0,
            }),
        );
        assert_eq!(app.parsed().operator_graph.nodes[id].shape, shape);
    }
    let too_many = ChipProject::ADD_NODE.with(OperatorNode {
        id: "over-budget".into(),
        kind: OperatorKind::Input,
        shape: vec![2, 32769],
        normalization_width: 32769,
        output_width: None,
        x: 0,
        y: 0,
    });
    assert!(app.invoke(too_many).is_err());
    assert_eq!(app.parsed().operator_graph.nodes.len(), 3);
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
    apply(&mut app, connect(edge("a", "result", "b", "value")));
    for invalid in [
        edge("b", "result", "a", "value"),
        edge("weights", "weights", "a", "value"),
        edge("missing", "result", "a", "value"),
        edge("a", "missing", "b", "value"),
    ] {
        assert!(app.invoke(connect(invalid)).is_err());
    }
    assert_eq!(app.parsed().operator_graph.active.len(), 1);
    apply(&mut app, disconnect("b", "value"));
    assert!(app.parsed().operator_graph.active.is_empty());
    assert!(app.undo());
    assert_eq!(app.parsed().operator_graph.active.len(), 1);
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
    apply(&mut left, connect(edge("a", "value", "sink", "value")));
    on_peer(&mut peer, connect(edge("b", "value", "sink", "value")));
    left.merge_peer(&mut peer).unwrap();
    let state = left.parsed().operator_graph;
    assert_eq!(state.candidates.len(), 2);
    assert_eq!(state.active.len(), 1);
    assert_eq!(state.rejected.len(), 1);
    assert_eq!(state.rejected[0].reason, "input_occupied");
    assert_eq!(
        ProjectState::of(&ChipProject::read(&peer).unwrap()).operator_graph,
        state
    );
    apply(&mut left, disconnect("sink", "value"));
    assert!(left.parsed().operator_graph.candidates.is_empty());
    assert!(left.undo());
    assert_eq!(left.parsed().operator_graph, state);
}

#[test]
fn merged_cycle_has_visible_rejection_without_unreadable_state() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    apply(&mut app, add("a", OperatorKind::RmsNorm));
    apply(&mut app, add("b", OperatorKind::Silu));
    let mut peer = app.peer_document();
    apply(&mut app, connect(edge("a", "result", "b", "value")));
    on_peer(&mut peer, connect(edge("b", "result", "a", "value")));
    app.merge_peer(&mut peer).unwrap();
    assert_eq!(app.parsed().operator_graph.candidates.len(), 2);
    assert_eq!(app.parsed().operator_graph.rejected[0].reason, "cycle");
    assert!(
        app.parsed()
            .operator_graph
            .graph()
            .evaluation_order()
            .is_ok()
    );
}

#[test]
fn generic_shots_seed_only_capture_and_render_bound_controls() {
    let mut capture = TestApp::<ChipDesignShell>::capture(Some("operator-graph")).unwrap();
    assert_eq!(capture.parsed().operator_graph.nodes.len(), 8);
    assert_eq!(capture.parsed().operator_graph.active.len(), 7);
    assert!(
        capture
            .parsed()
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
            .parsed()
            .operator_graph
            .nodes
            .is_empty()
    );
}

#[test]
fn pointer_ports_submit_the_same_typed_connection_and_undo() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    let source = node("source", OperatorKind::Input);
    let sink = OperatorNode {
        x: 280,
        ..node("sink", OperatorKind::ProjectionInput)
    };
    apply(&mut app, ChipProject::ADD_NODE.with(source));
    apply(&mut app, ChipProject::ADD_NODE.with(sink));
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
        app.parsed().operator_graph.active,
        vec![edge("source", "value", "sink", "value")]
    );
    assert!(app.undo());
    assert!(app.parsed().operator_graph.active.is_empty());
}

#[test]
fn shape_and_normalization_parameters_refuse_invalid_sequential_edits() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    for (shape, width) in [
        (vec![], 8),
        (vec![4, 0], 0),
        (vec![2048, 2048], 2048),
        (vec![2, 2, 2, 2, 2], 2),
    ] {
        let action = ChipProject::ADD_NODE.with(OperatorNode {
            id: "bad".into(),
            kind: OperatorKind::RmsNorm,
            shape,
            normalization_width: width,
            output_width: None,
            x: 0,
            y: 0,
        });
        assert!(app.invoke(action).is_err());
        assert!(app.parsed().operator_graph.nodes.is_empty());
    }
}

#[test]
fn residual_add_public_ports_refuse_shape_mismatch_and_undo_connections() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    for (node, kind) in [("residual", "input"), ("branch", "input"), ("sum", "add")] {
        crate::tests::graph(
            &mut app,
            "add-node",
            serde_json::json!({"node":node,"kind":kind,"shape":[4,8],
                "x":0,"y":0}),
        )
        .unwrap();
    }
    for (source, target) in [("residual", "left"), ("branch", "right")] {
        crate::tests::graph(
            &mut app,
            "connect",
            serde_json::json!({"from_node":source,"from_port":"value",
                "to_node":"sum","to_port":target}),
        )
        .unwrap();
    }
    assert_eq!(app.parsed().operator_graph.active.len(), 2);
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
    assert_eq!(app.parsed().operator_graph.active.len(), 1);
    crate::tests::graph(
        &mut app,
        "set-shape",
        serde_json::json!({"node":"branch","shape":[8]}),
    )
    .unwrap();
    let before = app.parsed().operator_graph.candidates.len();
    assert!(
        crate::tests::graph(
            &mut app,
            "connect",
            serde_json::json!({
                "from_node":"branch","from_port":"value","to_node":"sum","to_port":"right"
            })
        )
        .is_err()
    );
    assert_eq!(app.parsed().operator_graph.candidates.len(), before);
    let wire = serde_json::to_string(&app.parsed().operator_graph.nodes["sum"]).unwrap();
    let decoded: OperatorNode = serde_json::from_str(&wire).unwrap();
    assert_eq!(decoded.kind, OperatorKind::Add);
}

/// A first visit is told what the app is for and gets one way in that needs
/// nothing from outside: the empty project opens the operator graph.
#[test]
fn the_empty_project_explains_itself_and_opens_the_graph() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    app.settle();
    assert!(
        app.harness().a11y_node_named("Chip Design").is_none(),
        "the window bar names the app; the header does not repeat it"
    );
    assert!(!app.harness().targets().contains_key("chip-graph-add"));
    let open = app
        .harness()
        .controls()
        .into_iter()
        .find(|control| control.name.as_deref() == Some("Open the operator graph"))
        .expect("the empty project offers the operator graph");
    assert!(!open.disabled, "{open:?}");
    app.harness()
        .operate_control(&open.id, &ui::headless::ControlInput::Press)
        .unwrap();
    app.settle();
    app.settle();
    assert!(
        app.harness().targets().contains_key("chip-graph-add"),
        "the graph is open"
    );
}

/// The graph offers its operator kinds as one menu, not a button per kind,
/// and choosing one adds that operator; once there are operators, a line
/// says how to connect them.
#[test]
fn one_add_menu_adds_the_chosen_operator() {
    let mut app = TestApp::<ChipDesignShell>::new().unwrap();
    app.effect(&crate::view::ChipEffect::OperatorGraph).unwrap();
    app.settle();
    let targets = app.harness().targets();
    assert!(targets.contains_key("chip-graph-add"));
    assert!(
        !targets.keys().any(|name| name.contains("Add Multiply")),
        "no button per operator kind"
    );
    app.effect(&crate::view::ChipEffect::AddOperator {
        kind: "Multiply".into(),
    })
    .unwrap();
    app.settle();
    let nodes = &app.parsed().operator_graph.nodes;
    assert_eq!(nodes.len(), 1);
    assert!(
        nodes
            .values()
            .all(|node| node.kind == OperatorKind::Multiply)
    );
    assert!(
        app.effect(&crate::view::ChipEffect::AddOperator {
            kind: "Flux capacitor".into(),
        })
        .is_err()
    );
    let text = app.harness().inspection_json().unwrap();
    assert!(text.contains("To connect two operators"), "{text}");
}

#[test]
fn every_operator_and_the_zoom_controls_are_reachable_in_the_graph() {
    let mut capture = TestApp::<ChipDesignShell>::capture(Some("operator-graph")).unwrap();
    let targets = capture.harness().targets();
    assert!(targets.contains_key("chip-graph-operators"));
    assert!(targets.contains_key("chip-graph-zoom-in"));
    assert!(targets.contains_key("chip-graph-zoom-out"));
    assert!(targets.contains_key("chip-graph-fit"));
    let ids: Vec<String> = capture
        .parsed()
        .operator_graph
        .nodes
        .keys()
        .cloned()
        .collect();
    let rects: Vec<_> = ids
        .iter()
        .filter_map(|id| targets.get(&format!("chip-node-{id}")).copied())
        .collect();
    assert_eq!(rects.len(), ids.len(), "every operator is in the tree");
    assert!(
        rects.windows(2).any(|pair| pair[0] != pair[1]),
        "operators are laid out, not stacked: {rects:?}"
    );
    let graph = capture
        .harness()
        .targets()
        .get("chip-graph-zoom-in")
        .copied();
    assert!(graph.is_some_and(|rect| rect.width() > 0.0));
}
