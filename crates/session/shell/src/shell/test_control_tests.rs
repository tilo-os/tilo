#![cfg(test)]

//! Unit tests for the control socket: command dispatch, and the
//! presentation a press is proven against before it lands.

use super::*;

struct StaticState {
    registry: HashMap<String, Rect>,
}

impl StaticState {
    fn new(entries: &[(&str, Rect)]) -> Self {
        Self {
            registry: entries.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        }
    }
}

impl ControlState for StaticState {
    fn widget_registry(&self) -> HashMap<String, Rect> {
        self.registry.clone()
    }
}

/// A widget that holds `children` in the order its `path` step names
/// them, showing a stand-in for content until it is told it has drawn.
struct Witness {
    name: &'static str,
    presented: bool,
    children: Vec<Witness>,
}

impl Witness {
    fn drawn(name: &'static str) -> Self {
        Self::showing(name, true, Vec::new())
    }

    fn showing(name: &'static str, presented: bool, children: Vec<Witness>) -> Self {
        Self {
            name,
            presented,
            children,
        }
    }
}

impl ui::widget::Widget for Witness {
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        _: &ui::theme::Theme,
        _: &mut HashMap<taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> taffy::NodeId {
        tree.new_leaf(taffy::Style::default()).expect("a leaf")
    }

    fn children(&self) -> Vec<&dyn ui::widget::Widget> {
        self.children
            .iter()
            .map(|child| child as &dyn ui::widget::Widget)
            .collect()
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn presented(&self) -> bool {
        self.presented
    }
}

/// A live desktop for the widget registry: `root` is the tree the
/// registry was walked from, `frame` the one on screen.
struct WitnessedState {
    registry: HashMap<String, Rect>,
    root: Witness,
    frame: Option<u64>,
    in_transition: bool,
}

impl WitnessedState {
    fn new(registry: HashMap<String, Rect>, root: Witness) -> Self {
        Self {
            registry,
            root,
            frame: Some(11),
            in_transition: false,
        }
    }
}

impl ControlState for WitnessedState {
    fn widget_registry(&self) -> HashMap<String, Rect> {
        self.registry.clone()
    }

    fn press_witness(&self) -> Option<(&dyn ui::widget::Widget, PressFacts)> {
        Some((&self.root, PressFacts::at(self.frame, self.in_transition)))
    }

    fn chrome_state_json(&self) -> serde_json::Value {
        serde_json::json!({})
    }
}

#[test]
fn commands_parse_from_their_wire_json() {
    let enumerate =
        |query: &str, wait_ms, after_revision: Option<&str>| Command::ActionsEnumerate {
            include_programmatic: false,
            query: query.to_string(),
            page: ListingRequest::default(),
            wait_ms,
            after_revision: after_revision.map(str::to_string),
        };
    let invoke = |args: serde_json::Value, wait_ms| Command::ActionsInvoke {
        provider: "p".to_string(),
        id: "i".to_string(),
        args: args.as_object().cloned().unwrap_or_default(),
        device: None,
        wait_ms,
    };
    let cases = [
        (r#"{"op":"list_targets"}"#, Command::ListTargets),
        (
            r#"{"op":"snapshot","path":"/tmp/round.png"}"#,
            Command::Snapshot {
                path: "/tmp/round.png".to_string(),
            },
        ),
        (
            r#"{"op":"state_dump"}"#,
            Command::StateDump {
                include_history: false,
            },
        ),
        (
            r#"{"op":"state_dump","include_history":true}"#,
            Command::StateDump {
                include_history: true,
            },
        ),
        (r#"{"op":"windows"}"#, Command::Windows),
        (
            r#"{"op":"pointer_button","pressed":true}"#,
            Command::PointerButton {
                pressed: true,
                button: PointerButtonKind::Left,
            },
        ),
        (
            r#"{"op":"pointer_button","pressed":true,"button":"right"}"#,
            Command::PointerButton {
                pressed: true,
                button: PointerButtonKind::Right,
            },
        ),
        (
            r#"{"op":"actions_enumerate","query":"settings"}"#,
            enumerate("settings", None, None),
        ),
        (r#"{"op":"actions_enumerate"}"#, enumerate("", None, None)),
        (
            r#"{"op":"actions_enumerate","query":"read","wait_ms":2000,"after_revision":"r1"}"#,
            enumerate("read", Some(2000), Some("r1")),
        ),
        (
            r#"{"op":"actions_invoke","provider":"p","id":"i"}"#,
            invoke(serde_json::Value::Null, None),
        ),
        (
            r#"{"op":"actions_invoke","provider":"p","id":"i","wait_ms":5000}"#,
            invoke(serde_json::Value::Null, Some(5000)),
        ),
        (
            r#"{"op":"actions_invoke","provider":"p","id":"i","args":{"text":"hi","at":2}}"#,
            invoke(serde_json::json!({"text": "hi", "at": 2}), None),
        ),
        (
            r#"{"op":"actions_status","invocation_id":4}"#,
            Command::ActionsStatus {
                invocation_id: 4,
                wait_ms: None,
            },
        ),
        (
            r#"{"op":"actions_status","invocation_id":4,"wait_ms":10}"#,
            Command::ActionsStatus {
                invocation_id: 4,
                wait_ms: Some(10),
            },
        ),
    ];
    for (wire, want) in cases {
        assert_eq!(parse_command(wire).expect(wire), want, "{wire}");
    }
}

#[test]
fn requests_without_new_fields_serialise_as_older_clients_send_them() {
    let cases = [
        (
            r#"{"op":"state_dump"}"#,
            serde_json::json!({ "op": "state_dump" }),
        ),
        (
            r#"{"op":"actions_enumerate"}"#,
            serde_json::json!({"op": "actions_enumerate", "query": "", "include_programmatic": false}),
        ),
    ];
    for (wire, want) in cases {
        let cmd = parse_command(wire).expect(wire);
        assert_eq!(serde_json::to_value(&cmd).unwrap(), want, "{wire}");
    }
}

#[test]
fn unparseable_commands_return_a_structured_error() {
    for wire in [r#"{"op":"go_fishing"}"#, "this is not json"] {
        let Err(Response::Err { code, .. }) = parse_command(wire) else {
            panic!("expected Response::Err for {wire}");
        };
        assert_eq!(code, "parse_error", "{wire}");
    }
}

#[test]
fn serialise_response_emits_kebab_friendly_json() {
    let resp = Response::Targets(vec![TargetEntry {
        name: "btn".to_string(),
        rect: [0.0, 0.0, 10.0, 10.0],
    }]);
    let out = serialise_response(&resp);
    assert!(out.contains("\"targets\""));
    assert!(out.contains("\"name\":\"btn\""));
    assert!(out.contains("\"rect\""));
}

#[test]
fn serialise_error_emits_code_and_message() {
    let resp = Response::Err {
        code: "parse_error".to_string(),
        message: "oops".to_string(),
    };
    let out = serialise_response(&resp);
    assert!(out.contains("\"code\":\"parse_error\""));
    assert!(out.contains("\"message\":\"oops\""));
    assert!(out.contains("\"err\""));
}

#[test]
fn serialise_state_response_emits_workspace_tree_chrome_and_peers() {
    let resp = Response::State {
        workspace_tree: RawJson::of(&serde_json::json!({ "workspaces": [] })),
        chrome: serde_json::json!({ "overlays": {} }),
        peers: serde_json::json!({ "pointers": [] }),
        sharing: serde_json::json!({ "exposures": [] }),
    };
    let out = serialise_response(&resp);
    assert!(
        out.contains("\"state\""),
        "tag should be `state`; got {out}"
    );
    assert!(out.contains("\"workspace_tree\""), "got {out}");
    assert!(out.contains("\"chrome\""), "got {out}");
    assert!(out.contains("\"peers\""), "got {out}");
    assert!(out.contains("\"sharing\""), "got {out}");
    // The pre-serialised tree is spliced in as JSON, not as a string.
    let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        parsed["state"]["workspace_tree"],
        serde_json::json!({ "workspaces": [] })
    );
}

#[test]
fn snapshot_returns_ok_and_planned_action() {
    let state = StaticState::new(&[]);
    let (resp, actions) = dispatch_command(
        &Command::Snapshot {
            path: "/tmp/x.png".to_string(),
        },
        &state,
    );
    assert!(matches!(resp, Response::Ok { .. }));
    assert_eq!(
        actions,
        vec![PlannedAction::Snapshot {
            path: PathBuf::from("/tmp/x.png"),
        }]
    );
}

#[test]
fn snapshot_with_empty_path_returns_error() {
    let state = StaticState::new(&[]);
    let (resp, actions) = dispatch_command(
        &Command::Snapshot {
            path: String::new(),
        },
        &state,
    );
    assert!(actions.is_empty());
    let Response::Err { code, .. } = resp else {
        panic!("expected Response::Err for empty snapshot path");
    };
    assert_eq!(code, "invalid_path");
}

#[test]
fn state_dump_returns_state_response() {
    let state = StateDumpMockState {
        workspace_tree: serde_json::json!({
            "workspaces": [{ "id": "w-1", "name": "demo" }],
            "active": "w-1",
        }),
        chrome: serde_json::json!({
            "overlays": { "overview": false, "quick_settings": false },
            "revision": 7_u64,
        }),
    };
    let (resp, actions) = dispatch_command(
        &Command::StateDump {
            include_history: false,
        },
        &state,
    );
    assert!(actions.is_empty(), "state_dump must have no side effects");
    let Response::State {
        workspace_tree,
        chrome,
        peers: _,
        sharing: _,
    } = resp
    else {
        panic!("expected Response::State, got something else");
    };
    let workspace_tree = workspace_tree.to_value();
    assert!(
        workspace_tree.get("workspaces").is_some(),
        "workspace_tree should carry `workspaces` key; got {workspace_tree:?}"
    );
    let workspaces = workspace_tree["workspaces"].as_array().unwrap();
    assert_eq!(workspaces.len(), 1);
    assert!(
        chrome.get("overlays").is_some(),
        "chrome should carry `overlays` key; got {chrome:?}"
    );
}

#[test]
fn state_dump_default_impl_returns_nulls() {
    let state = StaticState::new(&[]);
    let (resp, actions) = dispatch_command(
        &Command::StateDump {
            include_history: false,
        },
        &state,
    );
    assert!(actions.is_empty());
    let Response::State {
        workspace_tree,
        chrome,
        peers: _,
        sharing: _,
    } = resp
    else {
        panic!("expected Response::State");
    };
    assert!(workspace_tree.to_value().is_null());
    assert!(chrome.is_null());
}

#[test]
fn windows_answers_rows_or_refuses_without_a_tree() {
    let (resp, actions) = dispatch_command(&Command::Windows, &StaticState::new(&[]));
    assert!(actions.is_empty());
    let Response::Err { code, .. } = resp else {
        panic!("no tree is not the same answer as no windows");
    };
    assert_eq!(code, "windows_failed");
    let state = StateDumpMockState {
        workspace_tree: serde_json::Value::Null,
        chrome: serde_json::Value::Null,
    };
    let (resp, actions) = dispatch_command(&Command::Windows, &state);
    assert!(actions.is_empty());
    assert_eq!(
        resp,
        Response::Answer(serde_json::json!({ "windows": [{ "tab": 1 }] }))
    );
}

/// One workspace, two strips; the active strip holds two groups, the
/// active one with two tabs.
fn windows_fixture() -> crate::shell::state::DesktopTree {
    use crate::shell::state::{
        AppKind, DesktopTree, Strip, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
        WorkspaceId,
    };
    let tab = |id, title: &str, tile| Tab {
        id,
        title: title.to_string(),
        tile,
        app_title: None,
    };
    let group = |id, tabs: Vec<Tab>, active_tab| WindowGroup {
        id,
        tabs,
        active_tab,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    let notes = || TileContent::Foreign {
        app_id: "so.tilo.notes".into(),
        surface_id: 9,
    };
    let mut home = Workspace::new(WorkspaceId(1), "Home");
    let mut main = Strip::new(crate::shell::state::StripId(10), "Main");
    main.window_groups = vec![
        group(
            100,
            vec![
                tab(1000, "Notes", notes()),
                tab(
                    1001,
                    "Placeholder",
                    TileContent::Stub {
                        kind: AppKind::Terminal,
                        label: "Terminal".into(),
                    },
                ),
            ],
            1001,
        ),
        group(101, vec![tab(1010, "Notes 2", notes())], 1010),
    ];
    let mut unnamed = Strip::new(crate::shell::state::StripId(11), "");
    unnamed.window_groups = vec![group(
        110,
        vec![tab(
            1100,
            "Activity",
            TileContent::LocalApp {
                app_id: "so.tilo.activity".into(),
                label: "Activity".into(),
                note: String::new(),
            },
        )],
        1100,
    )];
    home.strips = vec![main, unnamed];
    let mut tree = DesktopTree {
        workspaces: vec![home],
        active: WorkspaceId(1),
        ..DesktopTree::default()
    };
    tree.active_strip
        .insert(WorkspaceId(1), crate::shell::state::StripId(10));
    tree.active_group
        .insert(crate::shell::state::StripId(10), 100);
    tree
}

#[test]
fn windows_list_every_tab_with_its_app_place_and_the_one_in_front() {
    let rows = window_rows(&windows_fixture());
    assert_eq!(
        serde_json::Value::Array(rows),
        serde_json::json!([
            {"workspace": "Home", "strip": "Main", "group": 100, "tab": 1000, "kind": "Foreign",
             "app_id": "so.tilo.notes", "title": "Notes", "active": false},
            {"workspace": "Home", "strip": "Main", "group": 100, "tab": 1001, "kind": "Stub",
             "app_id": null, "title": "Placeholder", "active": true},
            {"workspace": "Home", "strip": "Main", "group": 101, "tab": 1010, "kind": "Foreign",
             "app_id": "so.tilo.notes", "title": "Notes 2", "active": false},
            // An unnamed strip is named by its id.
            {"workspace": "Home", "strip": 11, "group": 110, "tab": 1100, "kind": "LocalApp",
             "app_id": "so.tilo.activity", "title": "Activity", "active": false},
        ])
    );
}

#[test]
fn dispatching_a_right_click_plans_a_typed_button_not_the_left_one() {
    let state = StaticState::new(&[]);
    let (_, actions) = dispatch_command(
        &Command::PointerButton {
            pressed: true,
            button: PointerButtonKind::Right,
        },
        &state,
    );
    assert_eq!(
        actions,
        vec![PlannedAction::MousePointerButton {
            pressed: true,
            button: PointerButtonKind::Right,
        }],
        "a right-click must not plan as MouseLeftButton, or the \
             harness would be pressing the wrong button on the wire",
    );
}

#[test]
fn dispatching_a_left_click_still_plans_the_original_variant() {
    let state = StaticState::new(&[]);
    let (_, actions) = dispatch_command(
        &Command::PointerButton {
            pressed: false,
            button: PointerButtonKind::Left,
        },
        &state,
    );
    assert_eq!(
        actions,
        vec![PlannedAction::MouseLeftButton { pressed: false }],
    );
}

#[test]
fn list_targets_returns_sorted_entries() {
    let state = StaticState::new(&[
        ("zeta", Rect::new(0.0, 0.0, 10.0, 10.0)),
        ("alpha", Rect::new(100.0, 100.0, 20.0, 20.0)),
        ("mid", Rect::new(50.0, 50.0, 15.0, 15.0)),
    ]);
    let (resp, actions) = dispatch_command(&Command::ListTargets, &state);
    assert!(actions.is_empty(), "list_targets has no side effects");
    let Response::Targets(entries) = resp else {
        panic!("expected Targets response");
    };
    let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "mid", "zeta"]);
}

#[test]
fn list_targets_drops_zero_area_widgets() {
    let state = StaticState::new(&[
        ("clickable", Rect::new(0.0, 0.0, 10.0, 10.0)),
        ("not_laid_out", Rect::ZERO),
    ]);
    let (resp, _) = dispatch_command(&Command::ListTargets, &state);
    let Response::Targets(entries) = resp else {
        panic!("expected Targets response");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "clickable");
}

#[test]
fn list_targets_carries_rect_in_xywh_order() {
    let state = StaticState::new(&[(
        "tab",
        Rect::from_origin_size(
            vello::kurbo::Point::new(40.0, 80.0),
            vello::kurbo::Size::new(120.0, 36.0),
        ),
    )]);
    let (resp, _) = dispatch_command(&Command::ListTargets, &state);
    let Response::Targets(entries) = resp else {
        panic!("expected Targets response");
    };
    assert_eq!(entries[0].rect, [40.0, 80.0, 120.0, 36.0]);
}

#[test]
fn click_at_emits_motion_press_release_in_order() {
    let state = StaticState::new(&[]);
    let (resp, actions) = dispatch_command(&Command::ClickAt { x: 42.0, y: 7.0 }, &state);
    assert!(matches!(resp, Response::Ok { .. }));
    assert_eq!(
        actions,
        vec![
            PlannedAction::CursorMoved { x: 42.0, y: 7.0 },
            PlannedAction::MouseLeftButton { pressed: true },
            PlannedAction::MouseLeftButton { pressed: false },
        ]
    );
}

#[test]
fn click_named_resolves_to_rect_centre() {
    let state = StaticState::new(&[(
        "btn",
        Rect::from_origin_size(
            vello::kurbo::Point::new(100.0, 200.0),
            vello::kurbo::Size::new(40.0, 20.0),
        ),
    )]);
    let (resp, actions) = dispatch_command(
        &Command::ClickNamed {
            name: "btn".to_string(),
        },
        &state,
    );
    assert!(matches!(resp, Response::Ok { .. }));
    assert_eq!(
        actions[0],
        PlannedAction::CursorMoved { x: 120.0, y: 210.0 }
    );
}

#[test]
fn click_named_unknown_target_returns_error_with_no_actions() {
    let state = StaticState::new(&[]);
    let (resp, actions) = dispatch_command(
        &Command::ClickNamed {
            name: "ghost".to_string(),
        },
        &state,
    );
    assert!(actions.is_empty());
    let Response::Err { code, .. } = resp else {
        panic!("expected Err");
    };
    assert_eq!(code, "unknown_target");
}

#[test]
fn click_named_zero_area_returns_error() {
    let state = StaticState::new(&[("pending", Rect::ZERO)]);
    let (resp, actions) = dispatch_command(
        &Command::ClickNamed {
            name: "pending".to_string(),
        },
        &state,
    );
    assert!(actions.is_empty());
    let Response::Err { code, .. } = resp else {
        panic!("expected Err");
    };
    assert_eq!(code, "target_not_laid_out");
}

#[test]
fn click_named_presses_a_target_whose_presentation_has_drawn() {
    let state = WitnessedState::new(
        HashMap::from([(
            "Strips/Sheet".to_string(),
            Rect::from_origin_size(
                vello::kurbo::Point::new(10.0, 20.0),
                vello::kurbo::Size::new(40.0, 20.0),
            ),
        )]),
        Witness::showing(
            "Root",
            true,
            vec![Witness::showing(
                "Strips",
                true,
                vec![Witness::drawn("Sheet")],
            )],
        ),
    );
    let (resp, actions) = dispatch_command(
        &Command::ClickNamed {
            name: "Strips/Sheet".to_string(),
        },
        &state,
    );
    assert!(matches!(resp, Response::Ok { .. }), "{resp:?}");
    assert_eq!(actions[0], PlannedAction::CursorMoved { x: 30.0, y: 30.0 });
}

#[test]
fn click_named_waits_for_the_targets_own_presentation() {
    // The document's content has not drawn, so the sheet still
    // shows what stands in for it: pressing the rectangle would
    // press an opening card.
    let state = WitnessedState::new(
        HashMap::from([(
            "Strips/Sheet".to_string(),
            Rect::from_origin_size(
                vello::kurbo::Point::new(10.0, 20.0),
                vello::kurbo::Size::new(40.0, 20.0),
            ),
        )]),
        Witness::showing(
            "Root",
            true,
            vec![Witness::showing(
                "Strips",
                true,
                vec![Witness::showing("Sheet", false, Vec::new())],
            )],
        ),
    );
    let (resp, actions) = dispatch_command(
        &Command::ClickNamed {
            name: "Strips/Sheet".to_string(),
        },
        &state,
    );
    assert!(actions.is_empty());
    let Response::Err { code, .. } = resp else {
        panic!("expected Err");
    };
    assert_eq!(code, "target_unpresented");
}

#[test]
fn click_named_is_not_held_back_by_a_widget_off_its_path() {
    // A widget beside the target — a canvas well in another
    // strip, still asking to draw — is not on the path to the
    // sheet, and does not hold its press back.
    let state = WitnessedState::new(
        HashMap::from([(
            "Strips/Sheet".to_string(),
            Rect::from_origin_size(
                vello::kurbo::Point::new(0.0, 0.0),
                vello::kurbo::Size::new(10.0, 10.0),
            ),
        )]),
        Witness::showing(
            "Root",
            true,
            vec![Witness::showing(
                "Strips",
                true,
                vec![
                    Witness::showing("Well", false, Vec::new()),
                    Witness::drawn("Sheet"),
                ],
            )],
        ),
    );
    let (resp, _) = dispatch_command(
        &Command::ClickNamed {
            name: "Strips/Sheet".to_string(),
        },
        &state,
    );
    assert!(matches!(resp, Response::Ok { .. }), "{resp:?}");
}

#[test]
fn click_named_refuses_a_desktop_between_two_arrangements() {
    let mut state = WitnessedState::new(
        HashMap::from([(
            "Strips/Sheet".to_string(),
            Rect::from_origin_size(
                vello::kurbo::Point::new(0.0, 0.0),
                vello::kurbo::Size::new(10.0, 10.0),
            ),
        )]),
        Witness::showing(
            "Root",
            true,
            vec![Witness::showing(
                "Strips",
                true,
                vec![Witness::drawn("Sheet")],
            )],
        ),
    );
    state.in_transition = true;
    let (resp, actions) = dispatch_command(
        &Command::ClickNamed {
            name: "Strips/Sheet".to_string(),
        },
        &state,
    );
    assert!(actions.is_empty());
    let Response::Err { code, .. } = resp else {
        panic!("expected Err");
    };
    assert_eq!(code, "desktop_in_transition");
    // The state dump says the same, and says when it has settled.
    let in_transition = |state: &WitnessedState| {
        let (Response::State { chrome, .. }, _) = dispatch_command(
            &Command::StateDump {
                include_history: false,
            },
            state,
        ) else {
            panic!("expected State");
        };
        chrome["in_transition"].clone()
    };
    assert_eq!(in_transition(&state), serde_json::json!(true));
    state.in_transition = false;
    assert_eq!(in_transition(&state), serde_json::json!(false));
}

#[test]
fn drag_emits_press_motions_release() {
    let state = StaticState::new(&[]);
    let (resp, actions) = dispatch_command(
        &Command::Drag {
            x_from: 0.0,
            y_from: 0.0,
            x_to: 100.0,
            y_to: 100.0,
            steps: Some(4),
            pressure: None,
        },
        &state,
    );
    assert!(matches!(resp, Response::Ok { .. }));
    // Intermediates use approximate equality to dodge f64 round-off; endpoints are exact integers.
    assert_eq!(actions.len(), 7);
    assert_eq!(actions[0], PlannedAction::CursorMoved { x: 0.0, y: 0.0 });
    assert_eq!(actions[1], PlannedAction::MouseLeftButton { pressed: true });
    assert_eq!(
        actions[6],
        PlannedAction::MouseLeftButton { pressed: false }
    );
    // Interpolated motions, t = 0/3, 1/3, 2/3, 3/3.
    let expected_ts = [0.0_f64, 1.0 / 3.0, 2.0 / 3.0, 1.0];
    for (idx, t) in expected_ts.iter().enumerate() {
        let PlannedAction::CursorMoved { x, y } = actions[2 + idx] else {
            panic!("expected CursorMoved at index {}", 2 + idx);
        };
        let expected = 100.0 * t;
        assert!(
            (x - expected).abs() < 1e-9,
            "x mismatch at step {idx}: got {x}, want {expected}"
        );
        assert!(
            (y - expected).abs() < 1e-9,
            "y mismatch at step {idx}: got {y}, want {expected}"
        );
    }
}

#[test]
fn drag_default_steps_is_eight() {
    let state = StaticState::new(&[]);
    let (_, actions) = dispatch_command(
        &Command::Drag {
            x_from: 0.0,
            y_from: 0.0,
            x_to: 80.0,
            y_to: 0.0,
            steps: None,
            pressure: None,
        },
        &state,
    );
    assert_eq!(actions.len(), 11);
}

#[test]
fn a_drag_with_pressure_is_a_pen_stroke_with_pressure_spread_along_it() {
    use crate::compositor::backend::TabletPhase;
    let state = StaticState::new(&[]);
    let wire: Command = serde_json::from_value(serde_json::json!({
        "op": "drag", "x_from": 0.0, "y_from": 10.0, "x_to": 30.0, "y_to": 10.0,
        "steps": 4, "pressure": [0.2, 1.0]
    }))
    .unwrap();
    let (response, actions) = dispatch_command(&wire, &state);
    assert!(matches!(response, Response::Ok { .. }));
    let planned: Vec<_> = actions
        .iter()
        .map(|action| match action {
            PlannedAction::Pen {
                phase, x, pressure, ..
            } => (*phase, x.round(), (pressure * 100.0).round()),
            other => panic!("a pen drag plans only pen events: {other:?}"),
        })
        .collect();
    assert_eq!(
        planned,
        [
            (TabletPhase::ProximityIn, 0.0, 0.0),
            (TabletPhase::TipDown, 0.0, 20.0),
            (TabletPhase::Motion, 0.0, 20.0),
            (TabletPhase::Motion, 10.0, 47.0),
            (TabletPhase::Motion, 20.0, 73.0),
            (TabletPhase::Motion, 30.0, 100.0),
            (TabletPhase::TipUp, 30.0, 0.0),
            (TabletPhase::ProximityOut, 30.0, 0.0),
        ]
    );

    let constant = Command::Drag {
        x_from: 0.0,
        y_from: 0.0,
        x_to: 5.0,
        y_to: 5.0,
        steps: Some(2),
        pressure: Some(vec![0.6]),
    };
    let (_, actions) = dispatch_command(&constant, &state);
    assert!(actions.iter().all(|action| matches!(
        action,
        PlannedAction::Pen { phase: TabletPhase::Motion | TabletPhase::TipDown, pressure, .. }
            if (pressure - 0.6).abs() < 1e-9
    ) || matches!(
        action,
        PlannedAction::Pen {
            phase: TabletPhase::ProximityIn | TabletPhase::TipUp | TabletPhase::ProximityOut,
            ..
        }
    )));

    for bad in [vec![], vec![1.5], vec![-0.1, 0.5]] {
        let (response, actions) = dispatch_command(
            &Command::Drag {
                x_from: 0.0,
                y_from: 0.0,
                x_to: 5.0,
                y_to: 5.0,
                steps: None,
                pressure: Some(bad.clone()),
            },
            &state,
        );
        assert!(
            matches!(response, Response::Err { .. }) && actions.is_empty(),
            "{bad:?} is refused"
        );
    }
}

#[test]
fn touchpad_gestures_plan_tracker_edges_and_leave_few_fingers_to_the_client() {
    let state = StaticState::new(&[]);
    let wire: Command = serde_json::from_value(serde_json::json!({
        "op": "swipe", "fingers": 3, "dx": -300.0, "dy": 0.0, "steps": 3
    }))
    .expect("swipe parses with release defaulted");
    let (resp, actions) = dispatch_command(&wire, &state);
    assert_eq!(resp, Response::Ok { frame: 0 });
    assert_eq!(
        actions,
        vec![
            PlannedAction::Touchpad(TouchpadEdge::SwipeBegin { fingers: 3 }),
            PlannedAction::Touchpad(TouchpadEdge::SwipeUpdate {
                dx: -100.0,
                dy: 0.0
            }),
            PlannedAction::Touchpad(TouchpadEdge::SwipeUpdate {
                dx: -100.0,
                dy: 0.0
            }),
            PlannedAction::Touchpad(TouchpadEdge::SwipeUpdate {
                dx: -100.0,
                dy: 0.0
            }),
            PlannedAction::Touchpad(TouchpadEdge::SwipeEnd { cancelled: false }),
        ]
    );

    let (_, held) = dispatch_command(
        &Command::Swipe {
            fingers: 4,
            dx: 0.0,
            dy: 50.0,
            steps: Some(2),
            release: false,
        },
        &state,
    );
    assert!(
        !held.contains(&PlannedAction::Touchpad(TouchpadEdge::SwipeEnd {
            cancelled: false
        })),
        "a held swipe keeps its fingers down"
    );

    let (_, pinch) = dispatch_command(
        &Command::Pinch {
            fingers: 3,
            scale: 0.5,
            steps: Some(2),
        },
        &state,
    );
    assert_eq!(
        pinch,
        vec![
            PlannedAction::Touchpad(TouchpadEdge::PinchBegin { fingers: 3 }),
            PlannedAction::Touchpad(TouchpadEdge::PinchUpdate { scale: 0.75 }),
            PlannedAction::Touchpad(TouchpadEdge::PinchUpdate { scale: 0.5 }),
            PlannedAction::Touchpad(TouchpadEdge::PinchEnd),
        ]
    );

    for two in [
        Command::Swipe {
            fingers: 2,
            dx: 10.0,
            dy: 0.0,
            steps: None,
            release: true,
        },
        Command::Pinch {
            fingers: 2,
            scale: 2.0,
            steps: None,
        },
    ] {
        let (resp, actions) = dispatch_command(&two, &state);
        assert!(
            matches!(&resp, Response::Err { code, .. } if code == "client_gesture"),
            "{two:?} answered {resp:?}"
        );
        assert!(actions.is_empty());
    }
}

#[test]
fn key_named_emits_press_then_release() {
    let state = StaticState::new(&[]);
    let (resp, actions) = dispatch_command(
        &Command::Key {
            key: "Enter".to_string(),
        },
        &state,
    );
    assert!(matches!(resp, Response::Ok { .. }));
    assert_eq!(
        actions,
        vec![
            PlannedAction::KeyboardKey {
                key: KeyTag::Named(NamedKeyTag::Enter),
                pressed: true,
            },
            PlannedAction::KeyboardKey {
                key: KeyTag::Named(NamedKeyTag::Enter),
                pressed: false,
            },
        ]
    );
}

#[test]
fn key_single_char_maps_to_char_variant() {
    let state = StaticState::new(&[]);
    let (_, actions) = dispatch_command(
        &Command::Key {
            key: "a".to_string(),
        },
        &state,
    );
    assert_eq!(
        actions[0],
        PlannedAction::KeyboardKey {
            key: KeyTag::Char('a'),
            pressed: true,
        }
    );
}

#[test]
fn key_unknown_returns_parse_error() {
    let state = StaticState::new(&[]);
    let (resp, actions) = dispatch_command(
        &Command::Key {
            key: "go_fishing".to_string(),
        },
        &state,
    );
    assert!(actions.is_empty());
    let Response::Err { code, .. } = resp else {
        panic!("expected Response::Err");
    };
    assert_eq!(code, "parse_error");
}

#[test]
fn parse_key_tag_covers_all_named_keys() {
    let cases = [
        ("Enter", NamedKeyTag::Enter),
        ("Escape", NamedKeyTag::Escape),
        ("Tab", NamedKeyTag::Tab),
        ("ArrowLeft", NamedKeyTag::ArrowLeft),
        ("ArrowRight", NamedKeyTag::ArrowRight),
        ("ArrowUp", NamedKeyTag::ArrowUp),
        ("ArrowDown", NamedKeyTag::ArrowDown),
        ("Backspace", NamedKeyTag::Backspace),
        ("Delete", NamedKeyTag::Delete),
        ("Home", NamedKeyTag::Home),
        ("End", NamedKeyTag::End),
        ("PageUp", NamedKeyTag::PageUp),
        ("PageDown", NamedKeyTag::PageDown),
        ("F1", NamedKeyTag::F1),
        ("F4", NamedKeyTag::F4),
        ("PrintScreen", NamedKeyTag::PrintScreen),
        ("AudioVolumeUp", NamedKeyTag::AudioVolumeUp),
        ("AudioVolumeDown", NamedKeyTag::AudioVolumeDown),
        ("AudioVolumeMute", NamedKeyTag::AudioVolumeMute),
        ("MicrophoneVolumeMute", NamedKeyTag::MicrophoneVolumeMute),
        ("BrightnessUp", NamedKeyTag::BrightnessUp),
        ("BrightnessDown", NamedKeyTag::BrightnessDown),
        ("MediaPlayPause", NamedKeyTag::MediaPlayPause),
        ("MediaTrackNext", NamedKeyTag::MediaTrackNext),
        ("MediaTrackPrevious", NamedKeyTag::MediaTrackPrevious),
        ("Space", NamedKeyTag::Space),
    ];
    for (name, expected) in cases {
        assert_eq!(
            parse_key_tag(name),
            Some(KeyTag::Named(expected)),
            "named-key parse failed for `{name}`"
        );
    }
}

#[test]
fn parse_key_tag_rejects_empty_string() {
    assert!(parse_key_tag("").is_none());
}

#[test]
fn modifiers_command_emits_single_action_with_mask() {
    let state = StaticState::new(&[]);
    let (resp, actions) = dispatch_command(
        &Command::Modifiers {
            ctrl: true,
            shift: false,
            alt: true,
            logo: false,
        },
        &state,
    );
    assert!(matches!(resp, Response::Ok { .. }));
    assert_eq!(
        actions,
        vec![PlannedAction::SetModifiers {
            ctrl: true,
            shift: false,
            alt: true,
            logo: false,
        }]
    );
}

#[test]
fn modifiers_all_false_still_emits_action() {
    let state = StaticState::new(&[]);
    let (_, actions) = dispatch_command(
        &Command::Modifiers {
            ctrl: false,
            shift: false,
            alt: false,
            logo: false,
        },
        &state,
    );
    assert_eq!(actions.len(), 1);
}

#[test]
fn type_text_emits_press_release_per_char() {
    let state = StaticState::new(&[]);
    let (resp, actions) = dispatch_command(
        &Command::Type {
            text: "ab".to_string(),
        },
        &state,
    );
    assert!(matches!(resp, Response::Ok { .. }));
    assert_eq!(actions.len(), 4);
    assert_eq!(
        actions,
        vec![
            PlannedAction::KeyboardKey {
                key: KeyTag::Char('a'),
                pressed: true,
            },
            PlannedAction::KeyboardKey {
                key: KeyTag::Char('a'),
                pressed: false,
            },
            PlannedAction::KeyboardKey {
                key: KeyTag::Char('b'),
                pressed: true,
            },
            PlannedAction::KeyboardKey {
                key: KeyTag::Char('b'),
                pressed: false,
            },
        ]
    );
}

#[test]
fn line_breaks_and_tabs_in_typed_text_press_enter_and_tab() {
    let state = StaticState::new(&[]);
    let (_, actions) = dispatch_command(
        &Command::Type {
            text: "a\nb\r\nc\td\r".to_string(),
        },
        &state,
    );
    let pressed: Vec<KeyTag> = actions
        .iter()
        .filter_map(|action| match action {
            PlannedAction::KeyboardKey { key, pressed: true } => Some(*key),
            _ => None,
        })
        .collect();
    let enter = KeyTag::Named(NamedKeyTag::Enter);
    assert_eq!(
        pressed,
        vec![
            KeyTag::Char('a'),
            enter,
            KeyTag::Char('b'),
            enter,
            KeyTag::Char('c'),
            KeyTag::Named(NamedKeyTag::Tab),
            KeyTag::Char('d'),
            enter,
        ]
    );
    assert_eq!(actions.len(), pressed.len() * 2, "every press is released");
}

// Which key and which Shift or AltGr type a character is the seat keymap's
// to say (`compositor::keymap_typing`), so the plan names characters only: a
// US position planned here would type `z` for `y` on a German keymap.
#[test]
fn typing_plans_characters_not_keyboard_positions() {
    let state = StaticState::new(&[]);
    let (_, actions) = dispatch_command(
        &Command::Type {
            text: "H:@€".to_string(),
        },
        &state,
    );
    let expected: Vec<PlannedAction> = "H:@€"
        .chars()
        .flat_map(|c| {
            [true, false].map(|pressed| PlannedAction::KeyboardKey {
                key: KeyTag::Char(c),
                pressed,
            })
        })
        .collect();
    assert_eq!(actions, expected);
}

#[test]
fn type_empty_text_is_a_noop() {
    let state = StaticState::new(&[]);
    let (resp, actions) = dispatch_command(
        &Command::Type {
            text: String::new(),
        },
        &state,
    );
    assert!(matches!(resp, Response::Ok { .. }));
    assert!(actions.is_empty());
}

struct StateDumpMockState {
    workspace_tree: serde_json::Value,
    chrome: serde_json::Value,
}

impl ControlState for StateDumpMockState {
    fn widget_registry(&self) -> HashMap<String, Rect> {
        HashMap::new()
    }
    fn workspace_tree_json(&self, _include_history: bool) -> RawJson {
        RawJson::of(&self.workspace_tree)
    }
    fn chrome_state_json(&self) -> serde_json::Value {
        self.chrome.clone()
    }
    fn windows_json(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!([{ "tab": 1 }]))
    }
}

struct ActionState;

impl ControlState for ActionState {
    fn widget_registry(&self) -> HashMap<String, Rect> {
        HashMap::new()
    }

    fn enumerate_actions(&self, query: &str) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({
            "query": query,
            "count": 1,
            "actions": [{
                "provider": "shell.actions",
                "id": "settings.open",
                "kind": "Actions",
                "title": "Open settings",
                "subtitle": null,
                "score": 0,
            }],
        }))
    }

    fn resolve_action(
        &self,
        provider: &str,
        id: &str,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, ActionRefusal> {
        let params: &[crate::shell::search::action_api::ActionParam] = match (provider, id) {
            ("shell.actions", "settings.open") => &[],
            ("shell.actions", "text.insert") => MOCK_INSERT_PARAMS,
            _ => {
                return Err(ActionRefusal::unknown(format!(
                    "no visible action addressed by ({provider}, {id})"
                )));
            }
        };
        crate::shell::search::action_api::ActionArgs::bind_json(params, args)
            .map_err(|e| ActionRefusal::invalid_arguments(e.to_string()))?;
        Ok(serde_json::json!({
            "provider": provider,
            "id": id,
            "kind": "Actions",
            "title": "Open settings",
        }))
    }
}

#[test]
fn programmatic_discovery_wire_preserves_defaults_and_refuses_unsupported_states() {
    let normal: Command = serde_json::from_value(serde_json::json!({
        "op": "actions_enumerate", "query": "settings"
    }))
    .unwrap();
    assert!(matches!(
        &normal,
        Command::ActionsEnumerate {
            include_programmatic: false,
            ..
        }
    ));
    let (response, effects) = dispatch_command(&normal, &ActionState);
    assert!(matches!(response, Response::Answer(_)));
    assert!(effects.is_empty());
    let machine: Command = serde_json::from_value(serde_json::json!({
        "op": "actions_enumerate", "query": "settings", "include_programmatic": true
    }))
    .unwrap();
    let (response, effects) = dispatch_command(&machine, &ActionState);
    let Response::Err { code, message } = response else {
        panic!("unsupported machine catalogue must fail: {response:?}");
    };
    assert_eq!(code, "enumerate_failed");
    assert!(message.contains("cannot enumerate programmatic actions"));
    assert!(effects.is_empty());
}

/// A listing of `rows` actions, each with a contract, or of the
/// `renamed` set when the listing has changed under a cursor.
struct ManyActions {
    rows: usize,
    renamed: std::cell::Cell<bool>,
}

impl ControlState for ManyActions {
    fn widget_registry(&self) -> HashMap<String, Rect> {
        HashMap::new()
    }

    fn enumerate_actions(&self, query: &str) -> Result<serde_json::Value, String> {
        let prefix = if self.renamed.get() {
            "renamed"
        } else {
            "action"
        };
        let actions: Vec<_> = (0..self.rows)
            .map(|n| {
                serde_json::json!({
                    "provider": "shell.actions",
                    "id": format!("{prefix}.{n}"),
                    "params": [],
                    "contract": { "permission": "write" },
                })
            })
            .collect();
        Ok(serde_json::json!({ "query": query, "count": actions.len(), "actions": actions }))
    }
}

fn list(page: ListingRequest) -> Command {
    Command::ActionsEnumerate {
        query: String::new(),
        include_programmatic: false,
        page,
        wait_ms: None,
        after_revision: None,
    }
}

fn answer(response: Response) -> serde_json::Value {
    let Response::Answer(answer) = response else {
        panic!("expected an answer, got {response:?}");
    };
    answer
}

#[test]
fn listings_come_in_pages_an_old_client_sees_the_first_of() {
    let state = ManyActions {
        rows: 450,
        renamed: std::cell::Cell::new(false),
    };
    // A request without paging fields parses and is answered as before,
    // as the first page, which says the listing goes on.
    let old = parse_command(r#"{"op":"actions_enumerate","query":""}"#).unwrap();
    assert_eq!(old, list(ListingRequest::default()));
    assert_eq!(
        serde_json::to_value(&old).unwrap(),
        serde_json::json!({"op": "actions_enumerate", "query": "", "include_programmatic": false})
    );
    let first = answer(dispatch_command(&old, &state).0);
    assert_eq!(first["count"], DEFAULT_LISTING_LIMIT);
    assert_eq!(
        first["actions"].as_array().unwrap().len(),
        DEFAULT_LISTING_LIMIT
    );
    assert_eq!(first["total"], 450);
    assert_eq!(first["truncated"], true);
    assert_eq!(first["actions"][0]["contract"]["permission"], "write");

    // Following `next_cursor` visits every row once, in order.
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let request = parse_command(
            &serde_json::json!({
                "op": "actions_enumerate", "limit": 100, "cursor": cursor, "detail": "compact",
            })
            .to_string(),
        )
        .unwrap();
        let page = answer(dispatch_command(&request, &state).0);
        for row in page["actions"].as_array().unwrap() {
            assert!(
                row.get("contract").is_none(),
                "compact rows carry no contract"
            );
            assert!(
                row.get("params").is_some(),
                "compact rows can still be invoked"
            );
            seen.push(row["id"].as_str().unwrap().to_owned());
        }
        assert_eq!(page["count"], page["actions"].as_array().unwrap().len());
        if page["truncated"] == false {
            assert!(page["next_cursor"].is_null());
            break;
        }
        cursor = page["next_cursor"].as_str().map(str::to_owned);
    }
    let expected: Vec<String> = (0..450).map(|n| format!("action.{n}")).collect();
    assert_eq!(seen, expected);

    // A cursor into a listing that has since changed is refused, not
    // silently applied to the new one; a made-up one is a bad argument.
    let page = answer(
        dispatch_command(
            &list(ListingRequest {
                limit: Some(10),
                ..ListingRequest::default()
            }),
            &state,
        )
        .0,
    );
    let cursor = page["next_cursor"].as_str().unwrap().to_owned();
    state.renamed.set(true);
    let (response, _) = dispatch_command(
        &list(ListingRequest {
            cursor: Some(cursor),
            ..ListingRequest::default()
        }),
        &state,
    );
    assert!(
        matches!(&response, Response::Err { code, .. } if code == "stale_cursor"),
        "{response:?}"
    );
    let (response, _) = dispatch_command(
        &list(ListingRequest {
            cursor: Some("nonsense".into()),
            ..ListingRequest::default()
        }),
        &state,
    );
    assert!(
        matches!(&response, Response::Err { code, .. } if code == "invalid_arguments"),
        "{response:?}"
    );

    // `limit` is clamped, never trusted to size a reply.
    let window = ListingRequest {
        limit: Some(usize::MAX),
        ..ListingRequest::default()
    }
    .window(10_000, 7)
    .unwrap();
    assert_eq!(window.rows, 0..MAX_LISTING_LIMIT);
    let window = ListingRequest {
        limit: Some(0),
        ..ListingRequest::default()
    }
    .window(5, 7)
    .unwrap();
    assert_eq!(window.rows, 0..1);
}

/// A listing narrows by kind and lists files only when asked, on the wire
/// as the CLI sends it; a request that says neither keeps its old shape.
#[test]
fn a_listing_narrows_by_kind_and_lists_files_only_when_asked() {
    let page = |line: &str| {
        let Command::ActionsEnumerate { page, .. } = serde_json::from_str(line).unwrap() else {
            panic!("not an enumerate: {line}");
        };
        page
    };
    let asked = page(
        r#"{"op":"actions_enumerate","query":"x","kinds":["application","note"],"files":true}"#,
    );
    assert_eq!(asked.kinds, ["application", "note"]);
    assert!(asked.files);
    let plain = page(r#"{"op":"actions_enumerate","query":"x"}"#);
    assert_eq!(plain, ListingRequest::default());
    assert_eq!(
        serde_json::to_value(&plain).unwrap(),
        serde_json::json!({}),
        "the defaults add nothing to the wire"
    );
}

const MOCK_INSERT_PARAMS: &[crate::shell::search::action_api::ActionParam] = &[
    crate::shell::search::action_api::ActionParam::required(
        "text",
        crate::shell::search::action_api::ParamType::Text,
        "what to write",
    ),
    crate::shell::search::action_api::ActionParam::optional(
        "at",
        crate::shell::search::action_api::ParamType::Integer,
        "character offset",
    ),
];

fn invoke_with(id: &str, args: &serde_json::Value) -> Command {
    Command::ActionsInvoke {
        provider: "shell.actions".to_string(),
        id: id.to_string(),
        args: args.as_object().cloned().unwrap_or_default(),
        device: None,
        wait_ms: None,
    }
}

// If enumeration could produce a `PlannedAction`, surveying the surface would mutate the session just by looking at it.
#[test]
fn enumerate_answers_without_planning_any_side_effect() {
    let (resp, actions) = dispatch_command(
        &Command::ActionsEnumerate {
            include_programmatic: false,
            query: String::new(),
            page: ListingRequest::default(),
            wait_ms: None,
            after_revision: None,
        },
        &ActionState,
    );

    assert!(
        actions.is_empty(),
        "enumeration must plan no actions; got {actions:?}"
    );
    let Response::Answer(payload) = resp else {
        panic!("expected Response::Answer, got {resp:?}");
    };
    assert_eq!(payload["count"], 1);
    assert_eq!(payload["actions"][0]["provider"], "shell.actions");
    assert_eq!(payload["actions"][0]["id"], "settings.open");
}

#[test]
fn invoke_resolves_the_address_then_plans_one_invocation() {
    let (resp, actions) = dispatch_command(
        &invoke_with("settings.open", &serde_json::json!({})),
        &ActionState,
    );

    let Response::Answer(payload) = resp else {
        panic!("expected Response::Answer, got {resp:?}");
    };
    assert_eq!(payload["title"], "Open settings");
    assert_eq!(
        actions,
        vec![PlannedAction::InvokeAction {
            provider: "shell.actions".to_string(),
            id: "settings.open".to_string(),
            args: serde_json::Map::new(),
            device: None,
        }]
    );
}

#[test]
fn an_enrolled_device_is_addressed_without_the_local_catalogue() {
    let device = "22".repeat(32);
    let id = tilo_actions::ActionAddress::new("app.files", "open").encode();
    let (resp, actions) = dispatch_command(
        &Command::ActionsInvoke {
            provider: "workspace:room".into(),
            id: "app.files/open".into(),
            args: serde_json::Map::new(),
            device: Some(device.clone()),
            wait_ms: None,
        },
        &ActionState,
    );
    let Response::Answer(payload) = resp else {
        panic!("expected Response::Answer, got {resp:?}");
    };
    assert_eq!(payload["queued"], true);
    assert_eq!(payload["device"], device);
    assert_eq!(
        actions,
        vec![PlannedAction::InvokeAction {
            provider: "workspace:room".into(),
            id,
            args: serde_json::Map::new(),
            device: Some(device),
        }]
    );
    let (refused, none) = dispatch_command(
        &Command::ActionsInvoke {
            provider: "workspace:room".into(),
            id: "not-an-address".into(),
            args: serde_json::Map::new(),
            device: Some("33".repeat(32)),
            wait_ms: None,
        },
        &ActionState,
    );
    assert!(matches!(refused, Response::Err { .. }));
    assert!(none.is_empty());
}

#[test]
fn status_and_cancel_plan_the_broker_job_addresses() {
    for (command, expected) in [
        (
            Command::ActionsStatus {
                invocation_id: 37,
                wait_ms: None,
            },
            crate::shell::search::providers::live_app_actions::job_status_id(37),
        ),
        (
            Command::ActionsCancel { invocation_id: 37 },
            crate::shell::search::providers::live_app_actions::job_cancel_id(37),
        ),
    ] {
        let (response, actions) = dispatch_command(&command, &ActionState);
        assert!(matches!(response, Response::Answer(_)));
        assert_eq!(
            actions,
            vec![PlannedAction::InvokeAction {
                provider: crate::shell::search::providers::live_app_actions::PROVIDER_ID.into(),
                id: expected,
                args: serde_json::Map::new(),
                device: None,
            }]
        );
    }
}

// Something driving the shell without seeing it can't tell "ran and changed nothing" from "never ran" unless the wire says so.
#[test]
fn unknown_address_is_refused_and_plans_nothing() {
    let (resp, actions) = dispatch_command(
        &invoke_with("definitely.not.a.real.action", &serde_json::json!({})),
        &ActionState,
    );

    assert!(
        actions.is_empty(),
        "a refused address must plan no invocation; got {actions:?}"
    );
    let Response::Err { code, .. } = resp else {
        panic!("expected Response::Err, got {resp:?}");
    };
    assert_eq!(code, "unknown_action");
}

#[test]
fn stateless_control_refuses_enumeration_rather_than_reporting_empty() {
    let state = StaticState::new(&[]);
    let (resp, actions) = dispatch_command(
        &Command::ActionsEnumerate {
            include_programmatic: false,
            query: String::new(),
            page: ListingRequest::default(),
            wait_ms: None,
            after_revision: None,
        },
        &state,
    );

    assert!(actions.is_empty());
    let Response::Err { code, .. } = resp else {
        panic!("a stateless control must refuse, not answer; got {resp:?}");
    };
    assert_eq!(code, "enumerate_failed");
}

#[test]
fn a_nullary_invocation_needs_no_args_key() {
    let cmd =
        parse_command(r#"{"op":"actions_invoke","provider":"shell.actions","id":"settings.open"}"#)
            .expect("an invocation without `args` must still parse");
    let (resp, actions) = dispatch_command(&cmd, &ActionState);
    assert!(matches!(resp, Response::Answer(_)), "got {resp:?}");
    assert_eq!(
        actions.len(),
        1,
        "the nullary path still plans one invocation"
    );
}

#[test]
fn arguments_bind_and_reach_the_planned_invocation() {
    let (resp, actions) = dispatch_command(
        &invoke_with(
            "text.insert",
            &serde_json::json!({"text": "hello", "at": 3}),
        ),
        &ActionState,
    );
    assert!(matches!(resp, Response::Answer(_)), "got {resp:?}");
    let [PlannedAction::InvokeAction { args, .. }] = actions.as_slice() else {
        panic!("expected exactly one planned invocation, got {actions:?}");
    };
    assert_eq!(args["text"], "hello");
    assert_eq!(args["at"], 3);
}

// The refusal code matters: `unknown_action` would send the caller looking for an address that's actually fine — the two mistakes are corrected differently.
#[test]
fn a_missing_required_argument_is_refused_under_its_own_code() {
    let (resp, actions) = dispatch_command(
        &invoke_with("text.insert", &serde_json::json!({})),
        &ActionState,
    );
    assert!(
        actions.is_empty(),
        "a refused invocation must plan nothing; got {actions:?}"
    );
    let Response::Err { code, message } = resp else {
        panic!("expected Response::Err, got {resp:?}");
    };
    assert_eq!(code, "invalid_arguments");
    assert!(
        message.contains("text"),
        "the refusal must name the parameter at fault, got {message:?}"
    );
}

// Refused rather than coerced — guessing at intent is how a caller ends up believing an edit happened that didn't.
#[test]
fn a_wrong_typed_argument_is_refused_not_coerced() {
    let (resp, actions) = dispatch_command(
        &invoke_with("text.insert", &serde_json::json!({"text": 7})),
        &ActionState,
    );
    assert!(actions.is_empty(), "got {actions:?}");
    let Response::Err { code, message } = resp else {
        panic!("expected Response::Err, got {resp:?}");
    };
    assert_eq!(code, "invalid_arguments");
    assert!(
        message.contains("text") && message.contains("integer"),
        "{message}"
    );
}

// Silently dropping it is worst: the caller believes it asked for something, gets `ok`, and the difference is invisible without eyes on the screen.
#[test]
fn an_unknown_argument_is_refused_not_silently_ignored() {
    let (resp, actions) = dispatch_command(
        &invoke_with(
            "text.insert",
            &serde_json::json!({"text": "hi", "colour": "red"}),
        ),
        &ActionState,
    );
    assert!(actions.is_empty(), "got {actions:?}");
    let Response::Err { code, message } = resp else {
        panic!("expected Response::Err, got {resp:?}");
    };
    assert_eq!(code, "invalid_arguments");
    assert!(message.contains("colour"), "{message}");
}

#[test]
fn a_json_shape_with_no_parameter_type_is_refused() {
    for bad in [
        serde_json::json!({"text": null}),
        serde_json::json!({"text": ["a"]}),
        serde_json::json!({"text": "ok", "at": 1.5}),
    ] {
        let (resp, actions) = dispatch_command(&invoke_with("text.insert", &bad), &ActionState);
        assert!(actions.is_empty(), "got {actions:?}");
        let Response::Err { code, .. } = resp else {
            panic!("expected Response::Err, got {resp:?}");
        };
        assert_eq!(code, "invalid_arguments");
    }
}

// Arguments are checked against the action the *address* resolves to, not the one the caller intended.
#[test]
fn arguments_meant_for_another_action_are_refused() {
    let (resp, actions) = dispatch_command(
        &invoke_with("settings.open", &serde_json::json!({"text": "hi"})),
        &ActionState,
    );
    assert!(actions.is_empty(), "got {actions:?}");
    let Response::Err { code, .. } = resp else {
        panic!("expected Response::Err, got {resp:?}");
    };
    assert_eq!(code, "invalid_arguments");
}

#[test]
fn a_test_id_activation_asks_the_focused_window_and_stays_off_agent_and_admin_sockets() {
    let request = parse_request(r#"{"op":"activate_test_id","test_id":"akcqjax3.t34.t15.52/0"}"#)
        .expect("the activation parses");
    let command = request.command;
    assert_eq!(
        command,
        Command::ActivateTestId {
            test_id: "akcqjax3.t34.t15.52/0".into()
        }
    );
    let (resp, actions) = dispatch_command(&command, &StaticState::new(&[]));
    assert!(matches!(resp, Response::Ok { .. }));
    assert_eq!(
        actions,
        vec![PlannedAction::ActivateTestId {
            test_id: "akcqjax3.t34.t15.52/0".into()
        }],
        "the app finds the control itself: nothing is planned by where it is drawn"
    );
    assert!(
        !command.agent_permitted(),
        "agents press through input, not test ids"
    );
    assert!(!command.admin_permitted());
}
