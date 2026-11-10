//! The text and number surface, run: a text counts, cuts and cases in graphemes, numbers clamp
//! and round, and a list of text or numbers joins, sums and averages.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{AmStore, InvokeOutcome, Runtime, Value};

    const SOURCE: &str = "app so.tilo.text \"Text\"\n\
action count(t: Text) -> Int { return t.len() }\n\
action words(t: Text) -> Int { return t.words() }\n\
action empty(t: Text) -> Bool { return t.is_empty() }\n\
action tidy(t: Text) -> Text { return t.trim() }\n\
action shout(t: Text) -> Text { return t.upper() }\n\
action whisper(t: Text) -> Text { return t.lower() }\n\
action starts(t: Text, p: Text) -> Bool { return t.starts_with(p) }\n\
action ends(t: Text, p: Text) -> Bool { return t.ends_with(p) }\n\
action holds(t: Text, p: Text) -> Bool { return t.contains(p) }\n\
action glob(t: Text, p: Text) -> Bool { return t.matches(p) }\n\
action at(t: Text, p: Text) -> Int { return match t.find(p) { .some(i) => i, .none => -1 } }\n\
action cut(t: Text, a: Int, b: Int) -> Text { return t.slice(a, b) }\n\
action swap(t: Text, a: Text, b: Text) -> Text { return t.replace(a, b) }\n\
action pieces(t: Text, sep: Text) -> List<Text> { return t.split(sep) }\n\
action rejoined(t: Text, sep: Text) -> Text { return t.split(sep).join(\"|\") }\n\
action tied(xs: List<Text>, sep: Text) -> Text { return xs.join(sep) }\n\
action number(t: Text) -> Int { return match t.to_int() { .some(n) => n, .none => -1 } }\n\
action lowest(a: Int, b: Int) -> Int { return a.min(b) }\n\
action highest(a: Int, b: Int) -> Int { return a.max(b) }\n\
action bounded(n: Int) -> Int { return n.clamp(0, 10) }\n\
action size(n: Int) -> Int { return n.abs() }\n\
action as_float(n: Int) -> Float { return n.to_float() }\n\
action int_text(n: Int) -> Text { return n.to_text() }\n\
action least(a: Float, b: Float) -> Float { return a.min(b) }\n\
action fhighest(a: Float, b: Float) -> Float { return a.max(b) }\n\
action fbounded(f: Float) -> Float { return f.clamp(0.0, 1.0) }\n\
action fsize(f: Float) -> Float { return f.abs() }\n\
action whole(f: Float) -> Int { return f.round() }\n\
action down(f: Float) -> Int { return f.floor() }\n\
action up(f: Float) -> Int { return f.ceil() }\n\
action whole_to(f: Float, places: Int) -> Float { return f.round(places) }\n\
action down_to(f: Float, places: Int) -> Float { return f.floor(places) }\n\
action up_to(f: Float, places: Int) -> Float { return f.ceil(places) }\n\
action as_int(f: Float) -> Int { return f.to_int() }\n\
action float_text(f: Float) -> Text { return f.to_text() }\n\
action total(xs: List<Int>) -> Int { return xs.sum() }\n\
action total_f(xs: List<Float>) -> Float { return xs.sum() }\n\
action mean(xs: List<Int>) -> Float { return xs.avg() }\n\
action among(xs: List<Text>, x: Text) -> Bool { return xs.contains(x) }\n\
action has_num(xs: List<Int>, n: Int) -> Bool { return xs.contains(n) }\n\
action has_real(xs: List<Float>, f: Float) -> Bool { return xs.contains(f) }\n\
action where_num(xs: List<Int>, n: Int) -> Int { return match xs.index_of(n) { .some(i) => i, .none => -1 } }\n\
action flag(b: Bool) -> Text { return b.to_text() }";

    struct Surface {
        rt: Runtime,
        app: App,
    }

    fn surface() -> Surface {
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
        app.load(&rt);
        Surface { rt, app }
    }

    impl Surface {
        fn ask(&self, action: &str, args: Vec<Value>) -> Value {
            let aref = (self.app.action(action), args);
            match self.rt.invoke_action(&me(), aref).unwrap() {
                InvokeOutcome::Done(v) => v,
                InvokeOutcome::Suggested(s) => panic!("{action} stopped to ask {}", s.action),
                InvokeOutcome::Denied => panic!("{action} was denied"),
            }
        }

        fn text(&self, action: &str, args: Vec<Value>) -> String {
            match self.ask(action, args) {
                Value::Text(t) => t,
                other => panic!("{action} gave {other:?}"),
            }
        }

        fn int(&self, action: &str, args: Vec<Value>) -> i64 {
            match self.ask(action, args) {
                Value::Int(n) => n,
                other => panic!("{action} gave {other:?}"),
            }
        }

        fn yes(&self, action: &str, args: Vec<Value>) -> bool {
            match self.ask(action, args) {
                Value::Bool(b) => b,
                other => panic!("{action} gave {other:?}"),
            }
        }
    }

    fn t(s: &str) -> Value {
        Value::text(s)
    }

    #[test]
    fn a_text_is_counted_cut_and_cased_by_what_a_reader_sees() {
        let s = surface();
        assert_eq!(s.int("count", vec![t("hello")]), 5);
        // A family, a flag and a letter with an accent are each one thing to a reader.
        assert_eq!(s.int("count", vec![t("👩‍👩‍👧‍👦")]), 1);
        assert_eq!(s.int("count", vec![t("🇺🇸🇫🇷")]), 2);
        assert_eq!(s.int("count", vec![t("e\u{301}")]), 1);
        assert_eq!(s.int("count", vec![t("")]), 0);
        assert!(s.yes("empty", vec![t("")]));
        assert!(!s.yes("empty", vec![t(" ")]));

        assert_eq!(
            s.text("cut", vec![t("🇺🇸x"), Value::Int(1), Value::Int(2)]),
            "x"
        );
        // A cut past either end stops at it rather than failing.
        assert_eq!(
            s.text("cut", vec![t("ab"), Value::Int(-3), Value::Int(9)]),
            "ab"
        );
        assert_eq!(
            s.text("cut", vec![t("ab"), Value::Int(2), Value::Int(1)]),
            ""
        );

        assert_eq!(s.text("tidy", vec![t("\u{a0} pad \t")]), "pad");
        assert_eq!(s.text("shout", vec![t("straße")]), "STRASSE");
        assert_eq!(s.text("whisper", vec![t("ΟΔΟΣ")]), "οδος");
    }

    /// The words of a text are its runs of non-whitespace: what a person counts of what they
    /// wrote, whatever the whitespace between them and whether or not it is a letter.
    #[test]
    fn a_text_is_counted_in_words_as_a_person_counts_them() {
        let s = surface();
        assert_eq!(s.int("words", vec![t("a note of four words")]), 5);
        assert_eq!(s.int("words", vec![t("  one\ttwo\n\nthree  ")]), 3);
        assert_eq!(s.int("words", vec![t("")]), 0);
        assert_eq!(s.int("words", vec![t(" \n\t ")]), 0);
        assert_eq!(s.int("words", vec![t("— …")]), 2, "punctuation counts");
        assert_eq!(s.int("words", vec![t("👩‍👩‍👧‍👦 x")]), 2);
    }

    #[test]
    fn asking_about_a_text_locates_it_in_graphemes() {
        let s = surface();
        assert!(s.yes("starts", vec![t("hello"), t("he")]));
        assert!(!s.yes("starts", vec![t("hello"), t("el")]));
        assert!(s.yes("ends", vec![t("hello"), t("lo")]));
        assert!(!s.yes("ends", vec![t("hello"), t("he")]));
        assert!(s.yes("holds", vec![t("hello"), t("ell")]));
        assert!(!s.yes("holds", vec![t("hello"), t("z")]));
        assert_eq!(s.int("at", vec![t("a👩‍👩‍👧‍👦b"), t("b")]), 2);
        assert_eq!(s.int("at", vec![t("abc"), t("z")]), -1);
        assert_eq!(s.int("at", vec![t("abc"), t("")]), 0);

        // The pattern is the one search's identifiers use: `*` stands for any run, and the
        // match is case-sensitive here.
        assert!(s.yes("glob", vec![t("abc"), t("a*")]));
        assert!(s.yes("glob", vec![t("abc"), t("*c")]));
        assert!(s.yes("glob", vec![t("abc"), t("a*c")]));
        assert!(s.yes("glob", vec![t("ac"), t("a*c")]));
        assert!(!s.yes("glob", vec![t("abd"), t("a*c")]));
        assert!(!s.yes("glob", vec![t("Abc"), t("a*")]));

        assert_eq!(s.text("swap", vec![t("a-b-c"), t("b"), t("x")]), "a-x-c");
        assert_eq!(s.text("swap", vec![t("aaa"), t("a"), t("aa")]), "aaaaaa");
        assert_eq!(s.int("number", vec![t("42")]), 42);
        assert_eq!(s.int("number", vec![t("-7")]), -7);
        assert_eq!(s.int("number", vec![t("4.2")]), -1);
    }

    #[test]
    fn a_text_splits_and_a_list_of_text_joins() {
        let s = surface();
        assert_eq!(
            s.ask("pieces", vec![t("a,b,,c"), t(",")]),
            Value::List(vec![t("a"), t("b"), t(""), t("c")])
        );
        // No separator splits into what a reader sees as one thing.
        assert_eq!(
            s.ask("pieces", vec![t("🇺🇸x"), t("")]),
            Value::List(vec![t("🇺🇸"), t("x")])
        );
        assert_eq!(
            s.ask("pieces", vec![t(""), t(",")]),
            Value::List(vec![t("")])
        );
        assert_eq!(s.text("rejoined", vec![t("a,b,c"), t(",")]), "a|b|c");
        assert_eq!(
            s.text("tied", vec![Value::List(vec![t("a"), t("b")]), t(", ")]),
            "a, b"
        );
        assert_eq!(s.text("tied", vec![Value::List(Vec::new()), t(", ")]), "");
    }

    #[test]
    fn numbers_are_bounded_rounded_and_exchanged() {
        let s = surface();
        assert_eq!(s.int("lowest", vec![Value::Int(3), Value::Int(8)]), 3);
        assert_eq!(s.int("highest", vec![Value::Int(3), Value::Int(8)]), 8);
        assert_eq!(s.int("bounded", vec![Value::Int(-4)]), 0);
        assert_eq!(s.int("bounded", vec![Value::Int(4)]), 4);
        assert_eq!(s.int("bounded", vec![Value::Int(14)]), 10);
        assert_eq!(s.int("size", vec![Value::Int(-4)]), 4);
        assert_eq!(s.ask("as_float", vec![Value::Int(4)]), Value::Float(4.0));
        assert_eq!(s.text("int_text", vec![Value::Int(-4)]), "-4");

        assert_eq!(
            s.ask("least", vec![Value::Float(0.25), Value::Float(0.75)]),
            Value::Float(0.25)
        );
        assert_eq!(
            s.ask("fhighest", vec![Value::Float(0.25), Value::Float(0.75)]),
            Value::Float(0.75)
        );
        assert_eq!(
            s.ask("fbounded", vec![Value::Float(-2.0)]),
            Value::Float(0.0)
        );
        assert_eq!(
            s.ask("fbounded", vec![Value::Float(2.0)]),
            Value::Float(1.0)
        );
        assert_eq!(s.ask("fsize", vec![Value::Float(-2.5)]), Value::Float(2.5));

        assert_eq!(s.int("whole", vec![Value::Float(2.5)]), 3);
        assert_eq!(s.int("whole", vec![Value::Float(-2.5)]), -3);
        assert_eq!(s.int("down", vec![Value::Float(2.9)]), 2);
        assert_eq!(s.int("down", vec![Value::Float(-2.1)]), -3);
        assert_eq!(s.int("up", vec![Value::Float(2.1)]), 3);
        let places = |x: f64, n: i64| vec![Value::Float(x), Value::Int(n)];
        assert_eq!(
            s.ask("whole_to", places(1234.5678, 1)),
            Value::Float(1234.6)
        );
        assert_eq!(
            s.ask("down_to", places(1234.5678, 2)),
            Value::Float(1234.56)
        );
        assert_eq!(s.ask("up_to", places(1234.5678, 2)), Value::Float(1234.57));
        assert_eq!(s.ask("whole_to", places(2.5, 0)), Value::Float(3.0));
        assert_eq!(s.int("as_int", vec![Value::Float(2.9)]), 2);
        assert_eq!(s.int("as_int", vec![Value::Float(-2.9)]), -2);
        assert_eq!(s.text("float_text", vec![Value::Float(1.5)]), "1.5");

        assert_eq!(s.text("flag", vec![Value::Bool(true)]), "true");
        assert_eq!(s.text("flag", vec![Value::Bool(false)]), "false");
    }

    #[test]
    fn a_list_of_numbers_sums_and_averages() {
        let s = surface();
        let nums = |xs: &[i64]| Value::List(xs.iter().copied().map(Value::Int).collect());
        let reals = |xs: &[f64]| Value::List(xs.iter().copied().map(Value::Float).collect());
        assert_eq!(s.int("total", vec![nums(&[1, 2, 3])]), 6);
        assert_eq!(s.int("total", vec![nums(&[])]), 0);
        assert_eq!(
            s.ask("total_f", vec![reals(&[0.5, 0.25])]),
            Value::Float(0.75)
        );
        assert_eq!(s.ask("mean", vec![nums(&[1, 2])]), Value::Float(1.5));
        // An average of nothing is zero rather than a failure.
        assert_eq!(s.ask("mean", vec![nums(&[])]), Value::Float(0.0));
        assert!(s.yes("among", vec![Value::List(vec![t("a"), t("b")]), t("b")]));
        assert!(!s.yes("among", vec![Value::List(vec![t("a"), t("b")]), t("c")]));
        // A scalar item goes in boxed, as a set's element does.
        assert!(s.yes("has_num", vec![nums(&[1, 2, 3]), Value::Int(2)]));
        assert!(!s.yes("has_num", vec![nums(&[1, 2, 3]), Value::Int(4)]));
        assert!(s.yes("has_real", vec![reals(&[0.5, 0.25]), Value::Float(0.25)]));
        assert_eq!(s.int("where_num", vec![nums(&[4, 5, 6]), Value::Int(5)]), 1);
        assert_eq!(
            s.int("where_num", vec![nums(&[4, 5, 6]), Value::Int(7)]),
            -1
        );
    }
}
