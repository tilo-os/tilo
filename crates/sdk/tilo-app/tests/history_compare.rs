//! Production public action-door proof, coupled to the broker's routing-only fixture.
use super::*;
use tilo_app::Role;
use ui_core::automerge::transaction::Transactable;

#[path = "history_compare_readability.rs"]
mod readability;

fn compare(app: &mut TestApp<BoardApp>, args: Value) -> Value {
    app.call("compare", args).unwrap().unwrap()
}

#[test]
fn compare_is_typed_frozen_bounded_and_keeps_structural_diff() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("rename", json!({"title": "early"})).unwrap();
    app.call("checkpoint", json!({"name": "early"})).unwrap();
    app.merge_peer_edit(|doc| {
        doc.put(
            ui_core::automerge::ROOT,
            "private-raw-key",
            "do-not-disclose",
        )
        .unwrap();
        doc.commit();
    });
    app.call("rename", json!({"title": "late"})).unwrap();
    let live = app.peer_document().base_heads();
    app.set_role(Role::Viewer);
    let response = compare(&mut app, json!({"from": "early"}));
    assert_eq!(
        response["comparisons"][0]["before"]["content"]["title"],
        "early"
    );
    assert_eq!(
        response["comparisons"][0]["after"]["content"]["title"],
        "late"
    );
    assert!(!response.to_string().contains("private-raw-key"));
    assert!(!response.to_string().contains("do-not-disclose"));
    assert_eq!(app.peer_document().base_heads(), live);
    let diff = app.call("diff", json!({"from": "early"})).unwrap().unwrap();
    assert!(
        diff["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["item"] == "title")
    );
    assert_eq!(diff["from_heads"], response["from_heads"]);
    assert_eq!(diff["to_heads"], response["to_heads"]);
    assert!(response.to_string().len() < 256 * 1024);
}

#[test]
fn compare_added_removed_and_changed_public_items() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("rename", json!({"title": "board"})).unwrap();
    app.call("checkpoint", json!({"name": "empty"})).unwrap();
    app.call("add", json!({"text": "sky"})).unwrap();
    let id = app.state().cards.ids().next().unwrap().clone();
    let item = format!("test.board/cards/{}", id.as_str());
    app.call("checkpoint", json!({"name": "added"})).unwrap();
    let added = compare(&mut app, json!({"from": "empty", "items": [item]}));
    assert_eq!(
        added["comparisons"][0]["before"],
        json!({"status": "absent"})
    );
    assert_eq!(added["comparisons"][0]["after"]["content"]["text"], "sky");
    app.call("reword", json!({"card": id.as_str(), "text": "storm"}))
        .unwrap();
    let changed = compare(&mut app, json!({"from": "added", "items": [item]}));
    assert_eq!(
        changed["comparisons"][0]["before"]["content"]["text"],
        "sky"
    );
    assert_eq!(
        changed["comparisons"][0]["after"]["content"]["text"],
        "storm"
    );
    app.call("remove", json!({"card": id.as_str()})).unwrap();
    let removed = compare(&mut app, json!({"from": "added", "items": [item]}));
    assert_eq!(
        removed["comparisons"][0]["after"],
        json!({"status": "absent"})
    );
    assert_eq!(
        removed["comparisons"][0]["before"]["content"]["text"],
        "sky"
    );
}

#[test]
fn compare_resolves_multihead_versions_once() {
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
    laptop.call("rename", json!({"title": "later"})).unwrap();
    let answer = compare(&mut laptop, json!({"from": exact, "to": exact}));
    assert_eq!(answer["from_heads"].as_array().unwrap().len(), heads.len());
    assert_eq!(answer["from_heads"], answer["to_heads"]);
    assert_eq!(
        answer["comparisons"][0]["before"],
        answer["comparisons"][0]["after"]
    );
    assert_eq!(
        answer["comparisons"][0]["before"]["content"]["title"],
        "Laptop"
    );
    assert_eq!(
        answer["comparisons"][0]["before"]["content"]["cards"][0]["text"],
        "Phone"
    );
}

#[test]
fn compare_rejects_oversized_and_invalid_requests() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("rename", json!({"title": "small"})).unwrap();
    app.call("checkpoint", json!({"name": "small"})).unwrap();
    app.call("rename", json!({"title": "x".repeat(256 * 1024)}))
        .unwrap();
    assert!(refusal(app.call("compare", json!({"from": "small"}))).contains("256"));
    // Each side fits alone; their combined response must still fail.
    app.call("rename", json!({"title": "x".repeat(128 * 1024)}))
        .unwrap();
    app.call("checkpoint", json!({"name": "half"})).unwrap();
    app.call("rename", json!({"title": "y".repeat(128 * 1024)}))
        .unwrap();
    assert!(refusal(app.call("compare", json!({"from": "half"}))).contains("256"));
    for items in [
        json!([]),
        json!(["same", "same"]),
        json!(["x".repeat(513)]),
        json!((0..33).map(|i| i.to_string()).collect::<Vec<_>>()),
    ] {
        assert!(
            app.call("compare", json!({"from": "small", "items": items}))
                .is_err()
        );
    }
}

#[test]
fn compare_schema_failure_is_not_absent() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.call("rename", json!({"title": "healthy"})).unwrap();
    let mut broken = app.peer_document();
    broken
        .put(ui_core::automerge::ROOT, "title", 42_i64)
        .unwrap();
    broken.commit();
    let bad_heads = broken
        .base_heads()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("+");
    // Repair before merging: the live resource remains readable while the
    // historical frontier still requires a failing native model decode.
    broken
        .put(ui_core::automerge::ROOT, "title", "repaired")
        .unwrap();
    broken.commit();
    app.merge_peer(&mut broken).unwrap();
    let result = app.call(
        "compare",
        json!({"from": bad_heads, "items": ["test.board/cards/missing"]}),
    );
    assert!(
        result.is_err(),
        "schema failure must not masquerade as an absent item: {result:?}"
    );
}
