//! `invoker.app()`: the app whose process a call came through, as the daemon attested it. An
//! action admits one app's process by it and refuses another app's, and a person's own call.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, Runtime, Value};

    const SOURCE: &str = r#"app so.tilo.bridge "Bridge"

type Bridge {
    relayed: Collection<Relayed>
}

type Relayed {
    text: Text
    through: Text
}

/// What only the bridge's own program may do: say what arrived on the other side.
fn the_bridge(who: Actor) -> Bool {
    (who.app() ?? "") == "so.tilo.discord"
}

action relay(b: Bridge, text: Text) {
    require the_bridge(invoker) "Only the Discord bridge relays"
    b.relayed.insert(Relayed(text: text, through: invoker.app() ?? ""))
}
"#;

    #[test]
    fn an_action_admits_the_attested_app_and_refuses_another_and_a_person() {
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
        app.load(&rt);
        let bridge = rt.create("Bridge").unwrap();
        let relay = app.action("relay");
        let call = |through: Option<&str>, text: &str| {
            rt.invoke_admitted_through(
                &person("ada"),
                through,
                (
                    relay.as_str(),
                    vec![Value::resource(&bridge), Value::text(text)],
                ),
                None,
            )
        };

        call(Some("so.tilo.discord"), "hello").expect("the Discord bridge's process is admitted");
        for (through, who) in [(Some("so.tilo.other"), "another app"), (None, "a person")] {
            let refused = call(through, "sneaky").expect_err(who);
            assert!(
                refused
                    .to_string()
                    .contains("Only the Discord bridge relays"),
                "{who} is refused with the action's reason: {refused}"
            );
        }

        let relayed = format!("{:?}", rt.read(&bridge, "relayed").unwrap());
        assert!(
            !relayed.contains("sneaky"),
            "a refused call changed nothing: {relayed}"
        );
    }
}
