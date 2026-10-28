//! The language changes found by writing the apps (docs/designs/app-language/language-changes.md):
//! calendar literals, `if let`, `?.` and `??`, key paths, `remove_where`, moves by neighbour and
//! optional views. Each means the same in an action, a fn and a view.

use crate::common::{codes, compile, validate};

const C: &str = "type C {\n    t: Text\n    b: Bool = false\n    due: Option<Date> = .none\n    at: Option<Time> = .none\n    items: Collection<C>\n}\nroot C\ndefault view v for C\nview v(c: C) {\n    text(c.t)\n}\n";

fn expect(code: &str, rest: &str) {
    let body = format!("{C}{rest}");
    let got = codes(&body);
    assert!(
        got.iter().any(|c| c == code),
        "expected {code} for:\n{body}\ngot {got:?}"
    );
}

fn ok(rest: &str) {
    let src = format!("app t.x \"X\"\n{C}{rest}\n");
    let compiled =
        crate::common::compile_with(&src, &[]).unwrap_or_else(|d| panic!("{rest}\n{d:#?}"));
    validate(&compiled.wasm);
    // The canonical text reads back as itself.
    let printed = tilolang::print_module(&tilolang::parse(&src).expect("parses"));
    let again =
        tilolang::print_module(&tilolang::parse(&printed).expect("the printed text parses"));
    assert_eq!(printed, again);
}

#[test]
fn calendar_literals_are_dates_times_and_months() {
    ok("action a(c: C) {\n    c.due = 2026-06-29\n    c.at = 2026-06-29T07:02Z\n}");
    ok("fn f(c: C) -> Bool {\n    c.due == .some(2026-06-29)\n}");
    ok("fn f() -> List<C> {\n    query(C, in: root C, due: 2026-08)\n}");
    ok("fn f() -> List<C> {\n    query(C, in: root C, due: 2026-06-01..2026-07-01)\n}");
    // A month is a span of days: only a filter takes it.
    expect(
        "E105",
        "fn f(c: C) -> Bool {\n    c.due == .some(2026-08)\n}",
    );
    expect("E004", "fn f() -> Date {\n    2026-02-30\n}");
    expect("E004", "fn f() -> Time {\n    2026-06-29T07:02\n}");
    expect("E020", "fn f() -> Date {\n    2026-06-29T07:02Z\n}");
    let src = format!("app t.x \"X\"\n{C}fn f() -> Time {{\n    2026-06-29T07:02Z\n}}\n");
    let printed = tilolang::print_module(&tilolang::parse(&src).expect("parses"));
    assert!(printed.contains("    2026-06-29T07:02Z\n"), "{printed}");
}

#[test]
fn if_let_binds_option_values_in_actions_fns_and_views() {
    ok(
        "fn f(c: C) -> Text {\n    if let first = c.items.first(), d = first.due {\n        \"{first.t} {d}\"\n    } else {\n        \"none\"\n    }\n}",
    );
    ok("action a(c: C) {\n    if let last = c.items.last() {\n        c.t = last.t\n    }\n}");
    ok(
        "view w(c: C) {\n    stack {\n        if let last = c.items.last(), d = last.due {\n            text(last.t)\n            text(\"{d}\")\n        } else {\n            text(\"nothing due\")\n        }\n    }\n}",
    );
    expect(
        "E106",
        "fn f(c: C) -> Text {\n    if let t = c.t {\n        t\n    } else {\n        \"\"\n    }\n}",
    );
    expect(
        "E038",
        "fn f(c: C) -> Text {\n    if let l = c.items.last() {\n        l.t\n    }\n}",
    );
    // The name is not seen in the `else` branch.
    expect(
        "E014",
        "fn f(c: C) -> Text {\n    if let l = c.items.last() {\n        l.t\n    } else {\n        l.t\n    }\n}",
    );
}

#[test]
fn optional_chaining_and_fallback() {
    ok("fn f(c: C) -> Text {\n    c.items.last()?.t ?? \"No items yet\"\n}");
    ok("fn f(c: C) -> Option<Date> {\n    c.items.last()?.due\n}");
    ok("fn f(c: C) -> Int {\n    c.items.first()?.items?.len() ?? 0\n}");
    ok("view w(c: C) {\n    text(c.items.last()?.t ?? \"empty\")\n}");
    expect("E106", "fn f(c: C) -> Text {\n    c?.t ?? \"\"\n}");
    expect("E106", "fn f(c: C) -> Text {\n    c.t ?? \"\"\n}");
    expect(
        "E020",
        "fn f(c: C) -> Text {\n    c.items.last()?.t ?? 1\n}",
    );
}

#[test]
fn key_paths_stand_for_a_lambda_reading_a_field() {
    ok("fn f(c: C) -> Int {\n    c.items.count(.b)\n}");
    ok("fn f(c: C) -> List<C> {\n    c.items.filter(.b).sort_by(.t)\n}");
    ok("fn f(c: C) -> List<Text> {\n    c.items.map(.t)\n}");
    expect(
        "E021",
        "fn f(c: C) -> List<C> {\n    c.items.sort_by(.title)\n}",
    );
    expect("E020", "fn f(c: C) -> Int {\n    c.items.count(.t)\n}");
}

#[test]
fn remove_where_removes_and_counts() {
    ok("action clear(c: C) -> Int {\n    c.items.remove_where(.b)\n}");
    ok("action clear(c: C) -> Int {\n    c.items.remove_where(x -> x.t == \"\")\n}");
    expect(
        "E040",
        "fn f(c: C) -> Int {\n    c.items.remove_where(.b)\n}",
    );
    expect(
        "E051",
        "action clear(c: C) -> Int {\n    c.items.remove_where(true)\n}",
    );
}

#[test]
fn an_optional_view_renders_when_given() {
    ok(
        "view frame<R>(of: R, extra: Option<View> = .none) {\n    stack {\n        text(\"head\")\n        extra\n    }\n}\nview w(c: C) {\n    frame(c)\n}\nview z(c: C) {\n    frame(c) {\n        extra: {\n            text(c.t)\n        }\n    }\n}",
    );
    ok(
        "view frame<R>(of: R, extra: Option<View> = .none) {\n    stack {\n        if let e = extra {\n            e\n        } else {\n            text(\"nothing\")\n        }\n    }\n}\nview w(c: C) {\n    frame(c)\n}",
    );
}

#[test]
fn a_root_is_an_expression_anywhere() {
    ok(
        "view w() {\n    let c = root C\n    text(c.t)\n}\naction a() {\n    let c = root C\n    c.t = \"x\"\n}",
    );
    let _ = compile;
}
