use super::*;
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex as StdMutex};

/// A control socket that records commands and writes a `size` PNG for every
/// snapshot, as the compositor does.
struct FakeCompositor {
    commands: Arc<StdMutex<Vec<Value>>>,
    _dir: tempfile::TempDir,
    computer: Computer,
}

impl FakeCompositor {
    fn start(size: (u32, u32)) -> Self {
        Self::start_sized(move |_| size)
    }

    /// A compositor whose snapshot size depends on the command, as an
    /// agent's strip viewport does on the agent.
    fn start_sized(size_of: impl Fn(&Value) -> (u32, u32) + Send + 'static) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("control.sock");
        let scratch = dir.path().join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        let commands = Arc::new(StdMutex::new(Vec::new()));
        let recorded = Arc::clone(&commands);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let command: Value = serde_json::from_str(&line).unwrap();
                let reply = if command["op"] == "key" && command["key"] == "Unmapped" {
                    json!({"err":{"code":"parse_error","message":"unknown key"}})
                } else if command["op"] == "agent_seat" {
                    let (width, height) = size_of(&command);
                    json!({"answer":{"viewport":[40.0, 30.0, width, height],"window":command["window"]}})
                } else {
                    if command["op"] == "snapshot" {
                        let size = size_of(&command);
                        let image = image::RgbaImage::from_pixel(
                            size.0,
                            size.1,
                            image::Rgba([10, 20, 30, 255]),
                        );
                        image.save(command["path"].as_str().unwrap()).unwrap();
                    }
                    json!({"ok":{"frame":0}})
                };
                recorded.lock().unwrap().push(command);
                let mut stream = stream;
                writeln!(stream, "{reply}").unwrap();
            }
        });
        Self {
            commands,
            computer: Computer::new(socket, scratch),
            _dir: dir,
        }
    }

    fn ops(&self) -> Vec<Value> {
        std::mem::take(&mut *self.commands.lock().unwrap())
    }
}

fn act(action: &str) -> Action {
    Action {
        action: action.into(),
        ..Action::default()
    }
}

#[test]
fn chords_follow_xdotool_names() {
    assert_eq!(
        Chord::parse("ctrl+shift+Return").unwrap(),
        Chord {
            modifiers: vec!["Control", "Shift"],
            key: Some("Enter".into())
        }
    );
    assert_eq!(Chord::parse("a").unwrap().key.as_deref(), Some("a"));
    assert_eq!(
        Chord::parse("Page_Down").unwrap().key.as_deref(),
        Some("PageDown")
    );
    assert!(Chord::parse("ctrl+a+b").is_err());
    assert!(Chord::parse("F13").is_err());
    assert!(Chord::parse("").is_err());
}

#[test]
fn coordinates_need_a_screenshot_first() {
    let fake = FakeCompositor::start((800, 600));
    let click = Action {
        coordinate: Some([10.0, 10.0]),
        ..act("left_click")
    };
    let error = fake.computer.run(&[click], false).err().unwrap();
    assert!(error.contains("screenshot"), "{error}");
}

#[test]
fn large_screens_are_scaled_and_clicks_scaled_back() {
    let fake = FakeCompositor::start((2560, 1600));
    let shot = fake.computer.run(&[act("screenshot")], false).unwrap();
    assert_eq!(shot[0].text["width"], 1280);
    assert_eq!(shot[0].text["height"], 800);
    let frame = shot[0].image.as_ref().unwrap();
    assert_eq!(frame.mime, "image/jpeg");
    let decoded = image::load_from_memory(&frame.data).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (1280, 800));
    fake.ops();

    let click = Action {
        coordinate: Some([100.0, 50.0]),
        no_screenshot: Some(true),
        ..act("left_click")
    };
    let outcome = fake.computer.run(&[click], false).unwrap();
    assert!(outcome[0].image.is_none());
    assert_eq!(
        fake.ops(),
        vec![
            json!({"op":"pointer_move","x":200.0,"y":100.0}),
            json!({"op":"pointer_button","pressed":true,"button":"left"}),
            json!({"op":"pointer_button","pressed":false,"button":"left"}),
        ]
    );
    let position = fake.computer.run(&[act("cursor_position")], false).unwrap();
    assert_eq!(
        position[0].text["detail"]["coordinate"],
        json!([100.0, 50.0])
    );
}

#[test]
fn outside_the_screen_is_refused() {
    let fake = FakeCompositor::start((800, 600));
    fake.computer.run(&[act("screenshot")], false).unwrap();
    let click = Action {
        coordinate: Some([900.0, 10.0]),
        ..act("left_click")
    };
    assert!(fake.computer.run(&[click], false).is_err());
}

#[test]
fn modifiers_are_held_around_the_key() {
    let fake = FakeCompositor::start((800, 600));
    let key = Action {
        text: Some("ctrl+s".into()),
        no_screenshot: Some(true),
        ..act("key")
    };
    fake.computer.run(&[key], false).unwrap();
    assert_eq!(
        fake.ops(),
        vec![
            json!({"op":"key_down","key":"Control"}),
            json!({"op":"key","key":"s"}),
            json!({"op":"key_up","key":"Control"}),
        ]
    );
}

#[test]
fn a_refused_step_releases_held_modifiers_and_stops_the_batch() {
    let fake = FakeCompositor::start((800, 600));
    let actions = vec![
        Action {
            text: Some("hello".into()),
            ..act("type")
        },
        Action {
            text: Some("ctrl+Unmapped".into()),
            ..act("key")
        },
        act("screenshot"),
    ];
    // "Unmapped" is not a key this host knows, so it is refused locally.
    let error = fake.computer.run(&actions, true).err().unwrap();
    assert!(error.starts_with("[1]"), "{error}");
    let ops = fake.ops();
    assert_eq!(ops, vec![json!({"op":"type","text":"hello"})]);
}

#[test]
fn a_batch_returns_one_frame_at_the_end() {
    let fake = FakeCompositor::start((800, 600));
    let actions = vec![
        act("screenshot"),
        Action {
            coordinate: Some([400.0, 300.0]),
            scroll_direction: Some("down".into()),
            scroll_amount: Some(2),
            ..act("scroll")
        },
        Action {
            text: Some("x".into()),
            ..act("type")
        },
    ];
    let outcomes = fake.computer.run(&actions, true).unwrap();
    let frames: Vec<bool> = outcomes.iter().map(|o| o.image.is_some()).collect();
    assert_eq!(frames, vec![true, false, true]);
    let ops = fake.ops();
    assert!(ops.contains(&json!({"op":"scroll","dx":0.0,"dy":-2.0})));
    let content = content(&outcomes, true);
    let blocks = content["content"].as_array().unwrap();
    assert!(blocks[0]["text"].as_str().unwrap().starts_with("[0]"));
    assert_eq!(
        blocks
            .iter()
            .filter(|block| block["type"] == "image")
            .count(),
        2
    );
}

#[test]
fn zoom_returns_the_region_at_frame_size() {
    let fake = FakeCompositor::start((800, 600));
    fake.computer.run(&[act("screenshot")], false).unwrap();
    let zoom = Action {
        region: Some([0.0, 0.0, 200.0, 150.0]),
        format: Some("png".into()),
        ..act("zoom")
    };
    let outcome = fake.computer.run(&[zoom], false).unwrap();
    let frame = outcome[0].image.as_ref().unwrap();
    assert_eq!(frame.mime, "image/png");
    let decoded = image::load_from_memory(&frame.data).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (800, 600));
}

#[test]
fn drags_move_the_cursor_to_their_end() {
    let fake = FakeCompositor::start((800, 600));
    fake.computer.run(&[act("screenshot")], false).unwrap();
    fake.ops();
    let drag = Action {
        start_coordinate: Some([10.0, 20.0]),
        coordinate: Some([300.0, 400.0]),
        no_screenshot: Some(true),
        ..act("left_click_drag")
    };
    fake.computer.run(&[drag], false).unwrap();
    assert_eq!(
        fake.ops(),
        vec![json!({"op":"drag","x_from":10.0,"y_from":20.0,"x_to":300.0,"y_to":400.0,"steps":12})]
    );
    let position = fake.computer.run(&[act("cursor_position")], false).unwrap();
    assert_eq!(
        position[0].text["detail"]["coordinate"],
        json!([300.0, 400.0])
    );
}

#[test]
fn a_drag_with_pressure_is_drawn_with_a_pen() {
    let fake = FakeCompositor::start((800, 600));
    fake.computer.run(&[act("screenshot")], false).unwrap();
    fake.ops();
    let stroke: Action = serde_json::from_value(json!({
        "action": "left_click_drag",
        "start_coordinate": [10.0, 20.0],
        "coordinate": [300.0, 400.0],
        "pressure": [0.2, 0.9, 0.4],
        "no_screenshot": true
    }))
    .unwrap();
    let constant: Action = serde_json::from_value(json!({
        "action": "left_click_drag",
        "coordinate": [10.0, 20.0],
        "pressure": 0.6,
        "no_screenshot": true
    }))
    .unwrap();
    fake.computer.run(&[stroke, constant], true).unwrap();
    assert_eq!(
        fake.ops(),
        vec![
            json!({"op":"drag","x_from":10.0,"y_from":20.0,"x_to":300.0,"y_to":400.0,"steps":48,"pressure":[0.2,0.9,0.4]}),
            json!({"op":"drag","x_from":300.0,"y_from":400.0,"x_to":10.0,"y_to":20.0,"steps":48,"pressure":[0.6]}),
        ]
    );
}

fn agent(id: &str) -> Agent {
    Agent {
        id: id.into(),
        label: format!("Agent {id}"),
    }
}

#[test]
fn every_command_names_the_agent_it_acts_for() {
    let fake = FakeCompositor::start((800, 600));
    let key = Action {
        text: Some("shift+a".into()),
        no_screenshot: Some(true),
        ..act("key")
    };
    fake.computer
        .run_as(Some(&agent("a")), &[key], false)
        .unwrap();
    let ops = fake.ops();
    assert_eq!(ops.len(), 3);
    for op in &ops {
        assert_eq!(op["agent"], "a", "{op}");
        assert_eq!(op["agent_label"], "Agent a", "{op}");
    }
    fake.computer.run(&[act("screenshot")], false).unwrap();
    assert!(
        fake.ops().iter().all(|op| op.get("agent").is_none()),
        "a run for nobody names nobody"
    );
}

#[test]
fn each_agent_acts_in_the_frame_of_its_own_strip() {
    // The compositor crops each agent's snapshot to its strip: here `a`
    // works in a short strip and `b` in a tall one.
    let fake = FakeCompositor::start_sized(|command| match command["agent"].as_str() {
        Some("a") => (800, 200),
        _ => (800, 600),
    });
    let (a, b) = (agent("a"), agent("b"));
    let shot_a = fake
        .computer
        .run_as(Some(&a), &[act("screenshot")], false)
        .unwrap();
    assert_eq!(shot_a[0].text["height"], 200);
    let click = |y| Action {
        coordinate: Some([10.0, y]),
        no_screenshot: Some(true),
        ..act("left_click")
    };
    assert!(
        fake.computer
            .run_as(Some(&b), &[click(10.0)], false)
            .is_err(),
        "b has not seen its strip yet; a's screenshot is not b's"
    );
    fake.computer
        .run_as(Some(&b), &[act("screenshot")], false)
        .unwrap();
    fake.computer
        .run_as(Some(&b), &[click(500.0)], false)
        .unwrap();
    assert!(
        fake.computer
            .run_as(Some(&a), &[click(500.0)], false)
            .is_err(),
        "500 is outside a's strip even though it is inside b's"
    );
    let position = fake
        .computer
        .run_as(Some(&a), &[act("cursor_position")], false)
        .unwrap();
    assert_eq!(
        position[0].text["detail"]["coordinate"],
        json!([0.0, 0.0]),
        "b's click did not move a's pointer"
    );
}

#[test]
fn seating_and_leaving_are_one_command_each() {
    let fake = FakeCompositor::start((800, 600));
    let a = agent("a");
    fake.computer
        .run_as(Some(&a), &[act("screenshot")], false)
        .unwrap();
    fake.ops();
    fake.computer.seat(&a).unwrap();
    fake.computer.leave(&a).unwrap();
    assert_eq!(
        fake.ops(),
        vec![
            json!({"op":"agent_seat","agent":"a","agent_label":"Agent a"}),
            json!({"op":"agent_leave","agent":"a","agent_label":"Agent a"}),
        ]
    );
    let click = Action {
        coordinate: Some([10.0, 10.0]),
        ..act("left_click")
    };
    assert!(
        fake.computer.run_as(Some(&a), &[click], false).is_err(),
        "a left: its last frame is forgotten with it"
    );
}

/// A compositor whose canvas window is larger than a screen frame, and
/// whose screen is ordinary.
fn with_window() -> FakeCompositor {
    FakeCompositor::start_sized(|command| match command["window"].as_str() {
        Some("so.tilo.canvas") => (1600, 1000),
        _ => (800, 600),
    })
}

fn in_canvas(action: Action) -> Action {
    Action {
        app_id: Some("so.tilo.canvas".into()),
        no_screenshot: Some(true),
        ..action
    }
}

#[test]
fn a_window_is_acted_on_in_its_own_pixels_without_a_screenshot() {
    let fake = with_window();
    let click = in_canvas(Action {
        coordinate: Some([1500.0, 900.0]),
        ..act("left_click")
    });
    fake.computer.run(&[click], false).unwrap();
    let window = json!("so.tilo.canvas");
    assert_eq!(
        fake.ops(),
        vec![
            json!({"op":"agent_seat","window":window}),
            json!({"op":"pointer_move","x":1500.0,"y":900.0,"window":window}),
            json!({"op":"pointer_button","pressed":true,"button":"left","window":window}),
            json!({"op":"pointer_button","pressed":false,"button":"left","window":window}),
        ],
        "the compositor offsets a window's coordinates; this side passes them through unscaled"
    );
    let shot = fake
        .computer
        .run(
            &[Action {
                app_id: Some("so.tilo.canvas".into()),
                ..act("screenshot")
            }],
            false,
        )
        .unwrap();
    assert_eq!(
        (&shot[0].text["width"], &shot[0].text["height"]),
        (&json!(1600), &json!(1000)),
        "a window frame keeps the window's pixels"
    );
    assert_eq!(shot[0].text["app_id"], "so.tilo.canvas");
    let screen = fake
        .computer
        .run(
            &[Action {
                coordinate: Some([10.0, 10.0]),
                ..act("left_click")
            }],
            false,
        )
        .err()
        .unwrap();
    assert!(
        screen.contains("screenshot"),
        "a window's frame is no frame of the screen: {screen}"
    );
}

#[test]
fn a_point_outside_the_window_says_how_large_it_is() {
    let fake = with_window();
    let click = in_canvas(Action {
        coordinate: Some([1700.0, 10.0]),
        ..act("left_click")
    });
    let error = fake.computer.run(&[click], false).err().unwrap();
    assert!(
        error.contains("so.tilo.canvas's window, which is 1600x1000"),
        "{error}"
    );
}

#[test]
fn a_modifier_is_held_through_a_drag() {
    let fake = with_window();
    let drag = in_canvas(Action {
        start_coordinate: Some([10.0, 20.0]),
        coordinate: Some([300.0, 400.0]),
        modifier: Some("shift".into()),
        ..act("left_click_drag")
    });
    fake.computer.run(&[drag], false).unwrap();
    let ops: Vec<String> = fake
        .ops()
        .iter()
        .filter(|op| op["op"] != "agent_seat")
        .map(|op| format!("{} {}", op["op"].as_str().unwrap(), op["key"]))
        .collect();
    assert_eq!(
        ops,
        vec!["key_down \"Shift\"", "drag null", "key_up \"Shift\"",]
    );
}

#[test]
fn a_screenshot_after_input_is_one_request_the_compositor_settles() {
    let fake = with_window();
    let click = Action {
        app_id: Some("so.tilo.canvas".into()),
        coordinate: Some([10.0, 10.0]),
        ..act("left_click")
    };
    fake.computer.run(&[click], false).unwrap();
    let ops: Vec<String> = fake
        .ops()
        .iter()
        .filter_map(|op| op["op"].as_str().map(str::to_owned))
        .filter(|op| op != "agent_seat")
        .collect();
    assert!(
        !ops.iter().any(|op| op == "foreign_frames"),
        "the compositor knows when the window answered; nothing is polled: {ops:?}"
    );
    assert_eq!(ops.last().map(String::as_str), Some("snapshot"));
}

#[test]
fn a_key_held_down_spans_the_steps_until_it_is_let_go() {
    let fake = with_window();
    let key = |action: &str| {
        in_canvas(Action {
            text: Some("space".into()),
            ..act(action)
        })
    };
    let drag = in_canvas(Action {
        start_coordinate: Some([10.0, 20.0]),
        coordinate: Some([300.0, 400.0]),
        no_screenshot: Some(true),
        ..act("left_click_drag")
    });
    fake.computer
        .run(&[key("key_down"), drag, key("key_up")], true)
        .unwrap();
    let ops: Vec<String> = fake
        .ops()
        .iter()
        .filter(|op| !matches!(op["op"].as_str(), Some("agent_seat" | "snapshot")))
        .map(|op| format!("{} {}", op["op"].as_str().unwrap(), op["key"]))
        .collect();
    assert_eq!(
        ops,
        vec!["key_down \"Space\"", "drag null", "key_up \"Space\""]
    );
}
