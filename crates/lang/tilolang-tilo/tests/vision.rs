//! The image model as a tilolang package: a view reads the model's reading
//! through the root the session publishes, and calls `describe` on it as any
//! app's action, with a picture from the workspace's content store.

use crate::fixtures::Sent;
use std::cell::RefCell;
use std::rc::Rc;

use serde_json::json;
use tilolang::IdsLock;
use tilolang_runtime::delegate::Invocation;
use tilolang_runtime::{Actor, ModuleHash, ResourceRef, Value, ViewInstance};
use tilolang_tilo::host::Host;

/// A view over the image model: what it answers with now, and a button that
/// asks it to describe a picture.
const VIEW: &str = r#"app test.vision "Vision"

use so.tilo.vision as vision

view model(m: vision.ImageModel) {
  stack {
    text("{m.model}")
    button("Describe", on: vision.describe(vision.image_model, image: "tilo.content:fixture")) as describe
  }
}
"#;

/// The image model's resource, as a view names it through its root.
fn image_model() -> ResourceRef {
    ResourceRef::parse_id("vision:vision.image-model", "so.tilo.vision#ImageModel")
        .expect("a resource id")
}

/// A host that runs `VIEW` over the platform's package for the image model, as
/// the session publishes it with a vision model ready.
fn workspace() -> (Host, ViewInstance, String, ModuleHash) {
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .unwrap();
    let metas = tilolang_tilo::platform::platform_packages().expect("the platform's packages read");
    for meta in &metas {
        host.runtime()
            .load_foreign(meta.clone())
            .expect("a package loads");
    }
    let compiled = tilolang::compile_package(VIEW, &IdsLock::new(), &metas)
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    let module = tilolang_runtime::Runtime::module_hash(&compiled.wasm);
    let package = host.load_compiled(&compiled).unwrap();
    let view = package
        .meta
        .views
        .iter()
        .find(|view| view.name == "model")
        .expect("the view's declaration")
        .id
        .clone();
    let describe = package
        .meta
        .views
        .iter()
        .flat_map(|view| &view.anchors)
        .find(|anchor| anchor.name == "describe")
        .expect("the button's anchor")
        .id
        .clone();
    host.runtime()
        .set_reading(
            &image_model(),
            &json!({
                "available": true,
                "provider": "local",
                "model": "vision-1",
                "revision": "a".repeat(40),
                "faces": false,
                "detail": "",
            })
            .to_string(),
        )
        .expect("the model's reading");
    let open = host.open_resource(&image_model(), Some(&view)).unwrap();
    (host, open, describe, module)
}

/// The text the view shows.
fn shown(view: &ViewInstance) -> Value {
    fn first(node: &tilolang_runtime::UiNode) -> Option<Value> {
        if node.kind == "ui.Text" {
            return node.props.get("value").cloned();
        }
        node.children.iter().find_map(first)
    }
    first(view.tree()).expect("the view shows the model")
}

#[test]
fn describe_is_called_on_the_root_the_session_publishes() {
    let (host, view, describe, _module) = workspace();
    assert_eq!(shown(&view), Value::text("vision-1"));
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    view.press(&describe).expect("the button asks describe");
    let sent = sent.borrow();
    let [
        Invocation::Action {
            app,
            action,
            name,
            args,
            target,
            ..
        },
    ] = sent.as_slice()
    else {
        panic!("one invocation of describe: {sent:?}");
    };
    assert_eq!(
        (app.as_str(), action.as_str(), name.as_str()),
        (
            "so.tilo.vision",
            "so.tilo.vision#ImageModel/describe",
            "describe"
        ),
        "the image model's verb, named here by its app, reaches it as the verb it publishes"
    );
    assert_eq!(
        target.as_ref(),
        Some(&image_model()),
        "the call names the model root the session publishes"
    );
    assert_eq!(
        args[1],
        ("image".to_owned(), Value::text("tilo.content:fixture")),
        "{args:?}"
    );
}

#[test]
fn every_root_the_session_publishes_resolves_to_its_resource() {
    let metas = tilolang_tilo::platform::platform_packages().expect("the platform's packages");
    let vision = metas
        .iter()
        .find(|meta| meta.app == tilolang_tilo::platform::VISION_APP)
        .expect("the image model is a platform package");
    let roots: Vec<(&str, &str)> = vision
        .named_roots
        .iter()
        .map(|root| {
            (
                root.name.as_str(),
                root.published.as_deref().expect("a published address"),
            )
        })
        .collect();
    assert_eq!(
        roots,
        [
            ("image_model", "vision:vision.image-model"),
            ("analysis", "vision:vision.analysis"),
        ]
    );
}
