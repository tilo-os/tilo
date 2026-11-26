//! Chat's calls: who talks rings in the sidebar, how loud each person plays is this device's
//! own and is played the moment it is set and again on every join, and joining leaves the
//! microphone off.

use tilolang_runtime::{Actor, CallMember, CallStep, InvokeOutcome, UiNode, Value, ViewEvent};

use crate::fixtures::{Replica, Workspace, target, type_id};

const CHAT: &str = include_str!("../../../../apps/tl/chat/app.tl");
const CHAT_IDS: &str = include_str!("../../../../apps/tl/chat/app.ids");
const APP: &str = "so.tilo.chat";

fn person(name: &str) -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person(name))
}

fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    r.call(APP, action, args)
}

fn member(name: &str, mine: bool, speaking: bool) -> CallMember {
    CallMember {
        who: person(name),
        mine,
        speaking,
        muted: !speaking,
        deafened: false,
        camera: false,
        screen: false,
        volume: 100,
        bridged: None,
    }
}

/// A voice channel called Lounge, with `people` in its call.
fn lounge(r: &Replica, people: &[CallMember]) -> Value {
    let all = Value::resource(&r.host.root_of(APP).unwrap());
    r.commit();
    let lounge = call(r, "new_voice_channel", vec![all, Value::text("Lounge")]);
    r.commit();
    let place = tilolang_runtime::platform::call_place(&target(&lounge));
    r.host.runtime().set_call(&place, people);
    lounge
}

fn label(node: &UiNode) -> Option<String> {
    match node.props.get("label") {
        Some(Value::Text(text)) => Some(text.clone()),
        _ => None,
    }
}

/// Whether the flag `name` of `node` is set.
fn flag(node: &UiNode, name: &str) -> bool {
    match node.props.get(name) {
        Some(Value::Bool(on)) => *on,
        Some(Value::Int(n)) => *n != 0,
        _ => false,
    }
}

fn keyed(node: &UiNode, kind: &str, label_is: &str) -> String {
    node.of_kind(kind)
        .into_iter()
        .find(|n| label(n).as_deref() == Some(label_is))
        .unwrap_or_else(|| panic!("no {kind} {label_is}"))
        .key
        .clone()
}

/// The steps the window asked of the system's call.
fn steps(view: &mut tilolang_runtime::ViewInstance) -> Vec<CallStep> {
    view.poll_events()
        .into_iter()
        .filter_map(|event| match event {
            ViewEvent::Call { step, .. } => Some(step),
            _ => None,
        })
        .collect()
}

/// Mutes bo for ada alone, then brings them back at the level they had: a device's own choice,
/// kept for it and played at once.
fn mute_and_restore(ada: &Replica, voice: &mut tilolang_runtime::ViewInstance) {
    let silence = keyed(voice.tree(), "ui.MenuItem", "Mute for me");
    voice.fire(&silence, "choose").unwrap();
    ada.commit();
    assert_eq!(
        steps(voice),
        vec![CallStep::Volume {
            member: "bo".into(),
            percent: 0
        }]
    );
    voice.poll_patches().unwrap();
    assert!(
        voice
            .tree()
            .of_kind("ui.MenuButton")
            .iter()
            .any(|b| label(b).as_deref() == Some("Muted for you")),
    );
    let silence = keyed(voice.tree(), "ui.MenuItem", "Mute for me");
    voice.fire(&silence, "choose").unwrap();
    ada.commit();
    assert_eq!(
        steps(voice),
        vec![CallStep::Volume {
            member: "bo".into(),
            percent: 50
        }]
    );
}

#[test]
fn whoever_talks_in_a_call_rings_under_its_channel() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    lounge(
        &ada,
        &[member("ada", true, false), member("bo", false, true)],
    );
    let sidebar = ada.view(APP);
    let rows = sidebar.tree().of_kind("ui.NavItem");
    let bo = rows
        .iter()
        .find(|row| row.props.get("person") == Some(&Value::Actor(person("bo"))))
        .expect("bo's row under the channel");
    assert!(
        flag(bo, "speaking"),
        "the talking person's face rings: {:?}",
        bo.props
    );
    let ada_row = rows
        .iter()
        .find(|row| row.props.get("person") == Some(&Value::Actor(person("ada"))))
        .expect("ada's row under the channel");
    assert!(!flag(ada_row, "speaking"), "a listener's does not");
    assert!(
        !format!("{:?}", bo.props.get("secondary")).contains("Speaking"),
        "the ring says it, not a word under the name"
    );
}

#[test]
fn how_loud_someone_plays_is_this_devices_own_and_plays_at_once_and_on_joining() {
    let (ada, loaded) =
        Replica::on_device("ada", "ada-laptop", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    let meta = loaded[0].meta.clone();
    let channel = lounge(
        &ada,
        &[member("ada", true, false), member("bo", false, true)],
    );
    let mut voice = ada.host.open_resource(&target(&channel), None).unwrap();
    assert!(
        voice
            .tree()
            .of_kind("ui.MenuButton")
            .iter()
            .any(|b| label(b).as_deref() == Some("Volume 100%")),
        "bo plays as they arrive until the person sets otherwise"
    );

    // Half as loud: kept for this device and played at once.
    let half = keyed(voice.tree(), "ui.MenuItem", "Volume 50%");
    let fired = voice.fire(&half, "choose").unwrap();
    assert!(matches!(fired, InvokeOutcome::Done(_)), "{fired:?}");
    ada.commit();
    assert_eq!(
        steps(&mut voice),
        vec![CallStep::Volume {
            member: "bo".into(),
            percent: 50
        }],
        "the call plays bo at half at once"
    );
    let kept = tilolang_runtime::ResourceRef::root(
        format!(
            "devices/ada-laptop/{APP}/root-{}",
            type_id(&meta, "Hearing")
        ),
        type_id(&meta, "Hearing"),
    );
    let people = ada.read(&kept, "people");
    assert!(
        matches!(&people, Value::List(people) if people.len() == 1),
        "the level is kept under this device: {people:?}"
    );
    voice.poll_patches().unwrap();
    assert!(
        voice
            .tree()
            .of_kind("ui.MenuButton")
            .iter()
            .any(|b| label(b).as_deref() == Some("Volume 50%")),
        "and read back from it"
    );

    // Muted for ada alone, and back at the level they had.
    mute_and_restore(&ada, &mut voice);

    // Out of the call, joining joins muted and plays bo at the kept level: the microphone is
    // never turned on by joining.
    ada.host.runtime().set_call(
        &tilolang_runtime::platform::call_place(&target(&channel)),
        &[member("bo", false, true)],
    );
    voice.poll_patches().unwrap();
    // The system's call says its words through the window's own catalogue, which this host
    // has not: the button reads as its phrase.
    let join = voice
        .tree()
        .of_kind("ui.Button")
        .into_iter()
        .find(|b| {
            matches!(
                label(b).as_deref(),
                Some("Join muted" | "tilolang-call-join")
            )
        })
        .expect("the call's Join muted")
        .key
        .clone();
    voice.fire(&join, "press").unwrap();
    assert_eq!(
        steps(&mut voice),
        vec![
            CallStep::Join,
            CallStep::Volume {
                member: "bo".into(),
                percent: 50
            }
        ],
        "joining asks no unmute, and plays bo as this device keeps them"
    );
}
