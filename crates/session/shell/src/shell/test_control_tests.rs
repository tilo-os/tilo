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
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn control_wire_parses_commands_serialises_responses_and_dumps_state() {
    // Phase `control_commands_parse_and_responses_serialise_on_the_wire`.
    {
        // Phase `parse_list_targets_command_round_trips`.
        {
            let cmd = parse_command(r#"{"op":"list_targets"}"#).unwrap();
            assert_eq!(cmd, Command::ListTargets);
        }

        // Phase `parse_unknown_command_returns_structured_error`.
        {
            let err = parse_command(r#"{"op":"go_fishing"}"#).unwrap_err();
            let Response::Err { code, .. } = err else {
                panic!("expected Response::Err");
            };
            assert_eq!(code, "parse_error");
        }

        // Phase `parse_malformed_json_returns_structured_error`.
        {
            let err = parse_command(r"this is not json").unwrap_err();
            let Response::Err { code, .. } = err else {
                panic!("expected Response::Err");
            };
            assert_eq!(code, "parse_error");
        }

        // Phase `parse_snapshot_command_round_trips`.
        {
            let cmd = parse_command(r#"{"op":"snapshot","path":"/tmp/round.png"}"#).unwrap();
            assert_eq!(
                cmd,
                Command::Snapshot {
                    path: "/tmp/round.png".to_string()
                }
            );
        }

        // Phase `parse_state_dump_command_round_trips`.
        {
            let cmd = parse_command(r#"{"op":"state_dump"}"#).unwrap();
            assert_eq!(
                cmd,
                Command::StateDump {
                    include_history: false
                }
            );
            // An older client's request is unchanged on the wire.
            assert_eq!(
                serde_json::to_value(&cmd).unwrap(),
                serde_json::json!({ "op": "state_dump" })
            );
            let cmd = parse_command(r#"{"op":"state_dump","include_history":true}"#).unwrap();
            assert_eq!(
                cmd,
                Command::StateDump {
                    include_history: true
                }
            );
            assert_eq!(
                parse_command(r#"{"op":"windows"}"#).unwrap(),
                Command::Windows
            );
        }

        // Phase `serialise_response_emits_kebab_friendly_json`.
        {
            let resp = Response::Targets(vec![TargetEntry {
                name: "btn".to_string(),
                rect: [0.0, 0.0, 10.0, 10.0],
            }]);
            let out = serialise_response(&resp);
            assert!(out.contains("\"targets\""));
            assert!(out.contains("\"name\":\"btn\""));
            assert!(out.contains("\"rect\""));
        }

        // Phase `serialise_error_emits_code_and_message`.
        {
            let resp = Response::Err {
                code: "parse_error".to_string(),
                message: "oops".to_string(),
            };
            let out = serialise_response(&resp);
            assert!(out.contains("\"code\":\"parse_error\""));
            assert!(out.contains("\"message\":\"oops\""));
            assert!(out.contains("\"err\""));
        }

        // Phase `serialise_state_response_emits_workspace_tree_chrome_and_peers`.
        {
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
    }

    // Phase `snapshot_and_state_dump_answer_or_refuse_without_side_effects`.
    {
        // Phase `snapshot_returns_ok_and_planned_action`.
        {
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

        // Phase `snapshot_with_empty_path_returns_error`.
        {
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

        // Phase `state_dump_returns_state_response`.
        {
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

        // Phase `state_dump_default_impl_returns_nulls`.
        {
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

        // Phase `windows_answers_rows_or_refuses_without_a_tree`.
        {
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
    }
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
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn pointer_commands_plan_typed_buttons_clicks_drags_and_list_targets() {
    // Phase `pointer_button_parses_and_plans_the_typed_button`.
    {
        // Phase `parse_pointer_button_defaults_to_left`.
        {
            let cmd = parse_command(r#"{"op":"pointer_button","pressed":true}"#).unwrap();
            assert_eq!(
                cmd,
                Command::PointerButton {
                    pressed: true,
                    button: PointerButtonKind::Left,
                }
            );
        }

        // Phase `parse_pointer_button_reads_the_right_button`.
        {
            let cmd = parse_command(r#"{"op":"pointer_button","pressed":true,"button":"right"}"#)
                .unwrap();
            assert_eq!(
                cmd,
                Command::PointerButton {
                    pressed: true,
                    button: PointerButtonKind::Right,
                }
            );
        }

        // Phase `dispatching_a_right_click_plans_a_typed_button_not_the_left_one`.
        {
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

        // Phase `dispatching_a_left_click_still_plans_the_original_variant`.
        {
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
    }

    // Phase `list_targets_returns_sorted_non_empty_xywh_entries`.
    {
        // Phase `list_targets_returns_sorted_entries`.
        {
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

        // Phase `list_targets_drops_zero_area_widgets`.
        {
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

        // Phase `list_targets_carries_rect_in_xywh_order`.
        {
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
    }

    // Phase `clicks_resolve_to_rect_centres_or_structured_errors`.
    {
        // Phase `click_at_emits_motion_press_release_in_order`.
        {
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

        // Phase `click_named_resolves_to_rect_centre`.
        {
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

        // Phase `click_named_unknown_target_returns_error_with_no_actions`.
        {
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

        // Phase `click_named_zero_area_returns_error`.
        {
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

        // Phase `click_named_presses_a_target_whose_presentation_has_drawn`.
        {
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

        // Phase `click_named_waits_for_the_targets_own_presentation`.
        {
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

        // Phase `click_named_is_not_held_back_by_a_widget_off_its_path`.
        {
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

        // Phase `click_named_refuses_a_desktop_between_two_arrangements`.
        {
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
        }
    }

    // Phase `drag_emits_press_eight_default_motions_and_release`.
    {
        // Phase `drag_emits_press_motions_release`.
        {
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

        // Phase `drag_default_steps_is_eight`.
        {
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
    }
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
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn keyboard_commands_plan_keys_modifiers_and_typed_text() {
    // Phase `keys_and_modifiers_plan_press_release_actions`.
    {
        // Phase `key_named_emits_press_then_release`.
        {
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

        // Phase `key_single_char_maps_to_char_variant`.
        {
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

        // Phase `key_unknown_returns_parse_error`.
        {
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

        // Phase `parse_key_tag_covers_all_named_keys`.
        {
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

        // Phase `parse_key_tag_rejects_empty_string`.
        {
            assert!(parse_key_tag("").is_none());
        }

        // Phase `modifiers_command_emits_single_action_with_mask`.
        {
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

        // Phase `modifiers_all_false_still_emits_action`.
        {
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
    }

    // Phase `typing_plans_press_release_per_char_with_shift_where_needed`.
    {
        // Phase `type_text_emits_press_release_per_char`.
        {
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

        // Phase `line_breaks_and_tabs_in_typed_text_press_enter_and_tab`.
        {
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

        // Phase `typing_a_capital_holds_shift_around_it`.
        // Asserts the exact action sequence, not just a count — a missing Shift bracket is invisible to chrome (reads `text`) and only shows up on the delivered key stream.
        {
            let state = StaticState::new(&[]);
            let (resp, actions) = dispatch_command(
                &Command::Type {
                    text: "Hi".to_string(),
                },
                &state,
            );
            assert!(matches!(resp, Response::Ok { .. }));
            assert_eq!(
                actions,
                vec![
                    PlannedAction::KeyboardKey {
                        key: KeyTag::Named(NamedKeyTag::Shift),
                        pressed: true,
                    },
                    PlannedAction::KeyboardKey {
                        key: KeyTag::Char('H'),
                        pressed: true,
                    },
                    PlannedAction::KeyboardKey {
                        key: KeyTag::Char('H'),
                        pressed: false,
                    },
                    PlannedAction::KeyboardKey {
                        key: KeyTag::Named(NamedKeyTag::Shift),
                        pressed: false,
                    },
                    PlannedAction::KeyboardKey {
                        key: KeyTag::Char('i'),
                        pressed: true,
                    },
                    PlannedAction::KeyboardKey {
                        key: KeyTag::Char('i'),
                        pressed: false,
                    },
                ],
                "a capital is typed with Shift held across it, and a lowercase \
                     letter is not — a bare keycode would reach the client as its \
                     lowercase letter either way",
            );
        }

        // Phase `typing_a_shifted_punctuation_mark_holds_shift_around_it`.
        {
            let state = StaticState::new(&[]);
            let (_, actions) = dispatch_command(
                &Command::Type {
                    text: ":".to_string(),
                },
                &state,
            );
            assert_eq!(
                actions,
                vec![
                    PlannedAction::KeyboardKey {
                        key: KeyTag::Named(NamedKeyTag::Shift),
                        pressed: true,
                    },
                    PlannedAction::KeyboardKey {
                        key: KeyTag::Char(':'),
                        pressed: true,
                    },
                    PlannedAction::KeyboardKey {
                        key: KeyTag::Char(':'),
                        pressed: false,
                    },
                    PlannedAction::KeyboardKey {
                        key: KeyTag::Named(NamedKeyTag::Shift),
                        pressed: false,
                    },
                ],
            );
        }

        // Phase `type_empty_text_is_a_noop`.
        {
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
    }
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

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn action_ops_resolve_addresses_and_bind_arguments_strictly() {
    // Phase `action_ops_enumerate_invoke_and_track_jobs_by_address`.
    {
        // Phase `enumerate_answers_without_planning_any_side_effect`.
        // If enumeration could produce a `PlannedAction`, surveying the surface would mutate the session just by looking at it.
        {
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

        // Phase `invoke_resolves_the_address_then_plans_one_invocation`.
        {
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

        // Phase `an_enrolled_device_is_addressed_without_the_local_catalogue`.
        {
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

        // Phase `status_and_cancel_plan_the_broker_job_addresses`.
        {
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
                        provider: crate::shell::search::providers::live_app_actions::PROVIDER_ID
                            .into(),
                        id: expected,
                        args: serde_json::Map::new(),
                        device: None,
                    }]
                );
            }
        }

        // Phase `unknown_address_is_refused_and_plans_nothing`.
        // Something driving the shell without seeing it can't tell "ran and changed nothing" from "never ran" unless the wire says so.
        {
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

        // Phase `stateless_control_refuses_enumeration_rather_than_reporting_empty`.
        {
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

        // Phase `action_ops_round_trip_over_the_wire`.
        {
            let enumerate = parse_command(r#"{"op":"actions_enumerate","query":"settings"}"#)
                .expect("actions_enumerate should parse");
            assert_eq!(
                enumerate,
                Command::ActionsEnumerate {
                    include_programmatic: false,
                    query: "settings".to_string(),
                    page: ListingRequest::default(),
                    wait_ms: None,
                    after_revision: None,
                }
            );

            let bare = parse_command(r#"{"op":"actions_enumerate"}"#)
                .expect("actions_enumerate should parse without a query");
            assert_eq!(
                bare,
                Command::ActionsEnumerate {
                    include_programmatic: false,
                    query: String::new(),
                    page: ListingRequest::default(),
                    wait_ms: None,
                    after_revision: None,
                }
            );

            let invoke = parse_command(r#"{"op":"actions_invoke","provider":"p","id":"i"}"#)
                .expect("actions_invoke should parse");
            assert_eq!(
                invoke,
                Command::ActionsInvoke {
                    provider: "p".to_string(),
                    id: "i".to_string(),
                    args: serde_json::Map::new(),
                    device: None,
                    wait_ms: None,
                }
            );
            let Command::ActionsInvoke { wait_ms, .. } =
                parse_command(r#"{"op":"actions_invoke","provider":"p","id":"i","wait_ms":5000}"#)
                    .expect("actions_invoke should parse with a wait")
            else {
                panic!("expected ActionsInvoke");
            };
            assert_eq!(wait_ms, Some(5000));

            // A listing and a job read wait the same way; without the
            // fields they answer at once, and serialise as before.
            let waited_listing = parse_command(
                r#"{"op":"actions_enumerate","query":"read","wait_ms":2000,"after_revision":"r1"}"#,
            )
            .expect("actions_enumerate should parse with a wait");
            assert_eq!(
                waited_listing,
                Command::ActionsEnumerate {
                    include_programmatic: false,
                    query: "read".to_string(),
                    page: ListingRequest::default(),
                    wait_ms: Some(2000),
                    after_revision: Some("r1".to_string()),
                }
            );
            assert_eq!(
                serde_json::to_value(&bare).expect("serialise"),
                serde_json::json!({"op": "actions_enumerate", "query": "", "include_programmatic": false})
            );
            let status = parse_command(r#"{"op":"actions_status","invocation_id":4}"#)
                .expect("actions_status should parse");
            assert_eq!(
                status,
                Command::ActionsStatus {
                    invocation_id: 4,
                    wait_ms: None
                }
            );
            let waited_status =
                parse_command(r#"{"op":"actions_status","invocation_id":4,"wait_ms":10}"#)
                    .expect("actions_status should parse with a wait");
            assert_eq!(
                waited_status,
                Command::ActionsStatus {
                    invocation_id: 4,
                    wait_ms: Some(10)
                }
            );

            let with_args = parse_command(
                r#"{"op":"actions_invoke","provider":"p","id":"i","args":{"text":"hi","at":2}}"#,
            )
            .expect("actions_invoke should parse with args");
            let Command::ActionsInvoke { args, .. } = with_args else {
                panic!("expected ActionsInvoke");
            };
            assert_eq!(args["text"], "hi");
            assert_eq!(args["at"], 2);
        }
    }

    // Phase `action_arguments_bind_strictly_to_the_invoked_action`.
    {
        // Phase `a_nullary_invocation_needs_no_args_key`.
        {
            let cmd = parse_command(
                r#"{"op":"actions_invoke","provider":"shell.actions","id":"settings.open"}"#,
            )
            .expect("an invocation without `args` must still parse");
            let (resp, actions) = dispatch_command(&cmd, &ActionState);
            assert!(matches!(resp, Response::Answer(_)), "got {resp:?}");
            assert_eq!(
                actions.len(),
                1,
                "the nullary path still plans one invocation"
            );
        }

        // Phase `arguments_bind_and_reach_the_planned_invocation`.
        {
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

        // Phase `a_missing_required_argument_is_refused_under_its_own_code`.
        // The refusal code matters: `unknown_action` would send the caller looking for an address that's actually fine — the two mistakes are corrected differently.
        {
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

        // Phase `a_wrong_typed_argument_is_refused_not_coerced`.
        // Refused rather than coerced — guessing at intent is how a caller ends up believing an edit happened that didn't.
        {
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

        // Phase `an_unknown_argument_is_refused_not_silently_ignored`.
        // Silently dropping it is worst: the caller believes it asked for something, gets `ok`, and the difference is invisible without eyes on the screen.
        {
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

        // Phase `a_json_shape_with_no_parameter_type_is_refused`.
        {
            for bad in [
                serde_json::json!({"text": null}),
                serde_json::json!({"text": ["a"]}),
                serde_json::json!({"text": "ok", "at": 1.5}),
            ] {
                let (resp, actions) =
                    dispatch_command(&invoke_with("text.insert", &bad), &ActionState);
                assert!(actions.is_empty(), "got {actions:?}");
                let Response::Err { code, .. } = resp else {
                    panic!("expected Response::Err, got {resp:?}");
                };
                assert_eq!(code, "invalid_arguments");
            }
        }

        // Phase `arguments_meant_for_another_action_are_refused`.
        // Arguments are checked against the action the *address* resolves to, not the one the caller intended.
        {
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
    }
}
