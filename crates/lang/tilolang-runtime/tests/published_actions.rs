//! A Rust view's action on a verb its resource publishes, with no package declaring it: Chat's
//! channel publishes `post`, the session's service answers it, and the view names it on the
//! channel it shows. The runtime sends that invocation to the app's door — the one path another
//! app's declared action takes (one-app-model.md §2, §4).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use crate::common::*;
    use serde_json::json;
    use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
    use tilolang_runtime::published::{Published, PublishedAction};
    use tilolang_runtime::rust_views::{
        RustEvent, RustNode, RustRendered, RustResource, RustViewDecl, RustViews,
    };
    use tilolang_runtime::{
        AmStore, Meta, Principal, ResourceRef, Runtime, SimpleAuthority, Value, ViewInstance,
    };

    /// The app whose channel a view shows, and the verb its channel publishes.
    const APP: &str = "chat";
    const TYPE: &str = "chat#Channel";

    /// The app's description: its channel's type. The channel's `post` is not here — the
    /// session's service publishes it, and no package declares it.
    fn chat() -> Meta {
        meta(json!({
            "app": APP,
            "title": "Chat",
            "types": [{"id": "Channel", "name": "Channel", "fields": [
                {"id": "Channel.id", "name": "id", "type": "Text"},
            ]}],
        }))
    }

    /// A tilolang view that shows a channel (`show(r)`, the system's place-a-resource node):
    /// the channel is drawn by the Rust app's own view, as one-app-model.md §2 has it.
    const CANVAS: &str = r#"app test.canvas "Canvas"

use chat as chat

view canvas() {
  stack {
    let r = resource("chat.workspace/channels/c_ask", of: chat.Channel)
    show(r)
  }
}
"#;

    /// The channel the view shows, as the session publishes it.
    fn channel() -> ResourceRef {
        ResourceRef::parse_id("chat.workspace/channels/c_ask", TYPE).expect("a resource id")
    }

    /// The Rust view: a button that presses `post` on the channel it shows, with what it says.
    struct Send;

    impl RustViews for Send {
        fn app(&self) -> &'static str {
            APP
        }

        fn kinds(&self) -> Vec<String> {
            vec!["chat.Channel".to_owned()]
        }

        fn views_of(&self, _kind: &str) -> Vec<RustViewDecl> {
            vec![RustViewDecl {
                name: "channel".to_owned(),
                is_default: true,
            }]
        }

        fn resolve(&self, _kind: &str, name: &str) -> Option<RustViewDecl> {
            (name.is_empty() || name == "channel").then(|| RustViewDecl {
                name: "channel".to_owned(),
                is_default: true,
            })
        }

        fn render(
            &self,
            _kind: &str,
            _name: &str,
            target: &RustResource,
            _reading: Option<&serde_json::Value>,
            _person: &Principal,
            _window: &str,
        ) -> RustRendered {
            let button = RustNode {
                kind: "ui.Button".to_owned(),
                key: "send".to_owned(),
                props: BTreeMap::from([("label".to_owned(), Value::text("Send"))]),
                ..RustNode::default()
            };
            let events = BTreeMap::from([(
                ("send".to_owned(), "press".to_owned()),
                RustEvent::Invoke {
                    resource: String::new(),
                    action: "post".to_owned(),
                    args: vec![Value::text(target.id.clone()), Value::text("hi")],
                },
            )]);
            RustRendered {
                tree: RustNode {
                    kind: "ui.Stack".to_owned(),
                    key: "channel".to_owned(),
                    children: vec![button],
                    ..RustNode::default()
                },
                events,
            }
        }
    }

    /// Records what the view sends the app's door.
    struct Sent(Rc<RefCell<Vec<Invocation>>>);

    impl Invoker for Sent {
        fn invoke(&self, invocation: &Invocation) -> Invoked {
            self.0.borrow_mut().push(invocation.clone());
            Invoked::Done(Value::Unit)
        }
    }

    /// The canvas drawn over a channel whose app publishes `post` from `published` (the host's
    /// table), with `sent` taking what the views run.
    fn opened(
        published: Option<(Vec<String>, String)>,
        sent: &Rc<RefCell<Vec<Invocation>>>,
    ) -> ViewInstance {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        let app = chat();
        rt.load_foreign(app.clone()).unwrap();
        if let Some((params, app)) = published {
            let mut table = Published::default();
            table.insert(
                channel().id_string(),
                "post",
                PublishedAction { app, params },
            );
            rt.set_published_actions(table);
        }
        rt.set_rust_views(Rc::new(Send));
        rt.set_invoker(Sent(Rc::clone(sent)));
        rt.set_reading(&channel(), &json!({ "id": "c_ask" }).to_string())
            .expect("a reading of the channel");
        let canvas = App::compile(CANVAS, std::slice::from_ref(&app));
        canvas.load(&rt);
        rt.open_view(&Principal::new("ada"), &canvas.view("canvas"), Vec::new())
            .unwrap_or_else(|why| panic!("{why}"))
    }

    /// The node the Rust view's button was drawn as, as the runtime keys it in the canvas.
    fn send_key(view: &ViewInstance) -> String {
        fn find(node: &tilolang_runtime::UiNode) -> Option<String> {
            if node.kind == "ui.Button" {
                return Some(node.key.clone());
            }
            node.children.iter().find_map(find)
        }
        find(view.tree())
            .unwrap_or_else(|| panic!("the channel's page is drawn: {}", dump(view.tree())))
    }

    #[test]
    fn a_rust_views_press_of_a_published_verb_reaches_the_apps_door() {
        let sent = Rc::new(RefCell::new(Vec::new()));
        let view = opened(
            Some((
                vec!["channel".to_owned(), "text".to_owned()],
                APP.to_owned(),
            )),
            &sent,
        );
        let key = send_key(&view);
        view.fire(&key, "press")
            .unwrap_or_else(|why| panic!("{why}"));
        let sent = sent.borrow();
        let [
            Invocation::Action {
                app,
                action,
                args,
                subject,
                ..
            },
        ] = sent.as_slice()
        else {
            panic!("one invocation of the channel's verb: {sent:?}");
        };
        assert_eq!((app.as_str(), action.as_str()), (APP, "post"));
        assert_eq!(subject.as_ref(), Some(&channel()));
        // The arguments after the subject, by the names the app publishes, in the order it
        // declares them: the channel it names and the text the view wrote.
        assert_eq!(
            args,
            &[
                ("channel".to_owned(), Value::text(channel().id_string())),
                ("text".to_owned(), Value::text("hi")),
            ],
            "the verb's arguments by name, after its subject"
        );
    }

    #[test]
    fn a_verb_no_app_publishes_is_still_no_action() {
        // The same view over a channel whose app publishes nothing: no table entry, no package,
        // so the press is the runtime's refusal — never a silent no-op.
        let sent = Rc::new(RefCell::new(Vec::new()));
        let view = opened(None, &sent);
        let key = send_key(&view);
        let why = view
            .fire(&key, "press")
            .expect_err("no app publishes `post`");
        assert!(
            format!("{why}").contains("post"),
            "the refusal names the verb: {why}"
        );
        assert!(sent.borrow().is_empty(), "nothing was sent on");
    }
}
