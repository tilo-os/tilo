//! The production native action door, not a parallel snapshot helper.
use super::*;
use tilo_app::Role;
use ui_core::automerge::transaction::Transactable;

fn review(app: &mut TestApp<BoardApp>, version: &str, paths: &[String]) -> Value {
    app.call(
        "review-restore",
        json!({"version": version, "paths": paths}),
    )
    .unwrap()
    .unwrap()
    .get("review")
    .expect("the review is returned")
    .clone()
}

fn read_at(app: &mut TestApp<BoardApp>, version: &str) -> Value {
    app.call("read", json!({"version": version}))
        .unwrap()
        .unwrap()
}

#[test]
fn historical_read_is_typed_frozen_and_matches_the_history_preview() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("rename", json!({"title": "Early"})).unwrap();
    app.call("add", json!({"text": "sky"})).unwrap();
    app.call("checkpoint", json!({"name": "early"})).unwrap();
    let id = app.state().cards.ids().next().unwrap().clone();
    // Deliberately omitted from the app's typed read adapter.
    app.merge_peer_edit(|doc| {
        doc.put(
            ui_core::automerge::ROOT,
            "private-raw-key",
            "do-not-disclose",
        )
        .unwrap();
        doc.commit();
    });
    app.call("checkpoint", json!({"name": "with-private-data"}))
        .unwrap();
    app.call("rename", json!({"title": "Late"})).unwrap();
    app.call("reword", json!({"card": id.as_str(), "text": "storm"}))
        .unwrap();
    let before = app.peer_document().base_heads();

    let old = read_at(&mut app, "early");
    assert_eq!(old["content"]["title"], "Early");
    assert_eq!(old["content"]["cards"][0]["text"], "sky");
    let item = app
        .call("read", json!({"version": "early", "item": id.as_str()}))
        .unwrap()
        .unwrap();
    assert_eq!(item["content"]["text"], "sky");
    let private = read_at(&mut app, "with-private-data");
    let private_item = app
        .call(
            "read",
            json!({"version": "with-private-data", "item": id.as_str()}),
        )
        .unwrap()
        .unwrap();
    for response in [private, private_item] {
        assert!(!response.to_string().contains("private-raw-key"));
        assert!(!response.to_string().contains("do-not-disclose"));
    }
    let heads = old["heads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|head| head.as_str().unwrap().parse().unwrap())
        .collect::<Vec<_>>();
    let preview = app.past_version(&heads).unwrap();
    assert_eq!(preview.state().title, old["content"]["title"]);
    assert!(preview.texts().iter().any(|text| text.contains("sky")));
    assert_eq!(read_at(&mut app, old["version"].as_str().unwrap()), old);
    assert_eq!(app.state().title, "Late");
    assert_eq!(cards(&app), ["storm"]);
    assert_eq!(app.peer_document().base_heads(), before);
    assert!(app.call("read", json!({"version": "unknown"})).is_err());
}

#[test]
fn reviewed_item_restore_preserves_peer_work_is_attributed_and_undoable() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("add", json!({"text": "sky"})).unwrap();
    app.call("add", json!({"text": "sea"})).unwrap();
    app.call("checkpoint", json!({"name": "good"})).unwrap();
    let ids: Vec<Id> = app.state().cards.ids().cloned().collect();
    app.call("reword", json!({"card": ids[0].as_str(), "text": "storm"}))
        .unwrap();
    // A collaborator's interleaved work must survive selective restore.
    app.call_in_run("peer:run", "rename", json!({"title": "Peer's title"}))
        .unwrap();
    app.call_in_run(
        "peer:run",
        "reword",
        json!({"card": ids[1].as_str(), "text": "Peer's sea"}),
    )
    .unwrap();
    let path = format!("cards/{}", ids[0].as_str());
    let compared = app.call("diff", json!({"from": "good"})).unwrap().unwrap();
    assert!(
        compared["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["item"] == path)
    );
    let before = app.peer_document().base_heads();
    let reviewed = review(&mut app, "good", std::slice::from_ref(&path));
    assert_eq!(
        app.peer_document().base_heads(),
        before,
        "review never writes"
    );
    assert_eq!(reviewed["live_heads"], compared["to_heads"]);
    assert_eq!(reviewed["target_heads"], compared["from_heads"]);

    let agent =
        tilo_actions::InteractionActor::new("agent:historian", tilo_actions::ActorKind::Agent)
            .acting_for("person:quinten");
    let restored = app
        .call_by(
            tilo_actions::ActionInvoker::admitted(&agent),
            "test.board",
            "restore-reviewed",
            json!({"review": reviewed}),
        )
        .unwrap()
        .value
        .unwrap();
    assert_eq!(restored["restored"], json!([path]));
    assert_eq!(cards(&app), ["sky", "Peer's sea"]);
    assert_eq!(app.state().title, "Peer's title");
    let entry = newest(&mut app);
    assert_eq!(entry["what"]["op"], "restore");
    assert_eq!(entry["actor"]["id"], "agent:historian");
    assert_eq!(entry["actor"]["for"], "person:quinten");
    app.call("undo", json!({"by": "agent:historian"})).unwrap();
    assert_eq!(cards(&app), ["storm", "Peer's sea"]);
    assert_eq!(app.state().title, "Peer's title");
}

#[test]
fn stale_review_refuses_unrelated_edits_and_same_value_aba_without_history_writes() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("rename", json!({"title": "old"})).unwrap();
    app.call("checkpoint", json!({"name": "old"})).unwrap();
    app.call("rename", json!({"title": "new"})).unwrap();
    let first = review(&mut app, "old", &["title".into()]);
    app.call_in_run("peer:run", "add", json!({"text": "unrelated"}))
        .unwrap();
    let before = app.peer_document().base_heads();
    let message = refusal(app.call("restore-reviewed", json!({"review": first})));
    assert!(message.contains("Stale restore review"), "{message}");
    assert_eq!(app.peer_document().base_heads(), before);
    assert_eq!(app.state().title, "new");
    assert_eq!(cards(&app), ["unrelated"]);

    let second = review(&mut app, "old", &["title".into()]);
    app.call("rename", json!({"title": "temporary"})).unwrap();
    app.call("rename", json!({"title": "new"})).unwrap();
    let before = app.peer_document().base_heads();
    assert!(
        app.call("restore-reviewed", json!({"review": second}))
            .is_err()
    );
    assert_eq!(app.peer_document().base_heads(), before);
    assert_eq!(app.state().title, "new");
}

#[test]
fn review_pins_checkpoint_target_and_cannot_be_reused_on_another_document() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("rename", json!({"title": "old"})).unwrap();
    app.call("checkpoint", json!({"name": "point"})).unwrap();
    app.call("rename", json!({"title": "new"})).unwrap();
    let reviewed = review(&mut app, "point", &["title".into()]);
    // Changing the checkpoint is itself new work, invalidating this review.
    app.call("checkpoint", json!({"name": "point"})).unwrap();
    let before = app.peer_document().base_heads();
    assert!(
        app.call("restore-reviewed", json!({"review": reviewed}))
            .is_err()
    );
    assert_eq!(app.peer_document().base_heads(), before);

    let mut other = TestApp::<BoardApp>::new().unwrap();
    other.call("rename", json!({"title": "other"})).unwrap();
    let before = other.peer_document().base_heads();
    assert!(
        other
            .call("restore-reviewed", json!({"review": reviewed}))
            .is_err()
    );
    assert_eq!(other.peer_document().base_heads(), before);
    assert_eq!(other.state().title, "other");
}

#[test]
fn invalid_or_empty_selection_never_means_restore_everything() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("rename", json!({"title": "old"})).unwrap();
    app.call("checkpoint", json!({"name": "old"})).unwrap();
    app.call("rename", json!({"title": "new"})).unwrap();
    let valid = review(&mut app, "old", &["title".into()]);
    let before = app.peer_document().base_heads();
    for paths in [
        json!([]),
        json!(["missing"]),
        json!(["title", "missing"]),
        json!(["cards//id"]),
    ] {
        assert!(
            app.call("review-restore", json!({"version": "old", "paths": paths}))
                .is_err()
        );
        let mut invalid = valid.clone();
        invalid["paths"] = paths;
        assert!(
            app.call("restore-reviewed", json!({"review": invalid}))
                .is_err()
        );
        assert_eq!(app.peer_document().base_heads(), before);
        assert_eq!(app.state().title, "new");
    }
    for (field, value) in [
        ("schema", json!(2)),
        ("live_heads", json!([])),
        ("target_heads", json!(["0".repeat(64)])),
        ("target_heads", json!(["abcdef"])),
    ] {
        let mut invalid = valid.clone();
        invalid[field] = value;
        assert!(
            app.call("restore-reviewed", json!({"review": invalid}))
                .is_err()
        );
        assert_eq!(app.peer_document().base_heads(), before);
    }
}

#[test]
fn viewer_can_read_and_review_but_cannot_restore() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("rename", json!({"title": "old"})).unwrap();
    app.call("checkpoint", json!({"name": "old"})).unwrap();
    app.call("rename", json!({"title": "new"})).unwrap();
    app.set_role(Role::Viewer);
    assert_eq!(read_at(&mut app, "old")["content"]["title"], "old");
    let reviewed = review(&mut app, "old", &["title".into()]);
    let before = app.peer_document().base_heads();
    assert!(
        app.call("restore-reviewed", json!({"review": reviewed}))
            .is_err()
    );
    assert_eq!(app.peer_document().base_heads(), before);
    assert_eq!(app.state().title, "new");
}

#[test]
fn historical_read_preserves_all_concurrent_heads() {
    let mut laptop = TestApp::<BoardApp>::on_device("laptop").unwrap();
    let mut phone = TestApp::<BoardApp>::on_device("phone").unwrap();
    sync(&mut laptop, &mut phone);
    laptop.call("rename", json!({"title": "Laptop"})).unwrap();
    phone.call("add", json!({"text": "Phone"})).unwrap();
    sync(&mut laptop, &mut phone);
    let heads = laptop.peer_document().base_heads();
    assert!(heads.len() > 1);
    let exact = heads
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("+");
    let read = read_at(&mut laptop, &exact);
    assert_eq!(read["heads"].as_array().unwrap().len(), heads.len());
    assert_eq!(read["content"]["title"], "Laptop");
    assert_eq!(read["content"]["cards"][0]["text"], "Phone");
    assert_eq!(
        read_at(&mut laptop, read["version"].as_str().unwrap()),
        read
    );
}
