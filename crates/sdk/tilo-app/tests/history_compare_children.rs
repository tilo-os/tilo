//! A readable resource may publish nested children without overriding `child`.
use super::*;
use tilo_resource::{Demand, Doc, ModelError};
use ui_core::automerge::ReadDoc;

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
        if item != Some("body") {
            return Err("content read needs the body item key".into());
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
