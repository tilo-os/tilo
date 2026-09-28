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
                } else {
                    if command["op"] == "snapshot" {
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
