//! A Rust view's action on a verb its resource publishes, with no package declaring it: Chat's
//! channel publishes `post`, the session's service answers it, and the view names it on the
//! channel it shows. The runtime sends that invocation to the app's action API — the one path another
//! app's declared action takes (one-app-model.md §2, §4).

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
    use tilolang_runtime::{Actor, AmStore, Meta, ResourceRef, Runtime, Value, ViewInstance};

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
            _person: &Actor,
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

    /// Records what the view sends the app's action API.
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
        let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
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
        rt.open_view(&person("ada"), &canvas.view("canvas"), Vec::new())
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

    tilo_test_env::scenario! {
        fn a_published_verb_reaches_only_the_app_that_publishes_it;

        phase a_rust_views_press_of_a_published_verb_reaches_the_apps_api;
        {
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
                    target,
                    ..
                },
            ] = sent.as_slice()
            else {
                panic!("one invocation of the channel's verb: {sent:?}");
            };
            assert_eq!((app.as_str(), action.as_str()), (APP, "post"));
            assert_eq!(target.as_ref(), Some(&channel()));
            // The arguments after the target, by the names the app publishes, in the order it
            // declares them: the channel it names and the text the view wrote.
            assert_eq!(
                args,
                &[
                    ("channel".to_owned(), Value::text(channel().id_string())),
                    ("text".to_owned(), Value::text("hi")),
                ],
                "the verb's arguments by name, after its target"
            );
        }

        phase a_verb_no_app_publishes_is_still_no_action;
        {
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

    /// A tilolang view whose controls are bound to an action of an app the runtime does not
    /// run: a menu row (`choose`) and a button (`press`), as the shell's workspace menu binds
    /// the session's `delete-workspace`.
    const CONTROLS: &str = r#"app test.canvas "Canvas"

use chat as chat

view canvas() {
  stack {
    ui.MenuItem("Remove the channel") {
      on choose: chat.remove(channel: "c_ask")
    }
    ui.Button("Remove") {
      on press: chat.remove(channel: "c_ask")
    }
    ui.SwipeAction("Remove") {
      on swipe: chat.remove(channel: "c_ask")
    }
  }
}
"#;

    /// The app whose `remove` the view binds: an action of a described package, which the
    /// runtime sends to the app's own API, and whose availability the app publishes.
    fn chat_with_remove() -> Meta {
        meta(json!({
            "app": APP,
            "title": "Chat",
            "types": [{"id": "Channel", "name": "Channel", "fields": [
                {"id": "Channel.id", "name": "id", "type": "Text"},
            ]}],
            "actions": [{"id": "remove", "name": "remove", "class": "undoable",
                         "params": [{"name": "channel", "type": "Text"}]}],
        }))
    }

    /// The canvas drawn over `chat_with_remove`, with `unavailable` as the actions the host
    /// publishes as unable to run now, and the runtime it draws on.
    fn controls(unavailable: tilolang_runtime::Unavailable) -> (Runtime, ViewInstance) {
        let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
        let app = chat_with_remove();
        rt.load_foreign(app.clone()).unwrap();
        rt.set_unavailable_actions(unavailable);
        let canvas = App::compile(CONTROLS, std::slice::from_ref(&app));
        canvas.load(&rt);
        let view = rt
            .open_view(&person("ada"), &canvas.view("canvas"), Vec::new())
            .unwrap_or_else(|why| panic!("{why}"));
        (rt, view)
    }

    /// The one node of `kind` the tree draws, which must be there.
    fn one<'a>(view: &'a ViewInstance, kind: &str) -> &'a tilolang_runtime::UiNode {
        match of_kind(view.tree(), kind).as_slice() {
            [only] => only,
            other => panic!("one {kind}, found {}: {}", other.len(), dump(view.tree())),
        }
    }

    tilo_test_env::scenario! {
        fn an_action_a_rust_app_publishes_as_unavailable_is_drawn_disabled_with_its_reason;

        phase an_unavailable_action_disables_every_control_bound_to_it_and_says_why;
        {
            let mut unavailable = tilolang_runtime::Unavailable::default();
            unavailable.insert("remove", "the channel is the workspace's");
            let (_rt, view) = controls(unavailable);
            let menu = one(&view, "ui.MenuItem");
            assert_eq!(
                menu.props.get("disabled"),
                Some(&Value::Bool(true)),
                "a menu row it cannot run stands disabled: {}",
                dump(view.tree())
            );
            assert!(
                !menu.props.contains_key("enabled"),
                "a menu row says it in `disabled`, never in `enabled`"
            );
            assert_eq!(
                menu.props.get("description"),
                Some(&Value::text("the channel is the workspace's")),
                "the reason is the row's, for a person to read before pressing"
            );
            assert_eq!(
                menu.events.keys().map(String::as_str).collect::<Vec<_>>(),
                vec!["choose"],
                "the row keeps what choosing it runs: it is not a mark, and the press is refused \
                 with the same reason by the action layer"
            );
            let button = one(&view, "ui.Button");
            assert_eq!(
                button.props.get("enabled"),
                Some(&Value::Bool(false)),
                "a button it cannot run is drawn disabled: {}",
                dump(view.tree())
            );
            assert_eq!(
                button.props.get("description"),
                Some(&Value::text("the channel is the workspace's")),
                "the reason is the button's too"
            );
            // A swipe has no way to stand disabled: it is handed nothing it does not declare,
            // since the window refuses a whole patch with an undeclared prop and would show the
            // page before this one from then on. Swiping it is refused with the reason.
            let swipe = one(&view, "ui.SwipeAction");
            assert!(
                !swipe.props.contains_key("enabled") && !swipe.props.contains_key("description"),
                "a swipe is given only the props it declares: {}",
                dump(view.tree())
            );
            assert_eq!(
                swipe.events.keys().map(String::as_str).collect::<Vec<_>>(),
                vec!["swipe"],
                "the swipe keeps what it runs"
            );
        }

        phase the_controls_are_enabled_again_when_the_app_publishes_the_action_available;
        {
            let mut unavailable = tilolang_runtime::Unavailable::default();
            unavailable.insert("remove", "the channel is the workspace's");
            let (rt, mut view) = controls(unavailable);
            // The app publishes it available again: the view runs again and draws what it says.
            rt.set_unavailable_actions(tilolang_runtime::Unavailable::default());
            assert!(
                view.is_stale(),
                "a view that drew an action runs again when its availability moves"
            );
            view.poll_patches().unwrap();
            let menu = one(&view, "ui.MenuItem");
            assert!(
                !menu.props.contains_key("disabled"),
                "the row runs again: {}",
                dump(view.tree())
            );
            assert!(!menu.props.contains_key("description"), "and says no reason");
            let button = one(&view, "ui.Button");
            assert!(
                !button.props.contains_key("enabled"),
                "the button can be pressed again: {}",
                dump(view.tree())
            );
        }
    }
}
