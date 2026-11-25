//! What a problem report sent from the shell's box carries, as Chat receives it: the recording of
//! the last moments, which its row replays, and the picture of the window beside it. And a face
//! among a message's reactions, which opens that person's card.
//! The system's own words show as their ids here (`tilolang-person-moderator`): no words are given.

use crate::fixtures::{Replica, Workspace, target};
use tilo_actions::policy::Role;
use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
use tilolang_runtime::members::Member;
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::{Actor, InvokeOutcome, UiNode, Value, ViewInstance};

const CHAT: &str = include_str!("../../../../apps/tl/chat/app.tl");
const CHAT_IDS: &str = include_str!("../../../../apps/tl/chat/app.ids");
const APP: &str = "so.tilo.chat";

/// The device's content store, holding what the session kept of a report.
struct Device;

impl BlobHost for Device {
    fn read(&self, _hash: &str) -> Option<Vec<u8>> {
        Some(b"the last half minute".to_vec())
    }

    fn choose(&self, _request: &FileRequest) -> Result<Vec<FileValue>, String> {
        Ok(Vec::new())
    }

    fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
        Err("this test writes no files out".to_owned())
    }

    fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
        Ok(FileValue::new(
            name,
            media,
            i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            "cd".repeat(32),
        ))
    }
}

fn person(name: &str) -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person(name))
}

/// Run `action` as `who`, answering what it answered.
fn call_as(r: &Replica, who: &str, action: &str, args: Vec<Value>) -> Value {
    let meta = r.host.package(APP).unwrap().meta;
    match r
        .host
        .runtime()
        .invoke_action(
            &person(who),
            (crate::fixtures::action_id(&meta, action).as_str(), args),
        )
        .unwrap()
    {
        InvokeOutcome::Done(value) => value,
        other => panic!("{action}: {other:?}"),
    }
}

fn rich(text: &str) -> Value {
    Value::Rich(RichValue::from_text(text))
}

/// Every text a node and its children show.
fn texts(node: &UiNode) -> Vec<String> {
    crate::fixtures::nodes(node)
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            Value::Rich(rich) => Some(rich.written_text()),
            _ => None,
        })
        .collect()
}

/// The transcript's rows as the list builds them.
fn rows(view: &mut ViewInstance) -> Vec<UiNode> {
    view.poll_patches().unwrap();
    let list = view
        .tree()
        .first_of_kind("ui.VirtualList")
        .expect("a transcript")
        .key
        .clone();
    let len = view.list_len(&list).unwrap_or(0);
    (0..len)
        .filter_map(|i| view.row(&list, i).unwrap().cloned())
        .collect()
}

/// The key of the node of `kind` in `node` labelled `label`.
fn labelled(node: &UiNode, kind: &str, label: &str) -> Option<String> {
    node.of_kind(kind)
        .into_iter()
        .find(|n| n.props.get("label") == Some(&Value::text(label)))
        .map(|n| n.key.clone())
}

fn open(r: &Replica, at: &tilolang_runtime::ResourceRef) -> ViewInstance {
    if let Some(action) = r.host.runtime().opened_action(&at.ty) {
        match r
            .host
            .runtime()
            .invoke_action(
                &person(&r.name),
                (action.as_str(), vec![Value::resource(at)]),
            )
            .unwrap()
        {
            InvokeOutcome::Done(_) => {}
            other => panic!("opening {at:?}: {other:?}"),
        }
    }
    r.host.open_resource(at, None).unwrap()
}

fn workspace() -> (Replica, tilolang_runtime::ResourceRef) {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    ada.host.runtime().set_members(
        vec![
            Member::person("ada", Role::Owner),
            Member::person("ben", Role::Moderator),
            Member::person("cy", Role::Editor),
        ],
        None,
    );
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    (ada, root)
}

/// A report sent from the shell's box arrives as the session hands it over: the person's words,
/// the recording of the last moments as the report's own, which its row offers to replay, and the
/// picture of the window beside it. The files are said as the session says them, the JSON an
/// action's arguments travel in.
#[test]
fn a_report_from_the_shells_box_arrives_with_its_recording_to_replay_and_its_picture() {
    let (ada, root) = workspace();
    ada.host
        .give_content(&tilolang_tilo::content::WorkspaceContent {
            file_bytes: 50 * 1_048_576,
            member_bytes: 250 * 1_048_576,
            hourly_bytes: 250 * 1_048_576,
            refusals: Vec::new(),
            host_person: None,
        });
    ada.host.runtime().set_blobs(Device);
    // As the session says them: plain JSON, read the way the action API reads its arguments.
    let meta = ada.host.package(APP).expect("Chat runs").meta;
    let said = |json: serde_json::Value, ty: &str| ada.host.argument(&meta, &json, ty).unwrap();
    let recording = said(
        serde_json::json!({"file": {
            "name": "Recording.mp4",
            "media": "video/mp4",
            "size": 4_194_304,
            "hash": "ab".repeat(32),
        }}),
        "Option<File>",
    );
    let picture = said(
        serde_json::json!([{"file": {
            "name": "Screenshot.png",
            "media": "image/png",
            "size": 52_000,
            "hash": "cd".repeat(32),
        }}]),
        "List<File>",
    );
    assert!(
        matches!(&recording, Value::Opt(Some(file)) if matches!(**file, Value::File(_))),
        "the recording is the report's own: {recording:?}"
    );
    call_as(
        &ada,
        "ben",
        "report_problem",
        vec![
            Value::resource(&root),
            rich("The send button does nothing."),
            recording,
            picture,
        ],
    );
    ada.commit();

    let feedback = match ada.read(&ada.root(APP), "channels") {
        Value::List(items) => items.iter().map(target).next().expect("#feedback"),
        other => panic!("the app's channels: {other:?}"),
    };
    let mut channel = open(&ada, &feedback);
    let shown = rows(&mut channel);
    assert_eq!(shown.len(), 1, "the one report");
    let words = texts(&shown[0]);
    assert!(
        words.contains(&"The send button does nothing.".to_owned()),
        "{words:?}"
    );
    assert!(
        labelled(&shown[0], "ui.Chip", "Screenshot.png").is_some()
            || words.contains(&"Screenshot.png".to_owned()),
        "the picture of the window rides with it: {words:?}"
    );
    let replay = labelled(&shown[0], "ui.Button", "Replay Recording.mp4")
        .unwrap_or_else(|| panic!("the report's row offers its replay: {words:?}"));
    let played = channel.press(&replay).unwrap();
    assert!(
        matches!(played, InvokeOutcome::Done(_)),
        "the recording opens: {played:?}"
    );
}

/// Choosing a face among a message's reactions opens that person's card beside them: their name,
/// what they are in the workspace and what Chat does with someone. Dismissing it closes it.
#[test]
fn choosing_a_reactors_face_opens_their_card() {
    let (ada, root) = workspace();
    let design = call_as(
        &ada,
        "ada",
        "new_channel",
        vec![Value::resource(&root), Value::text("design")],
    );
    ada.commit();
    let message = call_as(
        &ada,
        "ada",
        "send",
        vec![design.clone(), rich("Shipping at four.")],
    );
    ada.commit();
    call_as(&ada, "ben", "react", vec![message, Value::text("🎉")]);
    ada.commit();

    let mut channel = open(&ada, &target(&design));
    let shown = rows(&mut channel);
    assert_eq!(shown.len(), 1);
    let strip = shown[0]
        .of_kind("ui.Reactions")
        .first()
        .map(|strip| strip.key.clone())
        .expect("the message's reactions");
    assert!(
        labelled(&shown[0], "ui.Popover", "About ben").is_none(),
        "no card before a face is chosen"
    );
    let chosen = channel
        .fire_with(&strip, "person", Some(&Value::Actor(person("ben"))))
        .unwrap();
    assert!(matches!(chosen, InvokeOutcome::Done(_)), "{chosen:?}");

    let shown = rows(&mut channel);
    let card = shown[0]
        .of_kind("ui.Popover")
        .into_iter()
        .find(|p| p.props.get("label") == Some(&Value::text("About ben")))
        .expect("Ben's card opens beside the reactions");
    // A literal `true` reaches the widget as the runtime's 1, which it reads as open.
    assert!(
        matches!(
            card.props.get("open"),
            Some(Value::Bool(true) | Value::Int(1))
        ),
        "{:?}",
        card.props.get("open")
    );
    let said = texts(card);
    for part in [
        "ben",
        "tilolang-person-moderator",
        "Message",
        "Report person",
    ] {
        assert!(
            said.contains(&part.to_owned()),
            "{part} on Ben's card: {said:?}"
        );
    }

    let key = card.key.clone();
    channel.fire(&key, "dismiss").unwrap();
    let shown = rows(&mut channel);
    assert!(
        labelled(&shown[0], "ui.Popover", "About ben").is_none(),
        "dismissed, the card closes"
    );
}
