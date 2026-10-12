//! Native history of the production project record, not a stand-in resource.

#![allow(
    clippy::unwrap_used,
    reason = "test setup and action results must fail loudly"
)]
#![allow(
    clippy::indexing_slicing,
    reason = "tests assert the typed JSON response contract"
)]

use code_editor::{CodeEditorShell, actions};
use serde_json::{Value, json};
use ui::automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
use ui::framework::testing::TestApp;
use ui::tilo_actions::{ActionInvoker, ActorKind};
use ui::tilo_resource::Resource as _;

type App = TestApp<CodeEditorShell>;

fn answer(app: &mut App, action: &str, arguments: Value) -> Value {
    app.call(action, arguments).unwrap().unwrap()
}

fn checkpoint(app: &mut App, name: &str) {
    app.call("checkpoint", json!({"name": name})).unwrap();
}

fn named(app: &App, name: &str) -> String {
    app.state()
        .files
        .iter()
        .find(|(_, entry)| entry.name == name)
        .unwrap()
        .0
        .to_string()
}

fn file(app: &mut App, name: &str, text: &str, folder: Option<&str>) -> String {
    let mut arguments = json!({"name": name, "text": text});
    if let Some(folder) = folder {
        arguments["folder"] = json!(folder);
    }
    app.call("new-file", arguments).unwrap();
    named(app, name)
}

fn read(app: &mut App, version: Option<&str>, item: Option<&str>) -> Value {
    let mut arguments = json!({});
    if let Some(version) = version {
        arguments["version"] = json!(version);
    }
    if let Some(item) = item {
        arguments["item"] = json!(item);
    }
    let response = answer(app, "read", arguments);
    if version.is_some() {
        response["content"].clone()
    } else {
        response
    }
}

fn replace(app: &mut App, id: &str, text: &str) {
    app.call_on(
        &actions::file_resource_id(id),
        "replace-text",
        json!({"text": text}),
    )
    .unwrap();
}

#[test]
fn whole_record_checkpoint_inventory_and_item_reads_are_frozen_across_rename_and_delete() {
    let mut app = App::new().unwrap();
    app.call("new-folder", json!({"name": "src"})).unwrap();
    let folder = named(&app, "src");
    let main = file(&mut app, "main.rs", "fn main() {}\n", Some(&folder));
    let notes = file(&mut app, "notes.txt", "notes\n", None);
    checkpoint(&mut app, "project");
    let inventory = read(&mut app, Some("project"), None);
    assert_eq!(inventory["scope"], "recorded-project");
    assert_eq!(inventory["complete"], true);
    let entries = inventory["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    for (id, path, kind, available) in [
        (&folder, "src", "folder", false),
        (&main, "src/main.rs", "file", true),
        (&notes, "notes.txt", "file", true),
    ] {
        let entry = entries.iter().find(|entry| entry["id"] == *id).unwrap();
        assert_eq!(entry["path"], path);
        assert_eq!(entry["kind"], kind);
        assert_eq!(entry["resource_id"], actions::file_resource_id(id));
        assert_eq!(entry["text_available"], available);
        assert!(entry["language"].is_string());
        assert!(
            entry.get("text").is_none(),
            "inventory must not copy buffers"
        );
    }
    let address = actions::file_resource_id(&main);
    let frozen = read(&mut app, Some("project"), Some(&main));
    assert_eq!(frozen["text"], "fn main() {}\n");
    assert_eq!(frozen, read(&mut app, Some("project"), Some(&address)));
    let folder_read = read(&mut app, Some("project"), Some(&folder));
    assert_eq!(folder_read["kind"], "folder");
    assert!(folder_read.get("text").is_none_or(Value::is_null));
    app.call_on(&address, "rename", json!({"name": "app.rs"}))
        .unwrap();
    replace(&mut app, &main, "fn changed() {}\n");
    checkpoint(&mut app, "renamed");
    let renamed = read(&mut app, Some("renamed"), Some(&main));
    assert_eq!(renamed["path"], "src/app.rs");
    assert_eq!(renamed["text"], "fn changed() {}\n");
    app.call_confirmed_on(&address, "delete", json!({}))
        .unwrap();
    checkpoint(&mut app, "deleted");
    assert!(
        app.call("read", json!({"version": "deleted", "item": main}))
            .is_err()
    );
    assert_eq!(read(&mut app, Some("project"), Some(&address)), frozen);
    assert_eq!(read(&mut app, Some("renamed"), Some(&address)), renamed);
    assert_eq!(read(&mut app, Some("project"), None), inventory);
    assert_eq!(read(&mut app, None, Some(&notes))["text"], "notes\n");
}

#[test]
fn compared_frozen_source_frontier_drives_selective_review_after_rename_and_edit() {
    let mut app = App::new().unwrap();
    app.call("new-folder", json!({"name": "src"})).unwrap();
    let folder = named(&app, "src");
    let main = file(&mut app, "main.rs", "fn original() {}\n", Some(&folder));
    let other = file(&mut app, "other.rs", "other original\n", None);
    checkpoint(&mut app, "baseline");
    let address = actions::file_resource_id(&main);
    app.call_on(&address, "rename", json!({"name": "renamed.rs"}))
        .unwrap();
    replace(&mut app, &main, "fn edited() {}\n");
    replace(&mut app, &other, "unrelated work\n");
    checkpoint(&mut app, "edited");
    let compared = answer(
        &mut app,
        "compare",
        json!({"from": "baseline", "to": "edited", "items": [address]}),
    );
    let pair = &compared["comparisons"][0];
    assert_eq!(pair["item"], address);
    assert_eq!(pair["before"]["status"], "present");
    assert_eq!(pair["before"]["content"]["id"], main);
    assert_eq!(pair["before"]["content"]["path"], "src/main.rs");
    assert_eq!(pair["before"]["content"]["text"], "fn original() {}\n");
    assert_eq!(pair["after"]["status"], "present");
    assert_eq!(pair["after"]["content"]["id"], main);
    assert_eq!(pair["after"]["content"]["path"], "src/renamed.rs");
    assert_eq!(pair["after"]["content"]["text"], "fn edited() {}\n");
    assert_eq!(
        app.state().read_texts(),
        (0, 0),
        "frozen reads must demand their own text, not load the live model"
    );

    // Rebind the mutable checkpoint and change live content. Consuming the
    // comparison identity must still inspect and recover the original text.
    checkpoint(&mut app, "baseline");
    replace(&mut app, &main, "fn later() {}\n");
    replace(&mut app, &other, "new unrelated work\n");
    let pinned = answer(
        &mut app,
        "compare",
        json!({"from": compared["from"], "to": compared["to"], "items": [address]}),
    );
    assert_eq!(pinned, compared);
    let diff = answer(&mut app, "diff", json!({"from": compared["from"]}));
    assert_eq!(diff["from_heads"], compared["from_heads"]);
    let path = format!("buffers/{main}");
    assert!(
        diff["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["item"] == path)
    );
    let reviewed = answer(
        &mut app,
        "review-restore",
        json!({"version": compared["from"], "paths": [path]}),
    );
    assert_eq!(reviewed["review"]["target_heads"], compared["from_heads"]);
    assert_eq!(reviewed["review"]["live_heads"], diff["to_heads"]);
    let restored = answer(
        &mut app,
        "restore-reviewed",
        json!({"review": reviewed["review"]}),
    );
    assert_eq!(restored["restored"], json!([path]));
    let current = read(&mut app, None, Some(&address));
    assert_eq!(current["text"], "fn original() {}\n");
    assert_eq!(
        current["path"], "src/renamed.rs",
        "buffer-only recovery must not undo the rename"
    );
    assert_eq!(
        read(&mut app, None, Some(&other))["text"],
        "new unrelated work\n"
    );
}

#[test]
fn compared_deleted_source_is_absent_only_on_deleted_side_and_recovers_selected_identity() {
    let mut app = App::new().unwrap();
    let main = file(&mut app, "main.rs", "retained source\n", None);
    let other = file(&mut app, "other.rs", "original\n", None);
    checkpoint(&mut app, "before-delete");
    let address = actions::file_resource_id(&main);
    app.call_confirmed_on(&address, "delete", json!({}))
        .unwrap();
    replace(&mut app, &other, "keep unrelated text\n");
    let compared = answer(
        &mut app,
        "compare",
        json!({"from": "before-delete", "items": [address]}),
    );
    let pair = &compared["comparisons"][0];
    assert_eq!(pair["before"]["status"], "present");
    assert_eq!(pair["before"]["content"]["id"], main);
    assert_eq!(pair["before"]["content"]["path"], "main.rs");
    assert_eq!(pair["before"]["content"]["text"], "retained source\n");
    assert_eq!(pair["after"], json!({"status": "absent"}));
    let diff = answer(
        &mut app,
        "diff",
        json!({"from": compared["from"], "to": compared["to"]}),
    );
    assert_eq!(diff["from_heads"], compared["from_heads"]);
    assert_eq!(diff["to_heads"], compared["to_heads"]);
    let paths = [format!("files/{main}"), format!("buffers/{main}")];
    for path in &paths {
        assert!(
            diff["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["item"] == *path)
        );
    }
    let reviewed = answer(
        &mut app,
        "review-restore",
        json!({"version": compared["from"], "paths": paths}),
    );
    assert_eq!(reviewed["review"]["target_heads"], compared["from_heads"]);
    assert_eq!(reviewed["review"]["live_heads"], compared["to_heads"]);
    app.call("restore-reviewed", json!({"review": reviewed["review"]}))
        .unwrap();
    assert_eq!(
        read(&mut app, None, Some(&address)),
        pair["before"]["content"]
    );
    assert_eq!(
        read(&mut app, None, Some(&other))["text"],
        "keep unrelated text\n"
    );
    assert_eq!(
        answer(
            &mut app,
            "compare",
            json!({"from": compared["from"], "to": compared["to"], "items": [address]})
        ),
        compared
    );
}

#[test]
fn reviewed_buffer_restore_preserves_unrelated_current_text_and_rejects_stale_review() {
    let mut app = App::new().unwrap();
    let main = file(&mut app, "main.rs", "original\n", None);
    let other = file(&mut app, "other.rs", "other original\n", None);
    checkpoint(&mut app, "baseline");
    replace(&mut app, &main, "changed\n");
    replace(&mut app, &other, "unrelated edit\n");
    checkpoint(&mut app, "edited");
    let path = format!("buffers/{main}");
    let diff = answer(
        &mut app,
        "diff",
        json!({"from": "baseline", "to": "edited"}),
    );
    assert!(
        diff["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["item"] == path)
    );
    let review = answer(
        &mut app,
        "review-restore",
        json!({"version": "baseline", "paths": [path]}),
    );
    replace(&mut app, &other, "new unrelated edit\n");
    assert!(
        app.call("restore-reviewed", json!({"review": review["review"]}))
            .is_err()
    );
    assert_eq!(read(&mut app, None, Some(&main))["text"], "changed\n");
    let fresh = answer(
        &mut app,
        "review-restore",
        json!({"version": "baseline", "paths": [path]}),
    );
    let agent = ActionInvoker::actor("agent:restorer")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let restored = app
        .call_by(
            agent.clone(),
            code_editor::project::Project::KIND,
            "restore-reviewed",
            json!({"review": fresh["review"]}),
        )
        .unwrap()
        .value
        .unwrap();
    assert!(app.history().iter().any(|entry| {
        entry.invoker.as_ref().is_some_and(|who| {
            who.actor == agent.actor
                && who.kind == ActorKind::Agent
                && who.on_behalf_of.as_deref() == Some("ada")
        })
    }));
    assert_eq!(restored["restored"], json!([path]));
    assert_eq!(read(&mut app, None, Some(&main))["text"], "original\n");
    assert_eq!(
        read(&mut app, None, Some(&other))["text"],
        "new unrelated edit\n"
    );
    assert_eq!(
        read(&mut app, Some("edited"), Some(&other))["text"],
        "unrelated edit\n"
    );
    app.call_by(
        agent.clone(),
        code_editor::project::Project::KIND,
        "undo",
        json!({}),
    )
    .unwrap();
    assert_eq!(read(&mut app, None, Some(&main))["text"], "changed\n");
    assert_eq!(
        read(&mut app, None, Some(&other))["text"],
        "new unrelated edit\n"
    );
    app.call_by(
        agent,
        code_editor::project::Project::KIND,
        "redo",
        json!({}),
    )
    .unwrap();
    checkpoint(&mut app, "restored");
    let mut document = app.peer_document();
    let mut replica = App::launched_replica(&[], &mut document).unwrap();
    assert_eq!(
        read(&mut replica, Some("restored"), Some(&main))["text"],
        "original\n"
    );
    assert_eq!(
        read(&mut replica, None, Some(&other))["text"],
        "new unrelated edit\n"
    );
}

#[test]
fn attributed_agent_edit_can_be_undone_and_redone_without_touching_another_file() {
    let mut app = App::new().unwrap();
    let main = file(&mut app, "main.rs", "before\n", None);
    let other = file(&mut app, "other.rs", "untouched\n", None);
    let agent = ActionInvoker::actor("agent:source-review")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    app.call_by(
        agent.clone(),
        &actions::file_resource_id(&main),
        "replace-text",
        json!({"text": "agent edit\n"}),
    )
    .unwrap();
    let entries = app.history();
    let attributed = entries
        .iter()
        .find(|entry| {
            entry
                .invoker
                .as_ref()
                .is_some_and(|who| who.actor == agent.actor)
        })
        .unwrap();
    let who = attributed.invoker.as_ref().unwrap();
    assert_eq!(who.kind, ActorKind::Agent);
    assert_eq!(who.on_behalf_of.as_deref(), Some("ada"));
    app.call_by(
        agent.clone(),
        code_editor::project::Project::KIND,
        "undo",
        json!({}),
    )
    .unwrap();
    assert_eq!(read(&mut app, None, Some(&main))["text"], "before\n");
    app.call_by(
        agent,
        code_editor::project::Project::KIND,
        "redo",
        json!({}),
    )
    .unwrap();
    assert_eq!(read(&mut app, None, Some(&main))["text"], "agent edit\n");
    assert_eq!(read(&mut app, None, Some(&other))["text"], "untouched\n");
    checkpoint(&mut app, "agent-result");
    let mut document = app.peer_document();
    let mut replica = App::launched_replica(&[], &mut document).unwrap();
    assert_eq!(
        read(&mut replica, Some("agent-result"), Some(&main))["text"],
        "agent edit\n"
    );
    assert_eq!(
        read(&mut replica, None, Some(&other))["text"],
        "untouched\n"
    );
    assert!(
        replica.history().iter().any(|entry| entry
            .invoker
            .as_ref()
            .is_some_and(|who| who.actor == "agent:source-review"
                && who.kind == ActorKind::Agent
                && who.on_behalf_of.as_deref() == Some("ada")))
    );
}

#[test]
fn typed_reads_refuse_oversized_text_instead_of_truncating_it() {
    let mut app = App::new().unwrap();
    let text = "x".repeat(241 * 1024);
    let id = file(&mut app, "large.txt", &text, None);
    checkpoint(&mut app, "large");
    let inventory = read(&mut app, Some("large"), None);
    assert_eq!(inventory["entries"][0]["text_available"], true);
    let comparison = app.call(
        "compare",
        json!({"from": "large", "items": [actions::file_resource_id(&id)]}),
    );
    assert!(
        comparison.is_err(),
        "unreadable demanded text must fail, not report absence: {comparison:?}"
    );
    for item in [&id, &actions::file_resource_id(&id)] {
        assert!(
            app.call("read", json!({"version": "large", "item": item}))
                .is_err()
        );
    }
}

#[test]
fn peer_private_fields_and_terminal_queue_are_not_source_history_content() {
    let mut app = App::new().unwrap();
    let id = file(&mut app, "public.rs", "public source\n", None);
    checkpoint(&mut app, "before-peer");
    app.merge_peer_edit(|doc| {
        doc.put(ROOT, "private_key", "PRIVATE_KEY_SENTINEL")
            .unwrap();
        let queue = doc.get(ROOT, "terminal_requests").unwrap().unwrap().1;
        let entry = doc
            .put_object(&queue, "peer-request", ObjType::Map)
            .unwrap();
        doc.put(&entry, "k", "a").unwrap();
        let value = doc.put_object(&entry, "v", ObjType::Map).unwrap();
        let request = doc.put_object(&value, "value", ObjType::Map).unwrap();
        doc.put(&request, "pane", "absent-pane").unwrap();
        doc.put(&request, "command", "TERMINAL_QUEUE_SENTINEL")
            .unwrap();
        doc.put(&request, "requested_by", "peer").unwrap();
    });
    assert_eq!(
        app.read_document(|doc| doc
            .get(ROOT, "private_key")
            .unwrap()
            .unwrap()
            .0
            .as_str()
            .unwrap()
            .to_owned()),
        "PRIVATE_KEY_SENTINEL"
    );
    assert_eq!(
        app.read_document(|doc| {
            let queue = doc.get(ROOT, "terminal_requests").unwrap().unwrap().1;
            let entry = doc.get(&queue, "peer-request").unwrap().unwrap().1;
            let value = doc.get(&entry, "v").unwrap().unwrap().1;
            let request = doc.get(&value, "value").unwrap().unwrap().1;
            doc.get(&request, "command")
                .unwrap()
                .unwrap()
                .0
                .as_str()
                .unwrap()
                .to_owned()
        }),
        "TERMINAL_QUEUE_SENTINEL"
    );
    checkpoint(&mut app, "after-peer");
    for version in [None, Some("after-peer")] {
        for item in [None, Some(id.as_str())] {
            let content = read(&mut app, version, item).to_string();
            assert!(!content.contains("PRIVATE_KEY_SENTINEL"));
            assert!(!content.contains("TERMINAL_QUEUE_SENTINEL"));
            assert!(!content.contains("terminal_requests"));
        }
    }
    for item in [
        "private_key",
        "terminal_requests",
        "terminal_requests/peer-request",
        "buffers",
        "files",
    ] {
        assert!(
            app.call("read", json!({"version": "after-peer", "item": item}))
                .is_err()
        );
    }
    assert_eq!(
        read(&mut app, Some("after-peer"), Some(&id))["text"],
        "public source\n"
    );
}

#[test]
fn reviewed_deleted_source_recovery_keeps_identity_and_unrelated_work() {
    let mut app = App::new().unwrap();
    let main = file(&mut app, "main.rs", "recover me\n", None);
    let other = file(&mut app, "other.rs", "original\n", None);
    checkpoint(&mut app, "before-delete");
    app.call_confirmed_on(&actions::file_resource_id(&main), "delete", json!({}))
        .unwrap();
    replace(&mut app, &other, "keep peer edit\n");
    let paths = [format!("files/{main}"), format!("buffers/{main}")];
    let diff = answer(&mut app, "diff", json!({"from":"before-delete"}));
    for path in &paths {
        assert!(
            diff["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["item"] == *path)
        );
    }
    let reviewed = answer(
        &mut app,
        "review-restore",
        json!({"version":"before-delete", "paths": paths}),
    );
    app.call("restore-reviewed", json!({"review": reviewed["review"]}))
        .unwrap();
    let recovered = read(&mut app, None, Some(&actions::file_resource_id(&main)));
    assert_eq!(recovered["id"], main);
    assert_eq!(recovered["path"], "main.rs");
    assert_eq!(recovered["text"], "recover me\n");
    assert_eq!(
        read(&mut app, None, Some(&other))["text"],
        "keep peer edit\n"
    );
}

#[test]
fn inventory_pages_cover_every_record_without_loading_texts() {
    let mut app = App::new().unwrap();
    for index in 0..130 {
        file(&mut app, &format!("file-{index}.rs"), "text", None);
    }
    checkpoint(&mut app, "paged");
    let first = read(&mut app, Some("paged"), Some("inventory:0"));
    let second = read(&mut app, Some("paged"), Some("inventory:1"));
    assert_eq!(first["total_entries"], 130);
    assert_eq!(first["entries"].as_array().unwrap().len(), 128);
    assert_eq!(first["next"], "inventory:1");
    assert_eq!(second["entries"].as_array().unwrap().len(), 2);
    assert!(second["next"].is_null());
    let inventory = read(&mut app, Some("paged"), None);
    let mut combined = first["entries"].as_array().unwrap().clone();
    combined.extend(second["entries"].as_array().unwrap().iter().cloned());
    assert_eq!(json!(combined), inventory["entries"]);
    assert_eq!(app.state().read_texts(), (0, 0));
}
