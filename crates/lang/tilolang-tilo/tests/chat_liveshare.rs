//! A window shared live into a conversation. Chat asks the shell to share one of the person's
//! windows, as the window's own bar does, and posts a message that refers to the share: the
//! message shows the window live, as the shell pictures it, for the sharer and for every member
//! whose shell shows the share. The sharer switches what the others may do and stops the share
//! from the message, which then says the window is no longer shared, for everyone.

use serde_json::json;
use tilolang_runtime::{Actor, InvokeOutcome, ResourceRef, UiNode, Value, ViewInstance};

use crate::fixtures::invoker::{Answered as _, Sent};
use crate::fixtures::{Replica, Workspace, target};

const CHAT: &str = include_str!("../../../../apps/tl/chat/app.tl");
const CHAT_IDS: &str = include_str!("../../../../apps/tl/chat/app.ids");
const APP: &str = "so.tilo.chat";

/// The window Ada shares: a sheet open on her laptop.
const WINDOW: &str = "7";
/// Where Ben's shell shows Ada's window: a place of its own.
const BENS_PLACE: &str = "41";

fn windows() -> ResourceRef {
    ResourceRef::parse_id("session:windows", "so.tilo.device#Windows").expect("a windows root")
}

fn shares() -> ResourceRef {
    ResourceRef::parse_id("session:window-share", "so.tilo.device#Shares").expect("a shares root")
}

/// One window as the session lists it.
fn window(key: &str, title: &str, app: &str, elsewhere: bool) -> serde_json::Value {
    json!({
        "key": key, "title": title, "origin_app": app, "app_key": "so.tilo.sheets",
        "icon": "table", "strip": "S1", "strip_key": "s1", "workspace": "Team",
        "workspace_key": "w1", "group_key": key, "tab_index": 0, "share": 1.0,
        "elsewhere": elsewhere, "device": "", "device_name": "", "place": 1, "recency": 1,
        "focused": false, "preview": "live", "width": 0, "height": 0,
        "not_responding": false, "attention": false, "busy": false, "modified": false,
        "presence": [], "fullscreen": false,
        "floating": false, "renameable": false, "opens_tabs": false, "controls_desktop": false,
        "display_id": "screen-1", "bar_location": "", "bar_field": "", "bar_typing": false,
        "find": null, "bar": null, "author": null, "ended": null,
    })
}

/// What a device's session publishes: its windows, the ones it shares and the ones it shows.
fn device(r: &Replica, all: &[serde_json::Value], sharing: &[&str], watching: &[(&str, &str)]) {
    let runtime = r.host.runtime();
    runtime
        .set_reading(
            &windows(),
            &json!({ "all": all, "displays": [] }).to_string(),
        )
        .expect("the windows reading is set");
    let sharing: Vec<_> = sharing
        .iter()
        .map(|tab| {
            json!({
                "tab": tab, "app_id": "so.tilo.sheets", "workspace": "w1",
                "input_allowed": false, "streaming": true, "waiting_for": "", "viewers": [],
            })
        })
        .collect();
    let watching: Vec<_> = watching
        .iter()
        .map(|(tab, shown_in)| {
            json!({
                "tab": tab, "workspace": "w1", "host": "laptop", "shown_in": shown_in,
                "waiting_for": "",
            })
        })
        .collect();
    runtime
        .set_reading(
            &shares(),
            &json!({ "sharing": sharing, "watching": watching }).to_string(),
        )
        .expect("the shares reading is set");
}

fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    r.call(APP, action, args)
}

fn person(r: &Replica) -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person(&r.name))
}

/// Open `at` as the system does: its opened action, then its window.
fn open(r: &Replica, at: &ResourceRef) -> ViewInstance {
    if let Some(action) = r.host.runtime().opened_action(&at.ty) {
        match r
            .host
            .runtime()
            .invoke_action(&person(r), (action.as_str(), vec![Value::resource(at)]))
            .unwrap()
        {
            InvokeOutcome::Done(_) => {}
            other => panic!("opening {at:?}: {other:?}"),
        }
    }
    r.host.open_resource(at, None).unwrap()
}

/// Every text a node and its children show.
fn texts(node: &UiNode) -> Vec<String> {
    crate::fixtures::nodes(node)
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// The transcript's rows, each one's node.
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

/// The windows a row pictures live, by the key each preview names.
fn pictured(row: &UiNode) -> Vec<String> {
    row.of_kind("ui.WindowPreview")
        .iter()
        .flat_map(|preview| texts(preview))
        .collect()
}

/// The key of the node of `kind` in `node` labelled `label`.
fn labelled(node: &UiNode, kind: &str, label: &str) -> Option<String> {
    node.of_kind(kind)
        .iter()
        .find(|n| n.props.get("label") == Some(&Value::text(label)))
        .map(|n| n.key.clone())
}

/// Run what the control `key` of `view` does on `event`, as the person does, answering the
/// system's confirm where what it does leaves the system.
fn run(r: &Replica, view: &ViewInstance, key: &str, event: &str) {
    let outcome = view
        .fire_answering(&r.host, &person(r), key, event)
        .unwrap_or_else(|why| panic!("{key} on {event}: {why}"));
    assert!(
        matches!(outcome, InvokeOutcome::Done(_)),
        "{event}: {outcome:?}"
    );
    r.commit();
}

/// The argument `name` of a verb as it was sent, unwrapped of its option.
fn arg(args: &[(String, Value)], name: &str) -> Option<Value> {
    args.iter()
        .find(|(named, _)| named == name)
        .map(|(_, value)| match value {
            Value::Opt(Some(inner)) => (**inner).clone(),
            other => other.clone(),
        })
}

/// Whether a sent flag says yes: the runtime carries a `Bool` as the number it stores it as, and
/// the verb's own parameter type reads it back.
fn truth(value: Value) -> bool {
    match value {
        Value::Bool(yes) => yes,
        Value::Int(number) => number != 0,
        other => panic!("not a flag: {other:?}"),
    }
}

#[test]
fn a_window_shared_into_a_channel_shows_live_and_says_when_it_ended() {
    let (ada, loaded) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(
        &ada,
        "new_channel",
        vec![all, Value::text("design"), Value::text(""), Value::none()],
    );
    ada.commit();
    let ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);

    // Ada's laptop has the budget open, shared with nobody yet; Ben's shows nothing of hers.
    let sent = Sent::default();
    ada.host.runtime().set_foreign_invoker(sent.clone());
    device(&ada, &[window(WINDOW, "Budget", "Sheets", false)], &[], &[]);
    device(&ben, &[], &[], &[]);

    // The composer offers her windows to share, by the name the shell gives them; choosing one
    // asks the shell to share it, view only, and posts the message that refers to the share.
    let mut ada_design = open(&ada, &target(&design));
    ada_design.poll_patches().unwrap();
    let share = labelled(ada_design.tree(), "ui.MenuItem", "Share Budget")
        .expect("the composer offers the open window");
    run(&ada, &ada_design, &share, "choose");
    let asked = sent.take_calls();
    let names: Vec<&str> = asked.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["share", "allow-remote-input"], "{asked:?}");
    assert_eq!(arg(&asked[0].1, "tab"), Some(Value::text(WINDOW)));
    assert_eq!(arg(&asked[1].1, "on").map(truth), Some(false));

    // The shell shares it now: Ada's message shows her window live, view only, with the ways to
    // change that and to stop.
    device(
        &ada,
        &[window(WINDOW, "Budget", "Sheets", false)],
        &[WINDOW],
        &[],
    );
    let row = rows(&mut ada_design).pop().expect("the share's message");
    assert_eq!(pictured(&row), [WINDOW], "{:?}", texts(&row));
    let said = texts(&row);
    assert!(said.contains(&"Live".to_owned()), "{said:?}");
    assert!(said.contains(&"Sheets · View only".to_owned()), "{said:?}");
    assert!(
        labelled(&row, "ui.Button", "Stop sharing").is_some(),
        "{said:?}"
    );

    // Ben's shell shows Ada's window in a place of its own: the message shows it there, live,
    // and offers him nothing of the sharer's.
    ada.sync_with(&ben);
    let ben_sent = Sent::default();
    ben.host.runtime().set_foreign_invoker(ben_sent.clone());
    // Before his shell shows it, the message offers to: the shell's own watch of the share.
    let mut ben_design = open(&ben, &target(&design));
    let row = rows(&mut ben_design).pop().expect("the share's message");
    assert!(pictured(&row).is_empty(), "{:?}", texts(&row));
    let show = labelled(&row, "ui.Button", "Show it live").expect("a way to see it");
    run(&ben, &ben_design, &show, "press");
    let asked = ben_sent.take_calls();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].0, "watch");
    assert_eq!(arg(&asked[0].1, "tab"), Some(Value::text(WINDOW)));
    assert_eq!(arg(&asked[0].1, "workspace"), Some(Value::text("w1")));
    device(
        &ben,
        &[window(BENS_PLACE, "Budget", "Sheets", true)],
        &[],
        &[(WINDOW, BENS_PLACE)],
    );
    let row = rows(&mut ben_design).pop().expect("the share's message");
    assert_eq!(pictured(&row), [BENS_PLACE], "{:?}", texts(&row));
    assert!(texts(&row).contains(&"Live".to_owned()));
    assert!(labelled(&row, "ui.Button", "Stop sharing").is_none());
    assert!(labelled(&row, "ui.Segment", "Can edit").is_none());

    // Ada lets the others edit: the shell's share lets them control the window.
    let row = rows(&mut ada_design).pop().unwrap();
    let edit = labelled(&row, "ui.Segment", "Can edit").expect("the sharer's access switch");
    run(&ada, &ada_design, &edit, "press");
    let asked = sent.take_calls();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].0, "allow-remote-input");
    assert_eq!(arg(&asked[0].1, "on").map(truth), Some(true));
    ada.sync_with(&ben);
    let row = rows(&mut ben_design).pop().unwrap();
    assert!(
        texts(&row).contains(&"Sheets · Can edit".to_owned()),
        "{:?}",
        texts(&row)
    );

    // She stops sharing from the message: the shell's share ends, and the message says so in
    // the window's place, for her and for Ben.
    let row = rows(&mut ada_design).pop().unwrap();
    let stop = labelled(&row, "ui.Button", "Stop sharing").unwrap();
    run(&ada, &ada_design, &stop, "press");
    let asked = sent.take_calls();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].0, "stop");
    assert_eq!(arg(&asked[0].1, "tab"), Some(Value::text(WINDOW)));
    device(&ada, &[window(WINDOW, "Budget", "Sheets", false)], &[], &[]);
    ada.sync_with(&ben);
    for view in [&mut ada_design, &mut ben_design] {
        let row = rows(view).pop().unwrap();
        assert!(pictured(&row).is_empty(), "{:?}", texts(&row));
        let said = texts(&row);
        assert!(said.contains(&"No longer shared".to_owned()), "{said:?}");
        assert!(
            labelled(&row, "ui.Button", "Stop sharing").is_none(),
            "{said:?}"
        );
    }
}

/// A share stopped from the window's own bar, not from the message, ends there too: the sharer's
/// shell no longer shares the window, and the message stops showing it live.
#[test]
fn a_share_stopped_at_the_window_ends_in_the_message() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    let design = call(
        &ada,
        "new_channel",
        vec![all, Value::text("design"), Value::text(""), Value::none()],
    );
    ada.commit();
    ada.host.runtime().set_foreign_invoker(Sent::default());
    device(&ada, &[window(WINDOW, "Budget", "Sheets", false)], &[], &[]);
    call(
        &ada,
        "share_window",
        vec![design.clone(), Value::text(WINDOW), Value::Bool(false)],
    );
    ada.commit();
    device(
        &ada,
        &[window(WINDOW, "Budget", "Sheets", false)],
        &[WINDOW],
        &[],
    );
    let mut view = open(&ada, &target(&design));
    assert_eq!(pictured(&rows(&mut view).pop().unwrap()), [WINDOW]);

    device(&ada, &[window(WINDOW, "Budget", "Sheets", false)], &[], &[]);
    let row = rows(&mut view).pop().unwrap();
    assert!(pictured(&row).is_empty());
    assert!(
        texts(&row).contains(&"No longer shared".to_owned()),
        "{:?}",
        texts(&row)
    );
}
