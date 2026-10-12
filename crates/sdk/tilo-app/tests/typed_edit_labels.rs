#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]

//! A custom surface's typed edits are recorded in history as what they did:
//! the action the edit is, never the generic `apply` they travel as.

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
            Column::new().push(Box::new(Button::new("erase", "Erase").on_click(
                move || {
                    dispatch.apply(&SketchAction::RemoveMarks {
                        ids: vec!["a".into()],
                    });
                },
            ))),
        )
    }
}

fn last_did(app: &TestApp<SketchApp>) -> tilo_actions::Did {
    app.versions()
        .last()
        .and_then(|version| version.context.as_ref())
        .and_then(|context| context.what.clone())
        .expect("the last version records what its action did")
}

fn verb_name(did: &tilo_actions::Did) -> &str {
    match did {
        tilo_actions::Did::Verb(verb) => &verb.action.action,
        other => panic!("a verb: {other:?}"),
    }
}

#[test]
fn typed_edits_are_recorded_in_history_as_what_they_did() {
    let mut app = TestApp::<SketchApp>::new().unwrap();

    // A surface's dispatcher submits the edit as `apply`; history records
    // the declared action it is.
    app.harness().click_named("erase").unwrap();
    assert_eq!(app.state(), 1);
    assert_eq!(verb_name(&last_did(&app)), "remove-marks");

    // An edit with no declaration is recorded as its variant.
    app.invoke(Invoke::apply(&moved("a")).unwrap()).unwrap();
    assert_eq!(verb_name(&last_did(&app)), "move-marks");

    // A group of one kind of edit records it once; a mixed one each member.
    app.invoke(Invoke::group([
        Invoke::apply(&moved("a")).unwrap(),
        Invoke::apply(&moved("b")).unwrap(),
    ]))
    .unwrap();
    let tilo_actions::Did::Group(same) = last_did(&app) else {
        panic!("a group");
    };
    assert_eq!(same.count, 2);
    assert!(
        same.members
            .iter()
            .all(|member| verb_name(member) == "move-marks")
    );
    app.invoke_confirmed(Invoke::group([
        Invoke::apply(&moved("a")).unwrap(),
        Invoke::apply(&SketchAction::RemoveMarks {
            ids: vec!["b".into()],
        })
        .unwrap(),
    ]))
    .unwrap();
    let tilo_actions::Did::Group(mixed) = last_did(&app) else {
        panic!("a group");
    };
    assert!(mixed.shared().is_none());
    assert_eq!(
        mixed.members.iter().map(verb_name).collect::<Vec<_>>(),
        ["move-marks", "remove-marks"]
    );

    // An agent's `history` lists the same entries, with a sentence each that
    // never names the generic `apply`.
    let listed = app.call("history", json!({})).unwrap().unwrap();
    let versions = listed["versions"].as_array().unwrap();
    assert!(
        versions
            .iter()
            .any(|version| version["what"]["action"]["action"] == "move-marks"),
        "{listed}"
    );
    assert!(
        versions.iter().all(|version| !version["sentence"]
            .as_str()
            .unwrap_or_default()
            .starts_with("Apply")),
        "{listed}"
    );
}
