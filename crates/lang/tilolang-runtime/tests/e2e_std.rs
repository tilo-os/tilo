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

    /// A map a function fills while it runs, holding numbers: a scalar is boxed to be handed
    /// over, and `get` answers it in an option of its own.
    const TALLY: &str = "app so.tilo.tally \"Tally\"

type Tally {
    seen: Float
    missing: Float
    count: Int
    absent: Int
    order: Text
    kept: Map<Int>
    kept_count: Int
    kept_absent: Int
}

root Tally

default view card for Tally

action tally(t: Tally) {
    let scores = Map<Float>()
    scores.put(\"a\", 2.5)
    scores.put(\"b\", 1.5)
    t.seen = scores.get(\"a\") ?? 0.0
    t.missing = scores.get(\"z\") ?? -1.0
    let counts = Map<Int>()
    bump(counts, \"a\", 2)
    bump(counts, \"b\", 5)
    bump(counts, \"a\", 1)
    t.count = counts.get(\"a\") ?? 0
    t.absent = counts.get(\"z\") ?? -1
    t.order = counts.keys().sort_by(k -> counts.get(k) ?? 0, desc).join(\",\")
}

action keep(t: Tally) {
    t.kept.put(\"a\", 4)
    t.kept.put(\"a\", (t.kept.get(\"a\") ?? 0) + 3)
}

action read_kept(t: Tally) {
    t.kept_count = t.kept.get(\"a\") ?? 0
    t.kept_absent = t.kept.get(\"z\") ?? -1
}

fn bump(counts: Map<Int>, key: Text, by: Int) -> () {
    counts.put(key, (counts.get(key) ?? 0) + by)
}

view card(t: Tally) {
    text(\"{t.seen}\")
}

view ranked(t: Tally) {
    text(\"{ranking().join(\",\")} {t.kept.get(\"a\") ?? 0} {t.kept.get(\"z\") ?? 5}\")
}

fn ranking() -> List<Text> {
    let counts = Map<Int>()
    let latest = Map<Text>()
    bump(counts, \"a\", 1)
    bump(counts, \"b\", 4)
    bump(counts, \"a\", 1)
    bump(counts, \"c\", 0)
    counts
        .keys()
        .sort_by(k -> k)
        .sort_by(k -> latest.get(k) ?? \"\", desc)
        .sort_by(k -> counts.get(k) ?? 0, desc)
        .map(k -> \"{k}{counts.get(k) ?? -1}\")
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
    fn a_number_put_in_a_map_the_code_holds_reads_back_as_that_number() {
        let app = App::compile(TALLY, &[]);
        let rt = runtime();
        app.load(&rt);
        let t = rt.create_root("Tally").unwrap();
        rt.invoke_action(&me(), (app.action("tally"), vec![res(&t)]))
            .unwrap();
        assert_eq!(
            rt.read(&t, "seen").unwrap(),
            Value::Float(2.5),
            "a number put in and read back is the number, not a box around it"
        );
        assert_eq!(
            rt.read(&t, "missing").unwrap(),
            Value::Float(-1.0),
            "a key the map does not hold answers the fallback"
        );
        assert_eq!(
            rt.read(&t, "count").unwrap(),
            Value::Int(3),
            "a whole number counted up through a fn reads back as the number"
        );
        assert_eq!(
            rt.read(&t, "absent").unwrap(),
            Value::Int(-1),
            "a key a map of whole numbers does not hold answers the fallback"
        );
        assert_eq!(
            rt.read(&t, "order").unwrap(),
            Value::text("b,a"),
            "a map's whole numbers order its keys"
        );
        rt.invoke_action(&me(), (app.action("keep"), vec![res(&t)]))
            .unwrap();
        rt.invoke_action(&me(), (app.action("read_kept"), vec![res(&t)]))
            .unwrap();
        assert_eq!(
            rt.read(&t, "kept_count").unwrap(),
            Value::Int(7),
            "a whole number a record's map keeps reads back as the number"
        );
        assert_eq!(
            rt.read(&t, "kept_absent").unwrap(),
            Value::Int(-1),
            "a key a record's map does not keep answers the fallback"
        );
    }

    #[test]
    fn a_view_ranks_by_the_whole_numbers_a_map_its_fn_fills_holds() {
        let app = App::compile(TALLY, &[]);
        let rt = runtime();
        app.load(&rt);
        let t = rt.create_root("Tally").unwrap();
        rt.invoke_action(&me(), (app.action("keep"), vec![res(&t)]))
            .unwrap();
        let view = rt
            .open_view(&me(), &app.view("ranked"), vec![res(&t)])
            .unwrap();
        assert_eq!(textual(view.tree()), "b4,a2,c0 7 5");
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

    /// A page whose header is given as an argument (`std.page(x, header: { ... })`) stands in
    /// the window's bar: the call reads that view as the page's part, never drops it.
    #[test]
    fn a_pages_header_given_as_an_argument_stands_in_its_bar() {
        const HEADED: &str = "app so.tilo.headed \"Headed\"

type Notes {
    title: Text
}

root Notes

default view all for Notes

view all(n: Notes) {
    std.page(n, header: { ui.Text(\"Header words\") }) {
        ui.Text(\"Body words\")
    }
}
";
        let app = App::compile(HEADED, &[]);
        let rt = runtime();
        app.load(&rt);
        let n = rt.create_root("Notes").unwrap();
        let view = rt
            .open_view(&me(), &app.view("all"), vec![res(&n)])
            .unwrap();
        let tree = view.tree();
        let shell = of_kind(tree, "ui.NavShell")
            .into_iter()
            .next()
            .expect("the page's window shell");
        let bar = shell
            .children
            .iter()
            .find(|child| {
                child.kind == "part" && child.props.get("name") == Some(&Value::text("actions"))
            })
            .unwrap_or_else(|| panic!("the window has a bar for the page's header:\n{}", dump(tree)));
        assert!(
            of_kind(bar, "ui.Text")
                .into_iter()
                .any(|text| text_prop(text, "value") == "Header words"),
            "the page's header stands in the window's bar:\n{}",
            dump(tree)
        );
    }

    /// The text of a view's one text node.
    fn textual(node: &tilolang_runtime::UiNode) -> String {
        let texts = of_kind(node, "ui.Text");
        assert_eq!(texts.len(), 1, "{}", dump(node));
        text_prop(texts[0], "value")
    }
}
