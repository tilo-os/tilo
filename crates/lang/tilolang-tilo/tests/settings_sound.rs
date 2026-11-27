//! The Sound pane over the device's sound reading and the person's audio settings: choosing a
//! device keeps it, the sound theme and each event's switch are there, and an output can be
//! made to say which one it is.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use tilolang_runtime::delegate::Invocation;
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

const APP: &str = "so.tilo.settings";

use crate::invoker::{Answered as _, Sent};

fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

fn settings() -> Host {
    crate::fixtures::settings_host().0
}

/// Where this device's sound plays and records: what the audio service publishes.
fn sound() -> ResourceRef {
    ResourceRef::parse_id("sound", "so.tilo.device#Sound").expect("the sound's address")
}

/// The person's audio settings, as the session keeps them.
fn audio() -> ResourceRef {
    ResourceRef::parse_id("settings/audio", "so.tilo.settings.sections#Audio")
        .expect("the audio settings' address")
}

/// What the device's sound is: speakers playing, headphones and a microphone to choose, and the
/// two sound themes installed.
const SOUND: &str = r#"{
  "output": 1,
  "volume": 0.6,
  "muted": false,
  "input": 3,
  "input-volume": 0.7,
  "input-muted": false,
  "outputs": [
    {"node": 1, "name": "alsa_output.speakers", "label": "Speakers", "chosen": true,
     "volume": 0.6, "muted": false, "channels": 2, "codec": null},
    {"node": 2, "name": "alsa_output.headphones", "label": "Headphones", "chosen": false,
     "volume": 0.6, "muted": false, "channels": 2, "codec": null}
  ],
  "inputs": [
    {"node": 3, "name": "alsa_input.microphone", "label": "Built-in Microphone", "chosen": true,
     "volume": 0.7, "muted": false, "channels": 1, "codec": null}
  ],
  "streams": [],
  "uses": [],
  "hardware": [],
  "themes": [
    {"id": "freedesktop", "name": "Freedesktop"},
    {"id": "ocean", "name": "Ocean"}
  ],
  "mono-output": true,
  "echo-cancellation": false,
  "pending": [],
  "problem": null
}"#;

/// The audio settings as the person left them: system sounds from the freedesktop theme, every
/// event on but new messages.
const AUDIO: &str = r#"{
  "system_sounds": {
    "enabled": true,
    "volume": 0.7,
    "theme": null,
    "events": {"notification": true, "message": false, "volume_change": true,
               "screenshot": true, "device_connected": true, "device_disconnected": true,
               "low_battery": true, "error": true}
  },
  "balance": 0.5,
  "mono_output": false,
  "echo_cancellation": false
}"#;

/// The pane as its view draws it, over the readings above.
fn sound_pane(host: &Host) -> ViewInstance {
    host.runtime()
        .set_reading(&sound(), SOUND)
        .expect("the sound reading is set");
    host.runtime()
        .set_reading(&audio(), AUDIO)
        .expect("the audio settings are set");
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .views
        .iter()
        .find(|view| view.name == "sound_pane")
        .expect("the pane is declared")
        .id
        .clone();
    let mut view = host
        .runtime()
        .open_view(&me(), &id, Vec::new())
        .expect("the pane opens");
    view.poll_patches().expect("the pane draws");
    view
}

/// The rows the pane drew, each with its title.
fn row<'a>(tree: &'a UiNode, title: &str) -> &'a UiNode {
    tree.of_kind("ui.ListRow")
        .into_iter()
        .find(|node| node.props.get("title") == Some(&Value::text(title)))
        .unwrap_or_else(|| panic!("a row titled {title}"))
}

/// Every text the pane shows, as written.
fn texts(view: &ViewInstance) -> Vec<String> {
    crate::fixtures::nodes(view.tree())
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            Value::Rich(rich) => Some(rich.written_text()),
            _ => None,
        })
        .collect()
}

#[test]
fn choosing_an_output_or_input_plays_it_and_keeps_it() {
    let host = settings();
    let view = sound_pane(&host);
    let sent = Sent::default();
    host.runtime().set_invoker(sent.clone());

    let headphones = row(view.tree(), "Headphones").key.clone();
    view.fire_answering(&host, &me(), &headphones, "activate")
        .expect("the row chooses");
    let microphone = row(view.tree(), "Built-in Microphone").key.clone();
    view.fire_answering(&host, &me(), &microphone, "activate")
        .expect("the row chooses");

    let sent = sent.all();
    assert_eq!(sent.len(), 4, "two changes for each choice: {sent:?}");
    // The action's arguments by name, but the resource it runs on, which the
    // runtime carries first.
    let action = |at: usize| match &sent[at] {
        Invocation::Action {
            app, name, args, ..
        } => (
            app.as_str().to_owned(),
            name.clone(),
            args.iter()
                .filter(|(_, value)| !matches!(value, Value::Ref(_)))
                .map(|(named, value)| (named.clone(), value.clone()))
                .collect::<Vec<_>>(),
        ),
        invocation @ Invocation::Edit { .. } => panic!("not an action: {invocation:?}"),
    };
    let (app, name, args) = action(0);
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("so.tilo.device", "route-output")
    );
    assert_eq!(
        args,
        [("node".to_owned(), Value::Int(2))],
        "the output plays"
    );
    let (app, name, args) = action(1);
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("so.tilo.settings.sections", "remember-output-device"),
        "and is kept for the next session"
    );
    assert_eq!(args, [("device".to_owned(), Value::text("Headphones"))]);
    let (_, name, args) = action(2);
    assert_eq!(name.as_str(), "select-input");
    assert_eq!(args, [("node".to_owned(), Value::Int(3))]);
    let (_, name, args) = action(3);
    assert_eq!(name.as_str(), "remember-input-device");
    assert_eq!(
        args,
        [("device".to_owned(), Value::text("Built-in Microphone"))]
    );
}

#[test]
fn the_sound_theme_and_each_event_are_chosen_in_the_pane() {
    let host = settings();
    let view = sound_pane(&host);
    let shown = texts(&view);
    for words in [
        "Theme",
        "Notifications",
        "New messages",
        "Volume change feedback",
        "Screenshot shutter",
        "Device connected",
        "Device disconnected",
        "Low battery",
        "Errors",
    ] {
        assert!(shown.iter().any(|text| text == words), "{words}: {shown:?}");
    }
    let sent = Sent::default();
    host.runtime().set_invoker(sent.clone());

    // The picker shows the theme in use and offers the installed ones by name.
    let picker = view
        .tree()
        .of_kind("ui.Dropdown")
        .into_iter()
        .find(|node| node.props.get("caption") == Some(&Value::text("Sound theme")))
        .expect("a theme picker");
    assert_eq!(
        picker.props.get("value"),
        Some(&Value::text("Freedesktop")),
        "nothing chosen follows the freedesktop theme"
    );
    let picker = picker.key.clone();
    view.fire_with_answering(&host, &me(), &picker, "change", Some(&Value::text("Ocean")))
        .expect("a theme is chosen");
    // Each event's switch is bound to that event's own choice, in the record
    // the section keeps: the one left off is new messages.
    let message = row(view.tree(), "New messages")
        .of_kind("ui.Toggle")
        .first()
        .expect("a switch for new messages")
        .key
        .clone();
    assert_eq!(
        view.bound_value(&message),
        Some(Value::Bool(false)),
        "the switch shows the event's own choice"
    );
    view.edit(&message, &Value::Bool(true))
        .expect("the event is switched on");

    let sent = sent.all();
    let [
        Invocation::Action { name, args, .. },
        Invocation::Edit {
            path, field, value, ..
        },
    ] = sent.as_slice()
    else {
        panic!("a theme and a switch: {sent:?}");
    };
    assert_eq!(name.as_str(), "set-system-sound-theme");
    assert_eq!(
        args.iter()
            .find(|(named, _)| named == "theme")
            .map(|(_, value)| value.clone()),
        Some(Value::Opt(Some(Box::new(Value::text("Ocean"))))),
        "the theme is chosen by the name the picker shows"
    );
    assert_eq!(
        path.as_slice(),
        ["system_sounds".to_owned(), "events".to_owned()]
    );
    assert_eq!(field.as_str(), "message");
    assert_eq!(value, &Value::Bool(true));
}

#[test]
fn each_output_plays_a_test_sound_of_its_own() {
    let host = settings();
    let view = sound_pane(&host);
    let sent = Sent::default();
    host.runtime().set_invoker(sent.clone());

    // Every output row offers to say which one it is, and the one playing
    // offers a side of its own.
    for title in ["Speakers", "Headphones"] {
        let output_row = row(view.tree(), title);
        let test = output_row
            .of_kind("ui.Button")
            .into_iter()
            .find(|button| {
                button.props.get("label")
                    == Some(&Value::text(format!("Play a test sound on {title}")))
            })
            .unwrap_or_else(|| panic!("{title} plays a test sound"));
        view.press_answering(&host, &me(), &test.key.clone())
            .expect("the tone plays");
    }
    for side in ["Left", "Right"] {
        let button = view
            .tree()
            .of_kind("ui.Button")
            .into_iter()
            .find(|button| button.props.get("label") == Some(&Value::text(side)))
            .unwrap_or_else(|| panic!("the speaker test's {side}"));
        view.press_answering(&host, &me(), &button.key.clone())
            .expect("the side plays");
    }

    let asked: Vec<(String, Vec<(String, Value)>)> = sent
        .all()
        .into_iter()
        .filter_map(|invocation| match invocation {
            Invocation::Action { name, args, .. } => Some((name, args)),
            Invocation::Edit { .. } => None,
        })
        .collect();
    let names: Vec<&str> = asked.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        ["test-output", "test-output", "test-output", "test-output"]
    );
    let node = |at: usize| {
        asked[at]
            .1
            .iter()
            .find(|(named, _)| named == "node")
            .map(|(_, value)| value.clone())
    };
    let channel = |at: usize| {
        asked[at]
            .1
            .iter()
            .find(|(named, _)| named == "channel")
            .and_then(|(_, value)| match value {
                Value::Opt(Some(inner)) => Some((**inner).clone()),
                Value::Opt(None) => None,
                other => Some(other.clone()),
            })
    };
    assert_eq!(node(0), Some(Value::Int(1)), "the chosen output");
    assert_eq!(channel(0), None, "both ears");
    assert_eq!(node(1), Some(Value::Int(2)), "the other output");
    assert_eq!(channel(1), None);
    assert_eq!(
        (node(2), channel(2)),
        (Some(Value::Int(1)), Some(Value::text("left")))
    );
    assert_eq!(
        (node(3), channel(3)),
        (Some(Value::Int(1)), Some(Value::text("right")))
    );
}
