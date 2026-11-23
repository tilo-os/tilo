//! What links to a resource: what was made from it, what mentions it and what names it in a
//! field, read from the store of every app of the workspace, and what a resource was made from.

use crate::fixtures::Workspace;
use serde_json::json;
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{Actor, InvokeOutcome, Value, ViewInstance};
use tilolang_tilo::host::Package;
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::publish::PlanRequest;

use crate::fixtures::Replica;

const LINKS: &str = include_str!("../../tilolang/tests/fixtures/links.tl");
const APP: &str = "so.tilo.links";

fn call(replica: &Replica, action: &str, args: Vec<Value>) -> Value {
    let package = replica.host.package(APP).unwrap();
    let id = package
        .meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap()
        .id
        .clone();
    match replica
        .host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person(&replica.name)),
            (id.as_str(), args),
        )
        .unwrap()
    {
        InvokeOutcome::Done(v) => v,
        other => panic!("{action}: {other:?}"),
    }
}

fn text(v: Value) -> String {
    match v {
        Value::Text(t) => t,
        other => panic!("not text: {other:?}"),
    }
}

fn lines(v: Value) -> Vec<String> {
    match v {
        Value::List(items) => items.into_iter().map(text).collect(),
        other => panic!("not a list: {other:?}"),
    }
}

fn notes(replica: &Replica) -> Vec<ResourceRef> {
    match replica
        .host
        .runtime()
        .read(&root(replica), "notes")
        .unwrap()
    {
        Value::List(items) => items
            .into_iter()
            .map(|item| match item {
                Value::Ref(r) => std::rc::Rc::unwrap_or_clone(r.target),
                other => panic!("not a resource: {other:?}"),
            })
            .collect(),
        other => panic!("not a list: {other:?}"),
    }
}

fn root(replica: &Replica) -> ResourceRef {
    replica.host.root_of(APP).unwrap()
}

/// Invoke `action` on the board as the API does, made from `from` when one is given.
fn invoke(replica: &Replica, action: &str, args: &serde_json::Value, from: Option<&ResourceRef>) {
    let meta = replica.host.package(APP).unwrap().meta;
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("the action {action}"))
        .id
        .clone();
    let kind = meta.kind_of("Board");
    let root = root(replica).id_string();
    let request = PlanRequest {
        app: APP,
        kind: &kind,
        action_id: &id,
        resource: Some(&root),
        args: args.as_object().unwrap(),
        caller: None,
        from,
    };
    replica.carry_out(&request).unwrap();
    replica.commit();
}

fn board() -> (Replica, Vec<Package>) {
    let (ada, packages) = Replica::new("ada", Workspace::new(), &[(LINKS, "")]);
    // The root is made on its first reading, as a window opens its app.
    let _ = root(&ada);
    ada.commit();
    (ada, packages)
}

/// Adds a note as the API does, from `from` when one is given, and answers its reference.
fn add_note(replica: &Replica, title: &str, from: Option<&ResourceRef>) -> ResourceRef {
    let meta = replica.host.package(APP).unwrap().meta;
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == "add_note")
        .unwrap()
        .id
        .clone();
    let kind = meta.kind_of("Board");
    let root = root(replica).id_string();
    let args = json!({"title": title});
    let request = PlanRequest {
        app: APP,
        kind: &kind,
        action_id: &id,
        resource: Some(&root),
        args: args.as_object().unwrap(),
        caller: None,
        from,
    };
    replica.carry_out(&request).unwrap();
    replica.commit();
    notes(replica).pop().unwrap()
}

tilo_test_env::scenario! {
    fn a_note_says_what_it_was_made_from_and_what_links_to_it;

    phase a_note_made_from_another_says_so;
    {
        let (ada, loaded) = board();
        let source = add_note(&ada, "A", None);
        let made = add_note(&ada, "B", Some(&source));
        assert_eq!(
            text(call(&ada, "made_from", vec![Value::resource(&made)])),
            "A"
        );
        assert_eq!(
            text(call(&ada, "made_from", vec![Value::resource(&source)])),
            ""
        );
        // It is a fact of the change, so another replica reads it from its store.
        let ben = ada.fork("ben", &[(LINKS, &loaded[0].ids)]);
        assert_eq!(
            text(call(&ben, "made_from", vec![Value::resource(&made)])),
            "A"
        );
    }

    phase what_links_to_a_note_is_read_from_the_store;
    {
        let (ada, _) = board();
        let source = add_note(&ada, "A", None);
        let made = add_note(&ada, "B", Some(&source));
        // An item whose field names the note, and a note whose text mentions it.
        invoke(
            &ada,
            "item_about",
            &json!({"n": source.id_string(), "title": "I"}),
            None,
        );
        invoke(
            &ada,
            "note_about",
            &json!({"n": source.id_string(), "title": "M"}),
            None,
        );
        assert_eq!(
            lines(call(&ada, "links_of", vec![Value::resource(&source)])),
            [
                "created_from B".to_owned(),
                "field I".to_owned(),
                "mention M".to_owned(),
            ]
        );
        // What links to it links to nothing itself.
        assert!(lines(call(&ada, "links_of", vec![Value::resource(&made)])).is_empty());
        // A view of what links to it shows the same, and runs again when one more is made from it.
        let view = ada
            .host
            .runtime()
            .open_view(
                &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
                &ada.host.package(APP).unwrap().view_id("links").unwrap(),
                vec![Value::resource(&source)],
            )
            .unwrap();
        let shown = |view: &ViewInstance| format!("{:?}", view.tree());
        assert!(shown(&view).contains("created_from B"), "{}", shown(&view));
        let mut view = view;
        add_note(&ada, "C", Some(&source));
        assert!(
            !view.poll_patches().unwrap().is_empty(),
            "the view runs again when what links to the note changes"
        );
        assert!(shown(&view).contains("created_from C"), "{}", shown(&view));
    }
}
