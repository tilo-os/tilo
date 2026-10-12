//! A readable resource may publish nested children without overriding `child`.
use super::*;
use tilo_resource::{Demand, Doc, ModelError};
use ui_core::automerge::ReadDoc;

const LEFT: &str = "resource/left/body";
const RIGHT: &str = "resource/right/body";

/// Both branches publish the same leaf name, but readers require its full identity.
struct Namespaced<const LAZY: bool>;
impl<const LAZY: bool> Resource for Namespaced<LAZY> {
    const KIND: &'static str = "resource";
    const LABEL: &'static str = "Namespaced";
    const READABLE: bool = true;
    type State = (String, String, bool);
    type Action = readability::Noop;

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        doc.put(ui_core::automerge::ROOT, "left", "left before")?;
        doc.put(ui_core::automerge::ROOT, "right", "right before")?;
        Ok(())
    }

    fn read(doc: &Doc) -> Result<Self::State, ModelError> {
        Ok((
            tilo_resource::model::read_key(doc, &ui_core::automerge::ROOT, "left")?,
            tilo_resource::model::read_key(doc, &ui_core::automerge::ROOT, "right")?,
            !LAZY,
        ))
    }

    fn children(state: &Self::State) -> Vec<tilo_actions::Resource> {
        if !state.2 {
            return Vec::new();
        }
        [LEFT, RIGHT]
            .into_iter()
            .map(|id| {
                let parent = id.strip_suffix("/body").unwrap();
                let mut folder = tilo_actions::Resource::new(parent, "test.folder", "Folder");
                folder
                    .children
                    .push(tilo_actions::Resource::new(id, "test.item", "Body"));
                folder
            })
            .collect()
    }

    fn load(state: &mut Self::State, _: &Doc, demand: &Demand) -> Result<(), ModelError> {
        state.2 = demand.keys.iter().any(|id| id == LEFT || id == RIGHT);
        Ok(())
    }

    fn read_content(state: &Self::State, _: &Doc, item: Option<&str>) -> Result<Value, String> {
        let text = match item {
            Some(LEFT) => &state.0,
            Some(RIGHT) => &state.1,
            _ => return Err("content read requires an exact published id".into()),
        };
        if text == "broken" {
            return Err(format!("content decoder failed for {}", item.unwrap()));
        }
        Ok(json!({"id": item, "text": text}))
    }
}

struct NamespacedApp<const LAZY: bool>;
impl<const LAZY: bool> ResourceApp for NamespacedApp<LAZY> {
    type Resource = Namespaced<LAZY>;
    const ID: &'static str = "so.tilo.test-namespaced";
    const TITLE: &'static str = "Namespaced";
    fn view(cx: &mut ViewCx<Self::Resource>) -> Element {
        Box::new(cx.text(|state| state.0.clone()))
    }
}

fn namespaced_reads_agree<const LAZY: bool>() {
    let mut app = TestApp::<NamespacedApp<LAZY>>::new().unwrap();
    app.call("checkpoint", json!({"name": "before"})).unwrap();
    app.merge_peer_edit(|doc| {
        doc.put(ui_core::automerge::ROOT, "left", "left after")
            .unwrap();
        doc.put(ui_core::automerge::ROOT, "right", "right after")
            .unwrap();
        doc.commit();
    });
    app.call("checkpoint", json!({"name": "after"})).unwrap();
    let compared = app
        .call(
            "compare",
            json!({"from": "before", "to": "after", "items": [LEFT, RIGHT]}),
        )
        .unwrap()
        .unwrap();
    for (index, (id, namespace)) in [(LEFT, "left"), (RIGHT, "right")].into_iter().enumerate() {
        let pair = compared.get("comparisons").unwrap().get(index).unwrap();
        assert_eq!(pair.get("item").unwrap(), id);
        for (side, version) in [("before", "before"), ("after", "after")] {
            let read = app
                .call("read", json!({"version": version, "item": id}))
                .unwrap()
                .unwrap();
            let expected = json!({"id": id, "text": format!("{namespace} {version}")});
            assert_eq!(read.get("content").unwrap(), &expected);
            assert_eq!(
                pair.get(side).unwrap(),
                &json!({"status": "present", "content": expected})
            );
        }
    }
    app.merge_peer_edit(|doc| {
        doc.put(ui_core::automerge::ROOT, "right", "broken")
            .unwrap();
        doc.commit();
    });
    app.call("checkpoint", json!({"name": "broken"})).unwrap();
    for action in ["read", "compare"] {
        let args = if action == "read" {
            json!({"version": "broken", "item": RIGHT})
        } else {
            json!({"from": "before", "to": "broken", "items": [RIGHT]})
        };
        let error = refusal(app.call(action, args));
        assert!(
            error.contains(&format!("content decoder failed for {RIGHT}")),
            "{error}"
        );
    }
}

#[test]
fn compare_preserves_published_tree_namespaces() {
    namespaced_reads_agree::<false>();
}

#[test]
fn compare_preserves_lazy_published_tree_namespaces() {
    namespaced_reads_agree::<true>();
}

const ITEM: &str = "test.children/items/body";
struct Children<const FAIL_LOAD: bool>;
impl<const FAIL_LOAD: bool> Resource for Children<FAIL_LOAD> {
    const KIND: &'static str = "test.children";
    const LABEL: &'static str = "Children";
    const READABLE: bool = true;
    type State = Option<String>;
    type Action = readability::Noop;

    fn read(doc: &Doc) -> Result<Self::State, ModelError> {
        if doc.get(ui_core::automerge::ROOT, "body")?.is_none() {
            return Ok(None);
        }
        tilo_resource::model::read_key(doc, &ui_core::automerge::ROOT, "body").map(Some)
    }

    fn children(state: &Self::State) -> Vec<tilo_actions::Resource> {
        let mut folder = tilo_actions::Resource::new("test.children/items", "test.folder", "Items");
        if state.is_some() {
            folder
                .children
                .push(tilo_actions::Resource::new(ITEM, "test.item", "Body"));
        }
        vec![folder]
    }

    fn load(_: &mut Self::State, _: &Doc, demand: &Demand) -> Result<(), ModelError> {
        if FAIL_LOAD && !demand.keys.is_empty() {
            Err(ModelError::invalid("demand load failed"))
        } else {
            Ok(())
        }
    }

    fn read_content(state: &Self::State, _: &Doc, item: Option<&str>) -> Result<Value, String> {
        if item != Some(ITEM) {
            return Err("content read needs the full published item id".into());
        }
        match state.as_deref() {
            Some("broken") => Err("published content decoder failed".into()),
            Some(body) => Ok(json!(body)),
            None => Err("missing body".into()),
        }
    }
}

struct ChildrenApp<const FAIL_LOAD: bool>;
impl<const FAIL_LOAD: bool> ResourceApp for ChildrenApp<FAIL_LOAD> {
    type Resource = Children<FAIL_LOAD>;
    const ID: &'static str = "so.tilo.test-children";
    const TITLE: &'static str = "Children";
    fn view(cx: &mut ViewCx<Self::Resource>) -> Element {
        Box::new(cx.text(|state| state.clone().unwrap_or_default()))
    }
}

fn heads<const FAIL_LOAD: bool>(app: &TestApp<ChildrenApp<FAIL_LOAD>>) -> String {
    app.peer_document()
        .base_heads()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("+")
}

fn body<const FAIL_LOAD: bool>(app: &mut TestApp<ChildrenApp<FAIL_LOAD>>, value: Option<&str>) {
    app.merge_peer_edit(|doc| {
        if let Some(value) = value {
            doc.put(ui_core::automerge::ROOT, "body", value).unwrap();
        } else {
            doc.delete(ui_core::automerge::ROOT, "body").unwrap();
        }
        doc.commit();
    });
}

#[test]
fn compare_children_only_present_added_and_deleted() {
    let mut app = TestApp::<ChildrenApp<false>>::new().unwrap();
    let empty = heads(&app);
    body(&mut app, Some("early"));
    let added = app
        .call("compare", json!({"from": empty, "items": [ITEM]}))
        .unwrap()
        .unwrap();
    assert_eq!(
        added["comparisons"][0]["before"],
        json!({"status": "absent"})
    );
    assert_eq!(
        added["comparisons"][0]["after"],
        json!({"status": "present", "content": "early"})
    );
    let early = heads(&app);
    body(&mut app, Some("late"));
    let result_spec = app
        .actions()
        .unwrap()
        .into_iter()
        .find(|descriptor| descriptor.address.action == "compare")
        .unwrap()
        .contract
        .result;
    let changed = app
        .call("compare", json!({"from": early, "items": [ITEM]}))
        .unwrap()
        .unwrap();
    assert_eq!(
        changed["comparisons"][0]["before"],
        json!({"status": "present", "content": "early"})
    );
    assert_eq!(
        changed["comparisons"][0]["after"],
        json!({"status": "present", "content": "late"})
    );
    let validate = |value| {
        result_spec.validate(&tilo_actions::ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        })
    };
    assert!(validate(changed.clone()).is_ok());
    for content in [Value::Null, json!(42), json!(["adapter", "content"])] {
        let mut unconstrained = changed.clone();
        unconstrained["comparisons"][0]["after"]["content"] = content;
        assert!(validate(unconstrained).is_ok());
    }
    let mut missing_heads = changed.clone();
    missing_heads.as_object_mut().unwrap().remove("from_heads");
    assert!(validate(missing_heads).is_err());
    let mut bad_heads = changed.clone();
    bad_heads["to_heads"] = json!([42]);
    assert!(validate(bad_heads).is_err());
    let mut bad_item = changed.clone();
    bad_item["comparisons"][0]["item"] = json!(42);
    assert!(validate(bad_item).is_err());
    for bad_side in [
        json!({"status": "unknown"}),
        json!({"status": "present"}),
        json!({"status": "absent", "content": "not absent"}),
    ] {
        let mut malformed = changed.clone();
        malformed["comparisons"][0]["before"] = bad_side;
        assert!(validate(malformed).is_err());
    }
    body(&mut app, None);
    let deleted = app
        .call("compare", json!({"from": early, "items": [ITEM]}))
        .unwrap()
        .unwrap();
    assert_eq!(
        deleted["comparisons"][0]["before"],
        json!({"status": "present", "content": "early"})
    );
    assert_eq!(
        deleted["comparisons"][0]["after"],
        json!({"status": "absent"})
    );
}

#[test]
fn compare_published_children_precede_fallback_and_preserve_errors() {
    let mut app = TestApp::<ChildrenApp<true>>::new().unwrap();
    body(&mut app, Some("public"));
    let public = heads(&app);
    let answer = app
        .call("compare", json!({"from": public, "items": [ITEM]}))
        .unwrap()
        .unwrap();
    for side in ["before", "after"] {
        assert_eq!(
            answer["comparisons"][0][side],
            json!({"status": "present", "content": "public"})
        );
    }
    let error = refusal(app.call(
        "compare",
        json!({"from": public, "items": ["test.children/items/missing"]}),
    ));
    assert!(error.contains("demand load failed"), "{error}");
    body(&mut app, Some("broken"));
    let error = refusal(app.call("compare", json!({"from": public, "items": [ITEM]})));
    assert!(
        error.contains("published content decoder failed"),
        "{error}"
    );
}
