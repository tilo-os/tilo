//! Two replicas of one room: concurrent edits converge, history names the invoker, and a peer
//! that never loaded the wasm module replays what the origin recorded.

use std::fmt::Write as _;

use automerge::AutoCommit;
use serde_json::json;
use tilo_actions::{ActionRegistry, Did};
use tilolang_tilo::action::register_actions;
use tilolang_tilo::publish::PlanRequest;
use tilolang_tilo::record::DynValue;

use crate::fixtures::{APP, COUNTER, Replica, TODO, button_key, count, field_id};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Ada's replica with the counter loaded and its root created, and Grace's fork of it.
fn counter_room() -> (Replica, Replica, tilolang_runtime::Meta) {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(COUNTER, "")]);
    let counter = packages[0].clone();
    ada.host.root_of(APP).unwrap();
    ada.commit();
    let grace = ada.fork("grace", &[(COUNTER, &counter.ids)]);
    (ada, grace, counter.meta)
}

#[test]
fn two_replicas_pressing_the_button_concurrently_read_two() {
    let (mut ada, mut grace, meta) = counter_room();
    let (view_ada, view_grace) = (ada.view(APP), grace.view(APP));
    view_ada.press(&button_key(&view_ada)).unwrap();
    view_grace.press(&button_key(&view_grace)).unwrap();
    ada.commit();
    grace.commit();
    assert_eq!(count(&ada.root(APP), &meta), 1, "only ada's press so far");
    ada.sync_with(&mut grace);
    assert_eq!(count(&ada.root(APP), &meta), 2);
    assert_eq!(count(&grace.root(APP), &meta), 2);
}

#[test]
fn a_view_open_over_a_replica_follows_a_collaborators_press() {
    let (mut ada, mut grace, _) = counter_room();
    let mut view_ada = ada.view(APP);
    let before = view_ada.tree().clone();
    let view_grace = grace.view(APP);
    view_grace.press(&button_key(&view_grace)).unwrap();
    grace.commit();
    ada.sync_with(&mut grace);
    let patches = view_ada.poll_patches().unwrap();
    assert!(!patches.is_empty(), "the text of the count changed");
    assert_ne!(&before, view_ada.tree());
}

#[test]
fn history_names_the_invoker_and_the_verb() {
    let (mut ada, mut grace, meta) = counter_room();
    let (view_ada, view_grace) = (ada.view(APP), grace.view(APP));
    view_ada.press(&button_key(&view_ada)).unwrap();
    view_grace.press(&button_key(&view_grace)).unwrap();
    ada.commit();
    grace.commit();
    ada.sync_with(&mut grace);
    let increment = &meta
        .actions
        .iter()
        .find(|a| a.name == "increment")
        .unwrap()
        .id;
    let versions = tilo_resource::history::versions(&mut ada.doc);
    let by = |author: &str| {
        versions.iter().any(|version| {
            version.actor == hex(author.as_bytes())
                && version
                    .context
                    .as_ref()
                    .and_then(|context| context.what.as_ref())
                    .is_some_and(
                        |did| matches!(did, Did::Verb(verb) if &verb.action.action == increment),
                    )
        })
    };
    assert!(by("ada"), "{versions:#?}");
    assert!(by("grace"), "{versions:#?}");
}

#[test]
fn a_peer_without_the_wasm_module_replays_the_recorded_actions() {
    let (mut ada, _, meta) = counter_room();
    let base = ada.doc.fork();
    let view = ada.view(APP);
    for _ in 0..2 {
        view.press(&button_key(&view)).unwrap();
        ada.commit();
    }
    // The peer holds a copy of the room and the decoder, and no runtime and no module.
    let mut peer = base;
    let registry = ActionRegistry::with(register_actions);
    let pressed: Vec<_> = ada.log.iter().skip(1).collect();
    assert_eq!(pressed.len(), 2);
    for envelope in pressed {
        tilo_actions::replay(&mut peer, envelope, &registry).unwrap();
    }
    let root = tilolang_tilo::action::resource_ids(&peer, APP);
    let record = tilolang_tilo::action::read_resource(&peer, APP, &root[0]).unwrap();
    assert_eq!(count(&record, &meta), 2);
    assert_eq!(peer.get_heads(), ada.doc.get_heads(), "the same changes");
}

fn todo_room() -> (Replica, Replica, tilolang_runtime::Meta, String) {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(TODO, "")]);
    let todo = packages[0].clone();
    let root = ada.host.root_of("so.tilo.todo").unwrap();
    ada.commit();
    let grace = ada.fork("grace", &[(TODO, &todo.ids)]);
    (ada, grace, todo.meta, root.doc)
}

fn plan(
    replica: &mut Replica,
    meta: &tilolang_runtime::Meta,
    root: &str,
    name: &str,
    args: serde_json::Value,
) {
    let action = meta.actions.iter().find(|a| a.name == name).unwrap();
    let args = args.as_object().cloned().unwrap_or_default();
    let planned = replica
        .host
        .plan(&PlanRequest {
            app: "so.tilo.todo",
            kind: "tl.so.tilo.todo.TodoList",
            action_id: &action.id,
            resource: Some(root),
            args: &args,
        })
        .unwrap();
    replica.apply(&planned);
}

fn item_ids(replica: &Replica, meta: &tilolang_runtime::Meta, root: &str) -> Vec<String> {
    let record = replica.host.store().record(root).unwrap();
    match record.get(&field_id(meta, "TodoList", "items")) {
        Some(DynValue::Collection(items)) => {
            items.iter().map(|(id, _)| id.as_str().to_owned()).collect()
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn concurrent_inserts_and_moves_of_a_collection_converge() {
    let (mut ada, mut grace, meta, root) = todo_room();
    plan(&mut ada, &meta, &root, "add", json!({"text": "milk"}));
    ada.sync_with(&mut grace);
    plan(&mut ada, &meta, &root, "add", json!({"text": "eggs"}));
    plan(&mut grace, &meta, &root, "add", json!({"text": "bread"}));
    ada.sync_with(&mut grace);
    let ids = item_ids(&ada, &meta, &root);
    assert_eq!(ids.len(), 3);
    assert_eq!(ids, item_ids(&grace, &meta, &root));

    // Ada moves the last item first while Grace adds another; both survive the merge.
    let items_field = field_id(&meta, "TodoList", "items");
    let last = format!("{root}/{items_field}:{}", ids[2]);
    plan(&mut ada, &meta, &root, "move_first", json!({"i": last}));
    plan(&mut grace, &meta, &root, "add", json!({"text": "rice"}));
    ada.sync_with(&mut grace);
    let merged = item_ids(&ada, &meta, &root);
    assert_eq!(merged.len(), 4);
    assert_eq!(merged[0], ids[2], "the moved item is first");
    assert_eq!(merged, item_ids(&grace, &meta, &root));
    assert_eq!(
        ada.host.store().record(&root).unwrap(),
        grace.host.store().record(&root).unwrap()
    );
}

#[test]
fn a_set_and_text_edited_concurrently_merge() {
    let (mut ada, mut grace, meta, root) = todo_room();
    plan(&mut ada, &meta, &root, "tag", json!({"t": "home"}));
    plan(&mut grace, &meta, &root, "tag", json!({"t": "work"}));
    ada.sync_with(&mut grace);
    let tags = field_id(&meta, "TodoList", "tags");
    for replica in [&ada, &grace] {
        let record = replica.host.store().record(&root).unwrap();
        let Some(DynValue::Set(set)) = record.get(&tags) else {
            panic!("no tags");
        };
        assert_eq!(set.len(), 2);
    }
}
