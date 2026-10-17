//! Retained results must come from the real framework semantic door.
use super::{ResourceApp, ViewCx, testing::TestApp};
use crate::prelude::*;
use serde_json::{Map, Value, json};
use tilo_resource::{Doc, ModelError};
use ui_core::automerge::{ROOT, ReadDoc, transaction::Transactable};
use ui_core::{
    headless::{Action, Invocation},
    widget::Element,
};

struct Document;
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Noop;
impl tilo_actions::Action for Noop {
    fn apply(&self, _: &mut Doc) -> Result<(), ActionError> {
        Ok(())
    }
}
// Packaging scans source files directly, so keep this fixture explicitly test-only.
#[cfg(test)]
impl Resource for Document {
    const KIND: &'static str = "test.retained-document";
    const LABEL: &'static str = "Document";
    const READABLE: bool = true;
    type State = String;
    type Action = Noop;
    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        doc.put(ROOT, "body", "visible evidence")?;
        Ok(())
    }
    fn read(doc: &Doc) -> Result<String, ModelError> {
        doc.get(ROOT, "body")?
            .and_then(|(value, _)| value.as_str().map(str::to_owned))
            .ok_or_else(|| ModelError::invalid("missing document body"))
    }
    fn children(_: &String) -> Vec<tilo_actions::Resource> {
        vec![
            tilo_actions::Resource::new("public-body", "text", "Public body"),
            tilo_actions::Resource::new("private-account", "account", "Secret account")
                .with_action(
                    tilo_actions::ActionSpec::operation(
                        "render",
                        "Private view",
                        tilo_actions::OperationKind::View,
                        "test.private-view",
                        &Noop,
                    )
                    .unwrap(),
                ),
        ]
    }
    fn document_child(_: &String, id: &str) -> bool {
        id == "public-body"
    }
    fn read_content(state: &String, _: &Doc, item: Option<&str>) -> Result<Value, String> {
        if item.is_some_and(|id| id != "public-body") {
            return Err("not part of the document".into());
        }
        Ok(json!({"body":state}))
    }
}
struct DocumentApp;
impl ResourceApp for DocumentApp {
    type Resource = Document;
    const ID: &'static str = "so.tilo.test-retained-document";
    const TITLE: &'static str = "Document";
    fn view(cx: &mut ViewCx<Document>) -> Element {
        Box::new(cx.text(Clone::clone))
    }
}
fn invoke(action: &str, args: Map<String, Value>, expected: Option<&str>) -> Action {
    Action::Invoke(Invocation {
        resource: "test.retained-document".into(),
        action: action.into(),
        args,
        confirm: false,
        confirmation_phrase: None,
        expect_error: expected.map(str::to_owned),
    })
}

#[test]
fn readable_parent_does_not_promote_private_custom_children() {
    let mut app = TestApp::<DocumentApp>::new().unwrap();
    app.settle();
    let resources = app.harness().resources();
    let root = resources
        .iter()
        .find(|r| r.id.as_str() == Document::KIND)
        .unwrap();
    let public = root
        .children
        .iter()
        .find(|r| r.id.as_str() == "public-body")
        .unwrap();
    let private = root
        .children
        .iter()
        .find(|r| r.id.as_str() == "private-account")
        .unwrap();
    assert!(public.actions.iter().any(|a| a.contract.reads_document()));
    assert!(!private.actions.iter().any(|a| a.contract.reads_document()));
    assert_eq!(private.label, "Secret account");
    assert_eq!(private.actions[0].kind, tilo_actions::OperationKind::View);
    assert!(
        app.call("read", json!({"item": "private-account"}))
            .is_err()
    );
}

#[test]
fn activation_is_a_view_action_that_preserves_the_document_and_owner() {
    let mut app = TestApp::<DocumentApp>::new().unwrap();
    app.settle();
    let record = app.record().clone();
    let heads = app.read_document(Transactable::base_heads);
    let shown = app
        .call(tilo_actions::remote::APPLICATION_ACTIVATE_ACTION, json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(shown, json!({"activation_requested": true}));
    assert!(
        app.harness().root_widget().needs_redraw(),
        "the platform can observe the pending activation while the view is otherwise idle"
    );
    assert!(ui_core::component_lifecycle::take_window_activation_intent(
        app.harness().root_widget()
    ));
    assert!(
        !ui_core::component_lifecycle::take_window_activation_intent(app.harness().root_widget())
    );
    assert_eq!(app.record(), &record);
    assert_eq!(app.state(), "visible evidence");
    assert_eq!(app.read_document(Transactable::base_heads), heads);
}
#[test]
fn semantic_read_render_and_refusal_are_retained_with_real_content() {
    let mut app = TestApp::<DocumentApp>::new().unwrap();
    app.settle();
    app.harness().retain_invocation_results();
    app.harness()
        .perform(&invoke("read", Map::new(), None))
        .unwrap();
    app.harness()
        .perform(&invoke("render", Map::new(), None))
        .unwrap();
    app.harness()
        .perform(&invoke(
            "missing",
            Map::from_iter([("secret".into(), json!("never-copy-this"))]),
            Some("unknown_action"),
        ))
        .unwrap();
    let raw = app.harness().invocation_results_json().unwrap();
    assert!(!raw.contains("never-copy-this"));
    let journal: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(journal["entries"].as_array().unwrap().len(), 3);
    assert_eq!(journal["entries"][0]["status"], "completed");
    assert!(
        journal["entries"][0]["output"]["value"]
            .to_string()
            .contains("visible evidence")
    );
    let image = &journal["entries"][1]["output"]["value"];
    assert!(image["width"].as_u64().unwrap() > 0);
    assert!(image["height"].as_u64().unwrap() > 0);
    let address = image["content"].as_str().unwrap();
    assert_eq!(journal["entries"][1]["output"]["resources"][0], address);
    let hash = tilo_actions::content_hash(address).unwrap();
    let bytes = app
        .content()
        .read(hash)
        .expect("actual returned content exists");
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(tilo_content::verify(hash, &bytes));
    assert_eq!(journal["entries"][2]["status"], "refused");
    assert_eq!(journal["entries"][2]["code"], "unknown_action");
    assert!(journal.get("retention_error").is_none());
}
