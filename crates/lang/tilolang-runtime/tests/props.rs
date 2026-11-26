//! Typed inputs of components: a map field handed whole, and the actions the host fills a menu
//! with, run by the events of the nodes it makes.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, EngineKind, Runtime, Value};

    const SOURCE: &str = r#"app so.tilo.talk "Talk"

type Message {
    text: Text
    pinned: Bool = false
    reactions: Map<List<Actor>>
}

action react(m: Message, emoji: Text) {
    m.reactions.put(emoji, [invoker])
}

action pin(m: Message) (label: "Pin", icon: .pin) {
    m.pinned = true
}

view message(m: Message) {
    ui.Reactions(reactions: m.reactions)
    ui.MenuButton("More", target: m)
}
"#;

    tilo_test_env::scenario! {
        fn maps_and_sets_reach_a_view_whole;

        phase a_map_goes_whole_and_offered_actions_run_by_their_nodes_events;
        {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
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
                matches!(&entries[0].1, Value::List(who) if who == &vec![Value::Actor(me())]),
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

        phase a_map_of_sets_holds_each_element_and_reads_as_sets_by_key;
        {
            let app = App::compile(REACTED, &[]);
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            app.load(&rt);
            let p = rt.create("Post").unwrap();
            let ben = person("ben");
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
            let mut who = vec![Value::Actor(me()), Value::Actor(ben)];
            who.sort_by_key(|p| format!("{p:?}"));
            assert_eq!(
                entries,
                &vec![(Value::text("👍"), Value::List(who))],
                "one emoji, both who reacted; the taken-back one is gone"
            );
        }
    }

    const REACTED: &str = r#"app so.tilo.talk "Talk"

type Post {
    text: Text
    reactions: Map<Set<Actor>>
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

    const FLAGS: &str = r#"app so.tilo.flags "Flags"

type Board {
    zoom: Int
}

action set_zoom(b: Board, zoom: Int) {
    b.zoom = zoom
}

view small_row(title: Text, smaller: Bool) {
    ui.ListRow(title: title, dense: smaller)
}

view board(b: Board) {
    small_row("Row", b.zoom == 2)
    ui.Button("Toggled", toggled: b.zoom == 2)
    ui.Text("Lines", lines: 3)
}
"#;

    /// A flag reads as a flag on every tier: a comparison's answer, and a `Bool` parameter's value
    /// handed on, reach a prop as `true` or `false`, never as the 1 the guest computes it as.
    #[test]
    fn a_flag_prop_reads_as_a_flag_on_every_tier() {
        // The tiered process checks both legs; a process pinned to one engine through
        // `TILOLANG_ENGINE` runs its own, and takes the compiled artifact only where it can.
        let engine = Runtime::new(AmStore::new(), TestAuthority::new())
            .unwrap()
            .engine()
            .kind();
        let legs: &[(bool, EngineKind)] = match engine {
            EngineKind::Tiered => &[(false, EngineKind::Wasmi), (true, EngineKind::Cranelift)],
            EngineKind::Wasmi => &[(false, EngineKind::Wasmi)],
            EngineKind::Cranelift => &[(true, EngineKind::Cranelift)],
            EngineKind::Winch => &[(false, EngineKind::Winch)],
        };
        for &(compiled, tier) in legs {
            // Each leg loads a module of its own, so the adopted artifact of the compiled leg
            // is not the interpreter's module already loaded under the same digest.
            let src = FLAGS.replacen(
                "so.tilo.flags",
                if compiled {
                    "so.tilo.flags.compiled"
                } else {
                    "so.tilo.flags"
                },
                1,
            );
            let app = App::compile(&src, &[]);
            if compiled {
                crate::common::adopt_compiled(&app.compiled.wasm);
            }
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            app.load(&rt);
            let hash = Runtime::module_hash(&app.compiled.wasm);
            assert_eq!(
                rt.engine().tier(&hash),
                Some(tier),
                "the module runs on the {tier:?} tier"
            );

            let b = rt.create("Board").unwrap();
            let zoom = |zoom: i64| {
                rt.invoke_action(
                    &me(),
                    (
                        app.action("set_zoom").as_str(),
                        vec![Value::resource(&b), Value::Int(zoom)],
                    ),
                )
                .unwrap();
            };
            let flags = || {
                let view = rt
                    .open_view(&me(), &app.view("board"), vec![Value::resource(&b)])
                    .unwrap();
                let tree = view.tree();
                let dense = of_kind(tree, "ui.ListRow")[0].props.get("dense").cloned();
                let toggled = of_kind(tree, "ui.Button")
                    .into_iter()
                    .find(|b| b.props.get("label") == Some(&Value::text("Toggled")))
                    .unwrap_or_else(|| panic!("the toggled button: {}", dump(tree)))
                    .props
                    .get("toggled")
                    .cloned();
                // A number still crosses as a number: the flag's own path did not take
                // the integer props with it.
                let lines = of_kind(tree, "ui.Text")
                    .into_iter()
                    .find(|t| t.props.get("value") == Some(&Value::text("Lines")))
                    .unwrap_or_else(|| panic!("the numbered text: {}", dump(tree)))
                    .props
                    .get("lines")
                    .cloned();
                (dense, toggled, lines)
            };
            zoom(2);
            assert_eq!(
                flags(),
                (
                    Some(Value::Bool(true)),
                    Some(Value::Bool(true)),
                    Some(Value::Int(3))
                ),
                "both flags are true, and the number is a number, on {tier:?}"
            );
            zoom(1);
            assert_eq!(
                flags(),
                (
                    Some(Value::Bool(false)),
                    Some(Value::Bool(false)),
                    Some(Value::Int(3))
                ),
                "both flags are false, and the number is a number, on {tier:?}"
            );
        }
    }
}
