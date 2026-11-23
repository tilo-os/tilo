//! The standard library's functions, as a package calls them: its views travel to each package
//! as templates it expands, and its `fn`s are code the runtime runs beside the caller. A call of
//! a function no package loaded here declares fails the run, naming it, rather than answering
//! nothing.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, SimpleAuthority, Value};

    /// An app that says what native reactions were, and what an emoji key names, with the
    /// library's own functions.
    const SAID: &str = "app so.tilo.said \"Said\"

type Word {
    said: Text
    tag: Text
}

root Word

default view card for Word

action say(w: Word, keys: List<Text>, key: Text) {
    w.said = std.reactions_said(keys)
    w.tag = std.emoji_of(key)
}

view card(w: Word) {
    text(w.said)
}
";

    /// A package whose `fn` a caller elsewhere uses, and one that uses it.
    const LIB: &str = "app so.tilo.lib \"Lib\"

fn shout(s: Text) -> Text {
    \"{s}!\"
}
";

    const CALLER: &str = "app so.tilo.caller \"Caller\"

use so.tilo.lib as lib

action greet() -> Text {
    lib.shout(\"hi\")
}
";

    fn runtime() -> Runtime {
        Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap()
    }

    fn res(r: &ResourceRef) -> Value {
        Value::resource(r)
    }

    /// The key a native app writes for a reaction: the emoji, then who reacted in hex (`616461`
    /// is ada's id and `626f62` bob's, as the native app wrote them).
    fn reaction(emoji: &str, who: &str) -> Value {
        Value::text(format!("{emoji}\u{1f}\u{1f}{who}"))
    }

    #[test]
    fn a_package_calls_a_library_fn_and_gets_its_value() {
        let app = App::compile(SAID, &[]);
        let rt = runtime();
        app.load(&rt);
        let w = rt.create_root("Word").unwrap();
        let keys = Value::List(vec![
            reaction("🎉", "616461"),
            reaction("🎉", "626f62"),
            reaction("👍", "616461"),
        ]);
        rt.invoke_action(
            &me(),
            (
                app.action("say"),
                vec![res(&w), keys, reaction("🎉", "616461")],
            ),
        )
        .unwrap();
        assert_eq!(
            rt.read(&w, "said").unwrap(),
            Value::text("🎉 2  👍 1"),
            "the library's fn runs and answers its value"
        );
        assert_eq!(
            rt.read(&w, "tag").unwrap(),
            Value::text("🎉"),
            "and the key's emoji reads as the emoji it names"
        );
    }

    #[test]
    fn a_call_of_a_function_no_package_here_declares_fails_the_run() {
        let lib = App::compile(LIB, &[]);
        let caller = App::compile(CALLER, &[lib.meta().clone()]);
        let rt = runtime();
        // The caller loads, and the package whose `fn` it calls does not: the call has nothing
        // to run.
        caller.load(&rt);
        let failed = rt
            .invoke_action(&me(), (caller.action("greet"), vec![]))
            .unwrap_err();
        let said = failed.to_string();
        assert!(
            said.contains("declares the function") && said.contains(&lib.fn_("shout")),
            "the run fails, naming the function it could not find: {said}"
        );
    }

    #[test]
    fn a_view_that_calls_a_library_fn_draws_its_value() {
        let app = App::compile(SAID, &[]);
        let rt = runtime();
        app.load(&rt);
        let w = rt.create_root("Word").unwrap();
        rt.invoke_action(
            &me(),
            (
                app.action("say"),
                vec![
                    res(&w),
                    Value::List(vec![reaction("🎉", "616461")]),
                    reaction("🎉", "616461"),
                ],
            ),
        )
        .unwrap();
        let view = rt
            .open_view(&me(), &app.view("card"), vec![res(&w)])
            .unwrap();
        assert_eq!(textual(view.tree()), "🎉 1");
    }

    /// The text of a view's one text node.
    fn textual(node: &tilolang_runtime::UiNode) -> String {
        let texts = of_kind(node, "ui.Text");
        assert_eq!(texts.len(), 1, "{}", dump(node));
        text_prop(texts[0], "value")
    }
}
