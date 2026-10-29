//! Typed inputs of components: a map field handed whole, and the actions offered on a
//! resource, each shown by the component and run by an event of its node.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{AmStore, Runtime, SimpleAuthority, Value};

    const SOURCE: &str = r#"app so.tilo.talk "Talk"

type Message {
    text: Text (title)
    pinned: Bool = false
    reactions: Map<List<Principal>>
}

action react(m: Message, emoji: Text) {
    m.reactions.put(emoji, [invoker])
}

action pin(m: Message) (label: "Pin", icon: .pin) {
    m.pinned = true
}

view message(m: Message) {
    ui.Reactions(reactions: m.reactions)
    ui.MenuButton("More", actions: actions(m))
}
"#;

    #[test]
    fn a_map_goes_whole_and_offered_actions_run_by_their_nodes_events() {
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let m = rt.create("Message").unwrap();
        rt.invoke_action(
            &me(),
            (
                app.action("react").as_str(),
                vec![Value::resource(&m), Value::text("👍")],
            ),
        )
        .unwrap();
        let mut view = rt
            .open_view(&me(), &app.view("message"), vec![Value::resource(&m)])
            .unwrap();
        let reactions = of_kind(view.tree(), "ui.Reactions")[0].clone();
        let Some(Value::Map(entries)) = reactions.props.get("reactions") else {
            panic!("a map: {}", dump(view.tree()));
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, Value::text("👍"));
        assert!(
            matches!(&entries[0].1, Value::List(who) if who == &vec![Value::Principal(me())]),
            "{entries:?}"
        );

        let menu = of_kind(view.tree(), "ui.MenuButton")[0].clone();
        let Some(Value::List(offered)) = menu.props.get("actions") else {
            panic!("offered actions: {}", dump(view.tree()));
        };
        let pin = offered
            .iter()
            .find_map(|a| match a {
                Value::Offered(o) if o.label == "Pin" => Some(o.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("pin is offered: {offered:?}"));
        assert_eq!(pin.icon.as_deref(), Some("pin"));
        assert!(
            menu.events.contains_key(&pin.event),
            "the node runs it by its event {}",
            pin.event
        );
        view.fire(&menu.key, &pin.event).unwrap();
        view.poll_patches().unwrap();
        assert_eq!(
            rt.read(&m, "pinned").unwrap(),
            Value::Bool(true),
            "choosing the offered action ran it"
        );
    }
}
