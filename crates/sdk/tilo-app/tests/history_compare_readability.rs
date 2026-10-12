//! Even an implemented content reader is not authority when READABLE is false.
use super::*;
use tilo_resource::{Doc, ModelError};

struct Guarded<const READABLE: bool>;
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Noop;
impl tilo_actions::Action for Noop {
    fn apply(&self, _: &mut Doc) -> Result<(), ActionError> {
        Ok(())
    }
}
impl<const READABLE: bool> Resource for Guarded<READABLE> {
    const KIND: &'static str = "test.guarded";
    const LABEL: &'static str = "Guarded";
    const READABLE: bool = READABLE;
    type State = String;
    type Action = Noop;
    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        doc.put(ui_core::automerge::ROOT, "body", "secret")
            .map_err(Into::into)
    }
    fn read(_: &Doc) -> Result<String, ModelError> {
        Ok("secret".into())
    }
    fn child(_: &String, id: &str) -> Option<tilo_actions::Resource> {
        Some(tilo_actions::Resource::new(id, "test.item", "Item"))
    }
    fn read_content(_: &String, _: &Doc, _: Option<&str>) -> Result<Value, String> {
        if READABLE {
            Err("content decoder failed".into())
        } else {
            Ok(json!({"body": "secret"}))
        }
    }
}
struct GuardedApp<const READABLE: bool>;
impl<const READABLE: bool> ResourceApp for GuardedApp<READABLE> {
    type Resource = Guarded<READABLE>;
    const ID: &'static str = "so.tilo.test-guarded";
    const TITLE: &'static str = "Guarded";
    fn view(cx: &mut ViewCx<Self::Resource>) -> Element {
        Box::new(cx.text(Clone::clone))
    }
}

#[test]
fn compare_never_discloses_non_readable_content() {
    let mut app = TestApp::<GuardedApp<false>>::new().unwrap();
    let heads = app
        .peer_document()
        .base_heads()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("+");
    for args in [
        json!({"from": heads}),
        json!({"from": heads, "items": ["item"]}),
    ] {
        let message = refusal(app.call("compare", args));
        assert!(
            message.contains("does not offer content reads"),
            "{message}"
        );
        assert!(!message.contains("secret"));
    }
}

#[test]
fn compare_content_decoder_error_is_not_absent() {
    let mut app = TestApp::<GuardedApp<true>>::new().unwrap();
    let heads = app
        .peer_document()
        .base_heads()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("+");
    let message = refusal(app.call("compare", json!({"from": heads, "items": ["item"]})));
    assert!(message.contains("content decoder failed"), "{message}");
}
