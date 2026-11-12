//! Typed inputs of components: a map field handed whole, and the actions the host fills a menu
//! with, run by the events of the nodes it makes.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, Runtime, SimpleAuthority, Value};

    const SOURCE: &str = r#"app so.tilo.talk "Talk"

type Message {
    text: Text
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
    ui.MenuButton("More", subject: m)
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

        let menu = of_kind(view.tree(), "ui.MenuButton")[0];
        let pin = of_kind(menu, "ui.MenuItem")
            .into_iter()
            .find(|item| item.props.get("label") == Some(&Value::text("Pin")))
            .unwrap_or_else(|| panic!("pin is offered: {}", dump(view.tree())));
        assert_eq!(pin.props.get("icon"), Some(&Value::text("pin")));
        assert!(
            pin.events.contains_key("choose"),
            "the node runs it by its event: {pin:?}"
        );
        view.fire(&pin.key, "choose").unwrap();
        view.poll_patches().unwrap();
        assert_eq!(
            rt.read(&m, "pinned").unwrap(),
            Value::Bool(true),
            "choosing the offered action ran it"
        );
    }

    const REACTED: &str = r#"app so.tilo.talk "Talk"

type Post {
    text: Text
    reactions: Map<Set<Principal>>
}

action add(p: Post, emoji: Text) {
    p.reactions.add(emoji, invoker)
}

action take(p: Post, emoji: Text) {
    p.reactions.remove(emoji, invoker)
}

view post(p: Post) {
    ui.Reactions(reactions: p.reactions)
}
"#;

    #[test]
    fn a_map_of_sets_holds_each_element_and_reads_as_sets_by_key() {
        let app = App::compile(REACTED, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let p = rt.create("Post").unwrap();
        let ben = tilolang_runtime::Principal::new("ben");
        for (who, action, emoji) in [
            (me(), "add", "👍"),
            (ben.clone(), "add", "👍"),
            (ben.clone(), "add", "🎉"),
            (ben.clone(), "take", "🎉"),
        ] {
            rt.invoke_action(
                &who,
                (
                    app.action(action).as_str(),
                    vec![Value::resource(&p), Value::text(emoji)],
                ),
            )
            .unwrap();
        }
        let view = rt
            .open_view(&me(), &app.view("post"), vec![Value::resource(&p)])
            .unwrap();
        let node = of_kind(view.tree(), "ui.Reactions")[0].clone();
        let Some(Value::Map(entries)) = node.props.get("reactions") else {
            panic!("a map: {}", dump(view.tree()));
        };
        let mut who = vec![Value::Principal(me()), Value::Principal(ben)];
        who.sort_by_key(|p| format!("{p:?}"));
        assert_eq!(
            entries,
            &vec![(Value::text("👍"), Value::List(who))],
            "one emoji, both who reacted; the taken-back one is gone"
        );
    }
}
