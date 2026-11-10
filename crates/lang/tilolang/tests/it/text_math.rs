//! The text and number surface as the checker reads it: what each method gives, what it takes,
//! and how a mistake about a type is named.

use crate::common::{codes, compile_body, compile_with, validate};

fn err(body: &str) -> tilolang::Diagnostic {
    let src = format!("app t.x \"X\"\n{body}\n");
    let mut d = compile_with(&src, &[]).expect_err("must be refused");
    d.remove(0)
}

#[test]
fn every_text_and_number_method_compiles_to_a_typed_value() {
    // Each function's return type is the assertion: the whole surface is written once with the
    // types it gives, and the compiler refuses any of them that gives another.
    compile_body(
        "fn count(t: Text) -> Int { return t.len() }\n\
fn words(t: Text) -> Int { return t.words() }\n\
fn empty(t: Text) -> Bool { return t.is_empty() }\n\
fn tidy(t: Text) -> Text { return t.trim() }\n\
fn shout(t: Text) -> Text { return t.upper() }\n\
fn whisper(t: Text) -> Text { return t.lower() }\n\
fn starts(t: Text) -> Bool { return t.starts_with(\"a\") }\n\
fn ends(t: Text) -> Bool { return t.ends_with(\"z\") }\n\
fn holds(t: Text) -> Bool { return t.contains(\"m\") }\n\
fn glob(t: Text) -> Bool { return t.matches(\"a*b\") }\n\
fn at(t: Text) -> Option<Int> { return t.find(\"b\") }\n\
fn cut(t: Text) -> Text { return t.slice(1, t.len()) }\n\
fn swap(t: Text) -> Text { return t.replace(\"a\", \"b\") }\n\
fn pieces(t: Text) -> List<Text> { return t.split(\",\") }\n\
fn tied(xs: List<Text>) -> Text { return xs.join(\", \") }\n\
fn number(t: Text) -> Option<Int> { return t.to_int() }\n\
fn lowest(a: Int, b: Int) -> Int { return a.min(b) }\n\
fn highest(a: Int, b: Int) -> Int { return a.max(b) }\n\
fn bounded(n: Int) -> Int { return n.clamp(0, 10) }\n\
fn size(n: Int) -> Int { return n.abs() }\n\
fn as_float(n: Int) -> Float { return n.to_float() }\n\
fn int_text(n: Int) -> Text { return n.to_text() }\n\
fn least(a: Float, b: Float) -> Float { return a.min(b) }\n\
fn fhighest(a: Float, b: Float) -> Float { return a.max(b) }\n\
fn fbounded(f: Float) -> Float { return f.clamp(0.0, 1.0) }\n\
fn fsize(f: Float) -> Float { return f.abs() }\n\
fn whole(f: Float) -> Int { return f.round() }\n\
fn down(f: Float) -> Int { return f.floor() }\n\
fn up(f: Float) -> Int { return f.ceil() }\n\
fn whole_to(f: Float) -> Float { return f.round(2) }\n\
fn down_to(f: Float) -> Float { return f.floor(2) }\n\
fn up_to(f: Float) -> Float { return f.ceil(2) }\n\
fn as_int(f: Float) -> Int { return f.to_int() }\n\
fn float_text(f: Float) -> Text { return f.to_text() }\n\
fn total(xs: List<Int>) -> Int { return xs.sum() }\n\
fn total_f(xs: List<Float>) -> Float { return xs.sum() }\n\
fn mean(xs: List<Int>) -> Float { return xs.avg() }\n\
fn among(xs: List<Text>, x: Text) -> Bool { return xs.contains(x) }\n\
fn flag(b: Bool) -> Text { return b.to_text() }",
    );
}

#[test]
fn a_container_field_reads_contains_as_its_items_do() {
    let c = compile_body(
        "type S {\n    tags: Set<Text>\n    notes: List<Int>\n}\n\
fn has_tag(s: S, t: Text) -> Bool { return s.tags.contains(t) }\n\
fn has_note(s: S, n: Int) -> Bool { return s.notes.contains(n) }",
    );
    // A scalar item is boxed going in, as a set's element is.
    validate(&c.wasm);
}

#[test]
fn a_method_takes_the_values_its_type_takes() {
    // Text where a text goes, a number where a number goes: no silent conversion between them.
    assert_eq!(
        codes("fn f(t: Text) -> Bool {\n    return t.starts_with(1)\n}"),
        vec!["E020"]
    );
    // Each argument the method takes is checked against its type.
    assert_eq!(
        codes("fn f(t: Text) -> Text {\n    return t.replace(1, 2)\n}"),
        vec!["E020", "E020"]
    );
    assert_eq!(
        codes("fn f(t: Text) -> List<Text> {\n    return t.split(1)\n}"),
        vec!["E020"]
    );
    assert_eq!(
        codes("fn f(n: Int) -> Int {\n    return n.min(1.5)\n}"),
        vec!["E020"]
    );
    assert_eq!(
        codes("fn f(x: Float) -> Float {\n    return x.clamp(0, 1)\n}"),
        vec!["E020", "E020"]
    );
    assert_eq!(
        codes("fn f(t: Text) -> Option<Int> {\n    return t.find(1)\n}"),
        vec!["E020"]
    );
}

#[test]
fn a_method_takes_the_number_of_arguments_it_names() {
    assert_eq!(
        codes("fn f(t: Text) -> Text {\n    return t.slice(0)\n}"),
        vec!["E022"]
    );
    assert_eq!(
        codes("fn f(t: Text) -> Text {\n    return t.trim(1)\n}"),
        vec!["E022"]
    );
    assert_eq!(
        codes("fn f(t: Text) -> Int {\n    return t.len(1)\n}"),
        vec!["E022"]
    );
    assert_eq!(
        codes("fn f(t: Text) -> Bool {\n    return t.contains(\"a\", \"b\")\n}"),
        vec!["E022"]
    );
    assert_eq!(
        codes("fn f(x: Float) -> Int {\n    return x.round(1, 2)\n}"),
        vec!["E022"]
    );
    assert_eq!(
        codes("fn f(x: Float) -> Float {\n    return x.round(places: 1)\n}"),
        vec!["E022"]
    );
    assert_eq!(
        codes("fn f(n: Int) -> Int {\n    return n.clamp(0)\n}"),
        vec!["E022"]
    );
    assert_eq!(
        codes("fn f(xs: List<Int>) -> Int {\n    return xs.sum(1)\n}"),
        vec!["E022"]
    );
}

#[test]
fn a_method_belongs_to_the_type_that_has_it() {
    assert_eq!(
        codes("fn f(n: Int) -> Int {\n    return n.trim()\n}"),
        vec!["E027"]
    );
    assert_eq!(
        codes("fn f(t: Text) -> Int {\n    return t.round()\n}"),
        vec!["E027"]
    );
    assert_eq!(
        codes("fn f(xs: List<Text>) -> Int {\n    return xs.sum()\n}"),
        vec!["E027"]
    );
    assert_eq!(
        codes("fn f(xs: List<Int>) -> Text {\n    return xs.join(\",\")\n}"),
        vec!["E027"]
    );
    assert_eq!(
        codes("fn f(b: Bool) -> Bool {\n    return b.contains(\"x\")\n}"),
        vec!["E027"]
    );
}

#[test]
fn a_wrong_method_message_names_what_the_type_has() {
    let int = err("fn f(n: Int) -> Int {\n    return n.trim()\n}");
    assert_eq!(int.code, "E027");
    assert!(
        int.message
            .contains("min, max, clamp, abs, to_float, to_text"),
        "{}",
        int.message
    );

    let text = err("fn f(t: Text) -> Int {\n    return t.round()\n}");
    assert_eq!(text.code, "E027");
    assert!(
        text.message
            .contains("len, words, is_empty, trim, lower, upper, starts_with"),
        "{}",
        text.message
    );
    assert!(text.message.contains("to_int"), "{}", text.message);
    // A number's surface is not offered where a text's is.
    assert!(!text.message.contains("clamp"), "{}", text.message);
}
