#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A custom surface's typed edits are named in history after what they did:
//! the declared action's label when the edit is one, else its variant in
//! words — never after the generic `apply` they travel as.

use serde_json::json;
use tilo_actions::{Action, ActionError, ActionParam, ParamType};
use tilo_app::automerge::transaction::Transactable;
use tilo_app::automerge::{ROOT, ReadDoc, ScalarValue, Value};
use tilo_app::prelude::*;
use tilo_resource::{ActionDecl, Doc, Invoke, ModelError};

const ACTION_TYPE: &str = "typed-edit-labels.sketch.action";
const COUNT: &str = "count";

#[derive(
    Clone,
    Debug,
    tilo_resource::__private::serde::Serialize,
    tilo_resource::__private::serde::Deserialize,
)]
#[serde(
    crate = "tilo_resource::__private::serde",
    tag = "action",
    rename_all = "kebab-case"
)]
enum SketchAction {
    RemoveMarks { ids: Vec<String> },
    MoveMarks { ids: Vec<String> },
}

struct Sketch;

impl Resource for Sketch {
    const KIND: &'static str = "typed-edit-labels.sketch";
    const LABEL: &'static str = "Sketch";
    const ACTION_TYPE: &'static str = ACTION_TYPE;
    type State = i64;
    type Action = SketchAction;

    fn read(doc: &Doc) -> Result<Self::State, ModelError> {
        Ok(count(doc))
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![ActionDecl::new(
            "remove-marks",
            "Remove the marks",
            vec![ActionParam::required("ids", ParamType::Array, "Mark ids")],
            |_, _| Ok(SketchAction::RemoveMarks { ids: Vec::new() }),
        )]
    }
}

impl Action for SketchAction {
    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), ActionError> {
        let next = count(doc).saturating_add(1);
        doc.put(ROOT, COUNT, next)?;
        Ok(())
    }
}

fn count(doc: &tilo_app::automerge::AutoCommit) -> i64 {
    match doc.get(ROOT, COUNT) {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            ScalarValue::Int(count) => *count,
            _ => 0,
        },
        _ => 0,
    }
}

fn moved(id: &str) -> SketchAction {
    SketchAction::MoveMarks {
        ids: vec![id.into()],
    }
}

struct SketchApp;

impl ResourceApp for SketchApp {
    type Resource = Sketch;
    const ID: &'static str = "so.tilo.typed-edit-labels";
    const TITLE: &'static str = "Sketch";

    fn view(cx: &mut ViewCx<Sketch>) -> Element {
        let dispatch = cx.dispatcher();
        Box::new(
            Column::new().push(Box::new(Button::new("erase", "Erase", None).on_click(
                move || {
                    dispatch.apply(&SketchAction::RemoveMarks {
                        ids: vec!["a".into()],
                    });
                },
            ))),
        )
    }
}

fn last_label(app: &TestApp<SketchApp>) -> String {
    app.versions()
        .last()
        .and_then(|version| version.context.as_ref())
        .map(|context| context.action_label.clone())
        .expect("the last version was made by an action")
}

#[test]
fn typed_edits_are_named_in_history_after_what_they_did() {
    let mut app = TestApp::<SketchApp>::new().unwrap();

    // A surface's dispatcher submits the edit as `apply`; history names the
    // declared action it is.
    app.harness().click_named("erase").unwrap();
    assert_eq!(app.state(), 1);
    assert_eq!(last_label(&app), "Remove the marks");

    // An edit with no declaration is named after its variant.
    app.invoke(Invoke::apply(&moved("a")).unwrap()).unwrap();
    assert_eq!(last_label(&app), "Move marks");

    // A group of one kind of edit is named after it; a mixed one is a group.
    app.invoke(Invoke::group([
        Invoke::apply(&moved("a")).unwrap(),
        Invoke::apply(&moved("b")).unwrap(),
    ]))
    .unwrap();
    assert_eq!(last_label(&app), "Move marks");
    app.invoke_confirmed(Invoke::group([
        Invoke::apply(&moved("a")).unwrap(),
        Invoke::apply(&SketchAction::RemoveMarks {
            ids: vec!["b".into()],
        })
        .unwrap(),
    ]))
    .unwrap();
    assert_eq!(last_label(&app), "Apply several Sketch edits as one");

    // An agent's `history` lists the same names.
    let listed = app.call("history", json!({})).unwrap().unwrap();
    let actions: Vec<&str> = listed["versions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|version| version["action"].as_str())
        .collect();
    assert!(
        !actions
            .iter()
            .any(|action| action.starts_with("Apply Sketch")),
        "{listed}"
    );
    assert!(actions.contains(&"Move marks"), "{listed}");
}
