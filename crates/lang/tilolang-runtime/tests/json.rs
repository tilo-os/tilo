//! `json.decode<T>`, `json.encode` and `json.parse`: text read into the type the code names and
//! errors as values naming the path that did not fit, values written back as text, and a
//! document whose shape code asks as it goes.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::rc::Rc;

    use serde_json::json;

    use crate::common::*;
    use tilolang_runtime::run::WebRequest;
    use tilolang_runtime::web::{WebReply, WebTransport};
    use tilolang_runtime::{
        AmStore, EnumValue, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, Value,
    };

    const SOURCE: &str = r#"app so.tilo.jsoncase "JSON case" (web: ["example.com"])

enum State {
    open
    closed
    tagged
}

type Item {
    title: Text
    stars: Int
    score: Float
    state: State
    note: Option<Text>
}

type Wrapped {
    inner: Item
    label: Text
}

type Feed {
    items: Collection<Item>
    status: Text
    summary: Text
    payload: Text
    count: Int
}

action refresh(f: Feed) uses web {
    let r = web.get("https://example.com/feed")
    match json.decode<List<Item>>(r.text()) {
        .ok(items) => {
            for item in items {
                f.items.insert(item)
            }
            f.count = items.len()
            f.status = "read"
        }
        .err(why) => { f.status = why }
    }
}

action decode_one(f: Feed) uses web {
    let r = web.get("https://example.com/feed")
    match json.decode<Item>(r.text()) {
        .ok(item) => { f.status = "read {item.title}" }
        .err(why) => { f.status = why }
    }
}

action wrap(f: Feed) uses web {
    let r = web.get("https://example.com/feed")
    match json.decode<Wrapped>(r.text()) {
        .ok(w) => { f.status = "read {w.label}" }
        .err(why) => { f.status = why }
    }
}

action encode_items(f: Feed) uses web {
    let r = web.get("https://example.com/feed")
    match json.decode<List<Item>>(r.text()) {
        .ok(items) => { f.payload = json.encode(items) }
        .err(why) => { f.status = why }
    }
}

action peek(f: Feed) uses web {
    let r = web.get("https://example.com/feed")
    match json.parse(r.text()) {
        .ok(v) => {
            f.status = "parsed"
            match v {
                .object(fields) => {
                    match fields.get("title") {
                        .some(held) => { f.summary = json.encode(held) }
                        .none => { f.summary = "none" }
                    }
                }
                .array(items) => { f.summary = "{items.len()}" }
                .string(text) => { f.summary = text }
                .number(number) => { f.summary = number.to_text() }
                .bool(flag) => { f.summary = if flag { "yes" } else { "no" } }
                .null => { f.summary = "nothing" }
            }
        }
        .err(why) => { f.status = why }
    }
}

action explore(f: Feed) uses web {
    let r = web.get("https://example.com/feed")
    match json.parse(r.text()) {
        .ok(v) => {
            f.status = "parsed"
            match v {
                .object(fields) => { f.count = fields.keys().len() }
                .array(items) => { f.count = items.len() }
                .string(text) => { f.summary = text }
                .number(number) => {
                    f.count = number.to_int()
                    f.summary = number.to_text()
                }
                .bool(flag) => { f.count = if flag { 1 } else { 0 } }
                .null => { f.count = 0 - 1 }
            }
        }
        .err(why) => { f.status = why }
    }
}
"#;

    /// What the invoking device's network answers.
    #[derive(Clone, Default)]
    struct FakeWeb {
        body: Rc<RefCell<String>>,
    }

    impl FakeWeb {
        fn serving(body: &str) -> Self {
            Self {
                body: Rc::new(RefCell::new(body.to_owned())),
            }
        }
    }

    impl WebTransport for FakeWeb {
        fn fetch(&self, _request: &WebRequest) -> WebReply {
            WebReply {
                status: 200,
                status_text: "OK".to_owned(),
                headers: vec!["Content-Type: application/json".to_owned()],
                body: self.body.borrow().clone().into_bytes(),
            }
        }
    }

    struct World {
        rt: Runtime,
        app: App,
        feed: ResourceRef,
    }

    fn world(body: &str) -> World {
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        rt.set_web(FakeWeb::serving(body));
        app.load(&rt);
        let feed = rt.create("Feed").unwrap();
        World { rt, app, feed }
    }

    impl World {
        fn call(&self, action: &str) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), vec![Value::resource(&self.feed)]))
                .unwrap()
        }

        fn text(&self, field: &str) -> String {
            self.rt
                .read(&self.feed, field)
                .unwrap()
                .as_text()
                .map_or_else(|| panic!("{field} is not text"), str::to_owned)
        }

        fn int(&self, field: &str) -> i64 {
            match self.rt.read(&self.feed, field).unwrap() {
                Value::Int(i) => i,
                other => panic!("{field} is {other:?}"),
            }
        }

        /// The feed's items, as `(title, stars, score, state)`.
        fn items(&self) -> Vec<(String, i64, f64, String)> {
            let Value::List(items) = self.rt.read(&self.feed, "items").unwrap() else {
                panic!("items is not a list");
            };
            items
                .into_iter()
                .map(|i| {
                    let Value::Ref(r) = i else {
                        panic!("not a ref")
                    };
                    let Value::Enum(EnumValue { variant, .. }) =
                        self.rt.read(&r.target, "state").unwrap()
                    else {
                        panic!("state is not an enum")
                    };
                    (
                        self.rt
                            .read(&r.target, "title")
                            .unwrap()
                            .as_text()
                            .unwrap()
                            .to_owned(),
                        match self.rt.read(&r.target, "stars").unwrap() {
                            Value::Int(i) => i,
                            other => panic!("stars is {other:?}"),
                        },
                        match self.rt.read(&r.target, "score").unwrap() {
                            Value::Float(f) => f,
                            other => panic!("score is {other:?}"),
                        },
                        variant,
                    )
                })
                .collect()
        }

        fn note(&self, at: usize) -> Value {
            let Value::List(items) = self.rt.read(&self.feed, "items").unwrap() else {
                panic!("items is not a list");
            };
            let Value::Ref(r) = &items[at] else {
                panic!("not a ref")
            };
            self.rt.read(&r.target, "note").unwrap()
        }
    }

    /// Three items: text with unicode, an Int, a Float, each of the enum's cases, an option
    /// there and one left out, and a field the type does not declare.
    const FEED: &str = r#"[
      {"title": "Zoë — 東京 ☕", "stars": 5, "score": 4.5, "state": "open", "note": "héllo", "extra": 1},
      {"title": "second", "stars": 0, "score": 3, "state": "tagged"},
      {"title": "third", "stars": 12, "score": 1.25, "state": "closed", "note": null}
    ]"#;

    fn id_of(app: &App, name: &str) -> String {
        app.compiled
            .ids
            .get(name)
            .unwrap_or_else(|| panic!("no id {name}"))
            .to_string()
    }

    #[test]
    fn a_feed_reads_into_records_of_the_type_the_code_names() {
        let w = world(FEED);
        w.call("refresh");

        assert_eq!(w.text("status"), "read");
        assert_eq!(w.int("count"), 3);
        let items = w.items();
        let titles: Vec<&str> = items.iter().map(|i| i.0.as_str()).collect();
        assert_eq!(
            titles,
            ["Zoë — 東京 ☕", "second", "third"],
            "text keeps its characters"
        );
        assert_eq!(
            items.iter().map(|i| i.1).collect::<Vec<_>>(),
            [5, 0, 12],
            "a whole number reads as an Int"
        );
        assert_eq!(
            items.iter().map(|i| i.2).collect::<Vec<_>>(),
            [4.5, 3.0, 1.25],
            "an Int in the text reads as the Float it is"
        );
        let states: Vec<&str> = items.iter().map(|i| i.3.as_str()).collect();
        assert_eq!(
            states,
            [
                id_of(&w.app, "State.open"),
                id_of(&w.app, "State.tagged"),
                id_of(&w.app, "State.closed"),
            ],
            "a case reads from its name, and the value holds the declared case"
        );
        assert_eq!(w.note(0), Value::some(Value::text("héllo")));
        assert_eq!(
            w.note(1),
            Value::none(),
            "a field the text leaves out is none"
        );
        assert_eq!(w.note(2), Value::none(), "null is none too");
        assert_eq!(
            w.text("status"),
            "read",
            "a field the type does not declare (`extra`) breaks nothing"
        );
    }

    #[test]
    fn a_value_that_does_not_fit_is_an_error_naming_its_path() {
        let w = world(r#"[{"title": "a", "stars": "no", "score": 1, "state": "open"}]"#);
        w.call("refresh");
        let why = w.text("status");
        assert!(why.contains("$[0].stars"), "names the path: {why}");
        assert!(why.contains("whole number"), "names what fitted: {why}");

        let w = world(r#"[{"title": "a", "score": 1, "state": "open"}]"#);
        w.call("refresh");
        let why = w.text("status");
        assert!(
            why.contains("$[0].stars is missing"),
            "names the missing field: {why}"
        );
        assert!(
            why.contains("title, stars, score, state, note"),
            "says what the type has: {why}"
        );

        let w = world(r#"[{"title": "a", "stars": 1, "score": 1, "state": "nope"}]"#);
        w.call("refresh");
        let why = w.text("status");
        assert!(why.contains("$[0].state is no case of `State`"), "{why}");
        assert!(
            why.contains("open, closed, tagged"),
            "lists the cases: {why}"
        );

        let w = world(r"not json at all");
        w.call("refresh");
        assert!(
            w.text("status").starts_with("not JSON"),
            "{}",
            w.text("status")
        );

        let mut w = world(r#"{"title": "a", "stars": 1, "score": 1, "state": "open"}"#);
        w.call("decode_one");
        assert_eq!(w.text("status"), "read a");

        // A record field holds a resource, which text cannot make: the error says where.
        w = world(r#"{"inner": {"title": "a"}, "label": "x"}"#);
        w.call("wrap");
        let why = w.text("status");
        assert!(why.contains("$.inner"), "names the path: {why}");
        assert!(why.contains("holds resources"), "{why}");
    }

    #[test]
    fn a_value_written_as_text_is_what_the_type_declares() {
        let w = world(FEED);
        w.call("encode_items");
        let written: serde_json::Value =
            serde_json::from_str(&w.text("payload")).expect("encode writes JSON");
        assert_eq!(
            written,
            json!([
                {"title": "Zoë — 東京 ☕", "stars": 5, "score": 4.5, "state": "open", "note": "héllo"},
                {"title": "second", "stars": 0, "score": 3.0, "state": "tagged", "note": null},
                {"title": "third", "stars": 12, "score": 1.25, "state": "closed", "note": null}
            ]),
            "fields by their names, enums by their cases, none as null"
        );

        // What was read reads back: text written from a value reads as that value, and writes
        // back as the same text.
        let first_written = w.text("payload");
        let w = world(&first_written);
        w.call("encode_items");
        assert_eq!(
            w.text("payload"),
            first_written,
            "a round trip keeps every field"
        );
    }

    #[test]
    fn a_document_whose_shape_is_unknown_reads_case_by_case() {
        let cases = [
            (r#"{"a": 1, "b": 2, "c": 3}"#, 3, ""),
            (r"[1, 2]", 2, ""),
            (r"1.0", 1, "1"),
            (r"7", 7, "7"),
            (r"true", 1, ""),
            (r"false", 0, ""),
            (r"null", -1, ""),
            (r"1.5", 1, "1.5"),
            ("\"hello\"", 0, "hello"),
        ];
        for (document, count, summary) in cases {
            let w = world(document);
            w.call("explore");
            assert_eq!(w.text("status"), "parsed", "{document}");
            assert_eq!(w.int("count"), count, "{document}");
            assert_eq!(w.text("summary"), summary, "{document}");
        }

        // A value under a key the code asks for by name: read back as the text it was.
        let w = world(r#"{"title": "Zoë ☕", "n": 2}"#);
        w.call("peek");
        assert_eq!(w.text("status"), "parsed");
        assert_eq!(w.text("summary"), "\"Zoë ☕\"");
        let w = world(r#"{"other": 1}"#);
        w.call("peek");
        assert_eq!(w.text("summary"), "none", "a key the object lacks is none");

        let w = world("{oops");
        w.call("explore");
        assert!(
            w.text("status").starts_with("not JSON"),
            "{}",
            w.text("status")
        );
    }
}
