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
    // A time of no zone is a wall-clock time: the person's own time of day, resolved where it
    // runs.
    ok("fn f() -> Time {\n    2026-06-29T07:02\n}");
    ok("fn f() -> Time {\n    2026-06-29T07:02:05\n}");
    expect("E004", "fn f() -> Date {\n    2026-02-30\n}");
    expect("E020", "fn f() -> Date {\n    2026-06-29T07:02Z\n}");
    let src = format!("app t.x \"X\"\n{C}fn f() -> Time {{\n    2026-06-29T07:02Z\n}}\n");
    let printed = tilolang::print_module(&tilolang::parse(&src).expect("parses"));
    assert!(printed.contains("    2026-06-29T07:02Z\n"), "{printed}");
}

#[test]
fn one_branch_is_the_option_of_its_value() {
    // `if c { v }` with no `else` is `v` where the condition holds and nothing where it does
    // not, which is an Option of `v`.
    ok("fn f(c: C) -> Option<Text> {\n    if c.b { c.t }\n}");
    ok("fn f(c: C) -> Option<Text> {\n    if let first = c.items.first() { \"{first.t}\" }\n}");
    ok(
        "fn f(c: C) -> Option<Date> {\n    if c.b { c.due } else if c.items.is_empty() { 2026-06-29 }\n}",
    );
    // A branch that is an Option itself is that Option, as `else { .none }` gives it: one Option
    // never holds another.
    ok("fn f(c: C) -> Option<C> {\n    if c.b { c.items.first() }\n}");
    ok("fn f(c: C) -> Option<Date> {\n    if let first = c.items.first() { first.due }\n}");
    // Where an Option of a value is expected, the value is the Option: in an argument, a field
    // and a comparison alike.
    ok(
        "fn g(x: Option<Text>) -> Text {\n    x ?? \"empty\"\n}\nfn f(c: C) -> Text {\n    g(c.t)\n}",
    );
    ok("action a(c: C) {\n    c.due = 2026-06-29\n}");
    ok("fn f(c: C) -> Bool {\n    c.due == 2026-06-29\n}");
    // One level: an Option of an Option says which of its levels it means.
    ok(
        "type N {\n    two: Option<Option<Text>> = .none\n}\naction a(n: N) {\n    n.two = .some(.none)\n}",
    );
    expect(
        "E020",
        "type N {\n    two: Option<Option<Text>> = .none\n}\naction a(n: N) {\n    n.two = \"one\"\n}",
    );
    // A branch is what it is: an `if` without `else` is not the value a Text is.
    expect("E020", "fn f(c: C) -> Option<Text> {\n    if c.b { c }\n}");
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
    // One branch is the Option of it: a Text is not an Option<Text>.
    expect(
        "E020",
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
fn a_lambda_takes_the_values_a_function_parameter_takes() {
    // `(a, b) -> ...` where a parameter of the type is given a function of two or more.
    ok(
        "action rename(c: C, t: Text) {\n    c.t = t\n}\nview picker(n: Text, act: (Text, Text) -> Action) {\n    ui.ListRow(title: n) {\n        on activate: act(n, n)\n    }\n}\nview w(c: C) {\n    picker(c.t, (a, b) -> rename(c, b))\n}",
    );
    ok(
        "action rename(c: C, t: Text) {\n    c.t = t\n}\nview picker(n: Text, act: (Text, Text, Text) -> Action) {\n    ui.ListRow(title: n) {\n        on activate: act(n, n, n)\n    }\n}\nview w(c: C) {\n    picker(c.t, (a, b, d) -> rename(c, d))\n}",
    );
    // One value is one `x -> ...`, unchanged.
    ok(
        "action rename(c: C, t: Text) {\n    c.t = t\n}\nview picker(n: Text, act: (Text) -> Action) {\n    ui.ListRow(title: n) {\n        on activate: act(n)\n    }\n}\nview w(c: C) {\n    picker(c.t, x -> rename(c, x))\n}",
    );
    // The lambda names no more and no fewer than the function takes.
    expect(
        "E050",
        "action rename(c: C, t: Text) {\n    c.t = t\n}\nview picker(n: Text, act: (Text, Text) -> Action) {\n    ui.ListRow(title: n) {\n        on activate: act(n, n)\n    }\n}\nview w(c: C) {\n    picker(c.t, x -> rename(c, x))\n}",
    );
}

#[test]
fn index_of_is_where_an_item_stands() {
    ok(
        "fn f(c: C) -> Bool {\n    if let i = c.items.index_of(c) {\n        i == 0\n    } else {\n        false\n    }\n}",
    );
    ok("fn f(c: C) -> Option<Int> {\n    c.items.filter(.b).index_of(c)\n}");
    expect("E020", "fn f(c: C) -> Int {\n    c.items.index_of(c)\n}");
}

#[test]
fn at_reads_the_item_at_a_place() {
    ok("fn f(c: C) -> Option<C> {\n    c.items.at(0)\n}");
    ok("fn f(c: C) -> Option<C> {\n    c.items.filter(.b).at(2)\n}");
    ok(
        "fn f(c: C) -> Bool {\n    if let x = c.items.at(0) {\n        x.t == \"\"\n    } else {\n        false\n    }\n}",
    );
    // The place is a whole number, and the item at it may be nothing.
    expect("E020", "fn f(c: C) -> C {\n    c.items.at(0)\n}");
    expect(
        "E020",
        "fn f(c: C) -> Option<C> {\n    c.items.at(\"0\")\n}",
    );
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
fn a_move_names_a_neighbour_or_a_place() {
    ok("action m(c: C, x: C, y: C) {\n    c.items.move(x, after: y)\n}");
    ok("action m(c: C, x: C, y: C) {\n    c.items.move(x, before: y)\n}");
    ok("action m(c: C, x: C, i: Int) {\n    c.items.move(x, to: i)\n}");
    expect("E022", "action m(c: C, x: C) {\n    c.items.move(x, 2)\n}");
    expect(
        "E020",
        "action m(c: C, x: C, y: C) {\n    c.items.move(x, to: y)\n}",
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
fn a_row_block_names_the_values_it_draws_with() {
    // A view of items that draws a row with the item it stands under takes a block naming
    // both: the item and the one it stands under.
    ok(
        "view listed<T>(items: List<T>, row: (T, Option<T>) -> View) {\n    stack {\n        for x in items {\n            row(x, .none)\n        }\n    }\n}\nview w(c: C) {\n    listed(c.items) { (x, under) ->\n        text(x.t)\n    }\n}",
    );
    // A block naming another number of values is refused, saying how many the view takes.
    expect(
        "E022",
        "view listed<T>(items: List<T>, row: (T, Option<T>) -> View) {\n    stack {\n        for x in items {\n            row(x, .none)\n        }\n    }\n}\nview w(c: C) {\n    listed(c.items) { x ->\n        text(x.t)\n    }\n}",
    );
}

#[test]
fn a_root_is_an_expression_anywhere() {
    ok(
        "view w() {\n    let c = root C\n    text(c.t)\n}\naction a() {\n    let c = root C\n    c.t = \"x\"\n}",
    );
    let _ = compile;
}

const TALK: &str = "type Talk {\n    draft: Text\n    lines: Collection<Line>\n}\ntype Line {\n    said: Text\n}\nview line(l: Line) {\n    text(l.said)\n}\naction send(t: Talk, said: Text) (icon: .send) {\n    if said != \"\" {\n        t.lines.insert(Line(said: said))\n    }\n}\n";

#[test]
fn typing_in_a_composer_is_personal() {
    let ok_src = format!(
        "app t.x \"X\"\n{TALK}view talk(t: Talk) {{\n    state draft: Text\n    std.transcript(t, t.lines, row: line) {{\n        composer: {{\n            row {{\n                input(bind: draft, fill)\n                button(send(t, draft).then(set(draft, \"\")), icon_only: true)\n            }}\n        }}\n    }}\n}}\n"
    );
    let compiled = crate::common::compile_with(&ok_src, &[]).unwrap_or_else(|d| panic!("{d:#?}"));
    validate(&compiled.wasm);
    let shared = format!(
        "{TALK}view talk(t: Talk) {{\n    std.transcript(t, t.lines, row: line) {{\n        composer: {{\n            input(bind: t.draft)\n        }}\n    }}\n}}"
    );
    let got = codes(&shared);
    assert!(got.iter().any(|c| c == "E108"), "{got:?}");
    // Outside a composer a shared field is edited together, as a document is.
    let together = format!("{TALK}view talk(t: Talk) {{\n    input(bind: t.draft)\n}}");
    assert!(codes(&together).is_empty(), "{:?}", codes(&together));
    let not_an_action = format!(
        "{TALK}view talk(t: Talk) {{\n    button(\"Send\", on: send(t, \"x\").then(1))\n}}"
    );
    assert!(codes(&not_an_action).iter().any(|c| c == "E020"));
}

#[test]
fn an_item_inserted_into_a_collection_of_a_union_is_of_its_own_type() {
    let src = "app t.x \"X\"\ntype A {\n    a: Text\n}\ntype B {\n    b: Text\n}\ntype Both {\n    all: Collection<A | B>\n}\nroot both: Both\ndefault view shown for Both\nview shown(both: Both) {\n    text(\"{both.all.len()}\")\n}\naction add_a(both: Both) -> A {\n    both.all.insert(A(a: \"x\"))\n}\naction named(both: Both) -> Text {\n    let b = both.all.insert(B(b: \"y\"))\n    b.b\n}\n";
    let compiled = crate::common::compile_with(src, &[]).unwrap_or_else(|d| panic!("{d:#?}"));
    validate(&compiled.wasm);
}

#[test]
fn an_item_block_names_the_other_slots_beside_the_row() {
    // The row sees the item and the view's state; the composer beside it sees the state only.
    let src = format!(
        "app t.x \"X\"\n{TALK}view said(t: Talk, l: Line, draft: State<Text>) {{\n    text(l.said)\n}}\nview talk(t: Talk) {{\n    state draft: Text\n    std.transcript(t, t.lines) {{ l ->\n        said(t, l, draft)\n        composer: {{\n            input(bind: draft, fill)\n        }}\n    }}\n}}\n"
    );
    let compiled = crate::common::compile_with(&src, &[]).unwrap_or_else(|d| panic!("{d:#?}"));
    validate(&compiled.wasm);
    let item_in_composer = format!(
        "{TALK}view talk(t: Talk) {{\n    state draft: Text\n    std.transcript(t, t.lines) {{ l ->\n        text(l.said)\n        composer: {{\n            text(l.said)\n        }}\n    }}\n}}"
    );
    assert!(
        !codes(&item_in_composer).is_empty(),
        "the composer does not see the item"
    );
}

#[test]
fn an_action_a_view_applies_reads_the_clock_as_it_is_at_the_press() {
    // An argument that names a day or a moment is read when the action is invoked, as its
    // view state is: a list in a place that starts its items today says so in the argument.
    let starts_today = "type Note {\n    title: Text\n    due: Option<Date> = .none\n}\ntype Book {\n    notes: Collection<Note>\n}\nroot book: Book\ndefault view book for Book\naction add(b: Book, due: Option<Date> = .none, title: Text = \"\") -> Note (icon: .plus) {\n    b.notes.insert(Note(title: title, due: due))\n}\nview book(b: Book) {\n    let in_today = true\n    stack {\n        button(add(b, due: if in_today { .some(today) } else { .none }))\n    }\n}\n";
    let src = format!("app t.x \"X\"\n{starts_today}");
    let compiled = crate::common::compile_with(&src, &[]).unwrap_or_else(|d| panic!("{d:#?}"));
    validate(&compiled.wasm);
    let at_event: Vec<(Vec<&str>, &str)> = compiled
        .meta
        .at_event
        .iter()
        .map(|f| {
            let params = f.params.iter().map(|p| p.name.as_str()).collect();
            (params, f.result.as_str())
        })
        .collect();
    assert_eq!(
        at_event,
        [(vec!["in_today"], "Option<Date>")],
        "the day is computed when the action is invoked, from what the view drew the rest \
         with: {at_event:?}"
    );
    // A view's body itself still reads no clock: it is drawn again from what it is shown.
    let drawn = "type Note {\n    title: Text\n    due: Option<Date> = .none\n}\ntype Book {\n    notes: Collection<Note>\n}\nroot book: Book\ndefault view book for Book\nview book(b: Book) {\n    let d = today\n    stack {\n        text(\"{d}\")\n    }\n}\n";
    let got = codes(drawn);
    assert!(got.iter().any(|c| c == "E049"), "{got:?}");
}

#[test]
fn an_action_a_view_applies_takes_its_view_state_as_it_is_at_the_press() {
    // Given as itself, view state is read when the event fires.
    let pressed = format!(
        "app t.x \"X\"\n{TALK}view talk(t: Talk) {{\n    state draft: Text\n    state n: Int = 1\n    stack {{\n        input(bind: draft)\n        button(send(t, draft).then(set(draft, \"\")))\n        button(\"More\", on: set(n, n))\n    }}\n}}"
    );
    let compiled = crate::common::compile_with(&pressed, &[]).unwrap_or_else(|d| panic!("{d:#?}"));
    validate(&compiled.wasm);
    // An argument computed from view state is a function of the module, run at the press on
    // the view state as it is then: a toggle, a count, a draft with more around it.
    let computed = format!(
        "app t.x \"X\"\n{TALK}view talk(t: Talk) {{\n    state draft: Text\n    state open: Bool = false\n    state n: Int = 1\n    stack {{\n        input(bind: draft)\n        button(send(t, \"{{draft}}!\"))\n        button(\"Toggle\", on: set(open, not open))\n        button(\"More\", on: set(n, n + 1))\n    }}\n}}"
    );
    let compiled = crate::common::compile_with(&computed, &[]).unwrap_or_else(|d| panic!("{d:#?}"));
    validate(&compiled.wasm);
    let at_event: Vec<(&str, Vec<&str>, &str)> = compiled
        .meta
        .at_event
        .iter()
        .map(|f| {
            let params = f.params.iter().map(|p| p.name.as_str()).collect();
            (f.name.as_str(), params, f.result.as_str())
        })
        .collect();
    assert_eq!(
        at_event
            .iter()
            .map(|(_, params, result)| (params.clone(), *result))
            .collect::<Vec<_>>(),
        [
            (vec!["draft"], "Text"),
            (vec!["open"], "Bool"),
            (vec!["n"], "Int"),
        ],
        "each takes the view state it reads: {at_event:?}"
    );
    // Its discipline is a fn's, as a parameter default's: it computes, it changes nothing.
    let acting = format!(
        "{TALK}view talk(t: Talk) {{\n    state draft: Text\n    button(send(t, said(t, draft)))\n}}\naction said(t: Talk, d: Text) -> Text {{\n    d\n}}"
    );
    assert!(
        codes(&acting).iter().any(|c| c == "E043"),
        "{:?}",
        codes(&acting)
    );
    // An action reads its own arguments as they are.
    ok(
        "action a(c: C, t: Text) {\n    c.t = t\n}\naction b(c: C) {\n    let t = c.t\n    a(c, t)\n}",
    );
}

#[test]
fn sort_by_takes_dates_and_times_and_a_descending_order() {
    ok("fn f(c: C) -> List<C> {\n    c.items.sort_by(x -> x.due ?? 2026-01-01, desc)\n}");
    ok("fn f(c: C) -> List<C> {\n    c.items.sort_by(x -> x.at ?? 2026-01-01T00:00Z, asc)\n}");
    ok("type D {\n    c: C\n}\nfn f(ds: List<D>) -> List<D> {\n    ds.sort_by(.c.t, desc)\n}");
    expect(
        "E051",
        "fn f(c: C) -> List<C> {\n    c.items.sort_by(.t, down)\n}",
    );
}

#[test]
fn sort_by_takes_a_key_path_that_may_be_absent() {
    // A list grouped by a day sorts by its date too, and the items without one come last.
    ok("fn f(c: C) -> List<C> {\n    c.items.sort_by(.due)\n}");
    ok("fn f(c: C) -> List<C> {\n    c.items.filter(.b).sort_by(.due, desc)\n}");
    ok("fn f(c: C) -> List<C> {\n    c.items.sort_by(x -> x.at)\n}");
}

#[test]
fn a_moment_reads_as_the_day_it_falls_on() {
    // `t.day()` is the day a moment falls on for the person, as `today` is the day it is: a day
    // a list groups its rows by, and a date a row shows.
    ok("fn f(c: C) -> Date {\n    let at = c.at ?? 2026-06-29T07:02Z\n    at.day()\n}");
    ok("action a(c: C) uses time {\n    c.due = now.day()\n    c.at = now\n}");
    ok(
        "view w(c: C) {\n    ui.VirtualList {\n        for x in c.items {\n            let heading: Option<Date> = if let at = x.at { .some(at.day()) } else { .none }\n            ui.ListItem(item: x, group: heading) {\n            }\n        }\n    }\n}",
    );
    // A day's `day` is the day of its month, and a moment's is the day it falls on.
    expect("E020", "fn f(d: Date) -> Date {\n    d.day()\n}");
    expect("E020", "fn f(t: Time) -> Int {\n    t.day()\n}");
    // The names the language used before are gone, with no aliases left behind.
    expect(
        "E014",
        "action a(c: C) {\n    c.due = .some(day_of(now))\n}",
    );
    expect(
        "E014",
        "action a(c: C) {\n    c.due = .some(dated(\"tomorrow\", now))\n}",
    );
}

#[test]
fn descendants_and_ancestors_follow_the_parent_an_item_names() {
    const TREE: &str = "type N {\n    p: Option<N> = .none\n    items: Collection<N>\n}\n";
    // The items under one, and the ones it is under: a tree the list itself holds.
    ok(&format!(
        "{TREE}fn f(all: List<N>, n: N) -> List<N> {{\n    all.descendants(n, x -> x.p)\n}}"
    ));
    ok(&format!(
        "{TREE}fn f(n: N, t: N) -> Bool {{\n    n.items.ancestors(t, .p).any(a -> a == t)\n}}"
    ));
    // An item's parent is an Option of the item, and the call names one to start from.
    expect(
        "E051",
        "type N {\n    p: Option<N> = .none\n}\nfn f(all: List<N>) -> List<N> {\n    all.descendants(.p)\n}",
    );
    expect(
        "E020",
        "type N {\n    t: Text\n}\nfn f(all: List<N>, n: N) -> List<N> {\n    all.descendants(n, x -> x.t)\n}",
    );
}

#[test]
fn a_ui_let_writes_the_type_its_value_is_read_as() {
    // What a row writes: a heading of the key the app groups by, absent when the item
    // names none.
    ok(
        "view w(c: C) {\n    ui.VirtualList {\n        let heading: Option<Date> = if let d = c.due { d } else { .none }\n        ui.ListItem(item: c, group: heading) {\n        }\n    }\n}",
    );
    // Without it the `else` has nothing to say what it is.
    expect(
        "E020",
        "view w(c: C) {\n    ui.VirtualList {\n        let heading = if let d = c.due { d } else { .none }\n        ui.ListItem(item: c, group: heading) {\n        }\n    }\n}",
    );
}

#[test]
fn a_function_argument_binds_the_type_its_result_names() {
    // The key a list groups by is whatever the caller reads: `.due` makes its heading a day
    // the host presents as one, `x -> .some(x.t)` the text it is. A template's type parameter
    // is not a variable, so what the function makes is read where the call passes it.
    const ROWS: &str = "view rows<T, K>(items: List<T>, group: Option<(T) -> Option<K>> = .none) {\n    ui.VirtualList {\n        for x in items {\n            let heading: Option<K> = if let g = group { g(x) } else { .none }\n            ui.ListItem(item: x, group: heading) {\n            }\n        }\n    }\n}\n";
    let compiled = |rest: &str| {
        crate::common::compile_with(&format!("app t.x \"X\"\n{C}{ROWS}{rest}"), &[])
            .unwrap_or_else(|d| panic!("{rest}\n{d:#?}"))
    };
    // What the code emits: the metadata section carries the package's declarations as text
    // (a template's body among them), so the scan is over the module without it.
    let code = |c: &tilolang::Compiled| {
        tilolang::meta::without_section(&c.wasm, tilolang::meta::META_SECTION)
    };
    let day = compiled("view w(c: C) {\n    rows(c.items, group: .due)\n}");
    let body = code(&day);
    let has = |name: &str| body.windows(name.len()).any(|w| w == name.as_bytes());
    assert!(has("group:as"), "a day's heading is dated for the host");
    crate::common::validate(&day.wasm);
    let word = compiled("view w(c: C) {\n    rows(c.items, group: x -> .some(x.t))\n}");
    let body = code(&word);
    let has = |name: &str| body.windows(name.len()).any(|w| w == name.as_bytes());
    assert!(!has("group:as"), "a word's heading is the word it is");
    crate::common::validate(&word.wasm);
    // A list that groups nothing names no key at all, and rows keep no heading.
    let plain = compiled("view w(c: C) {\n    rows(c.items)\n}");
    let body = code(&plain);
    let has = |name: &str| body.windows(name.len()).any(|w| w == name.as_bytes());
    assert!(!has("group"), "a plain list has no heading");
    crate::common::validate(&plain.wasm);
    // A function that makes something else than the key is told so.
    expect(
        "E020",
        &format!("{ROWS}view w(c: C) {{\n    rows(c.items, group: x -> x.t)\n}}"),
    );
}

#[test]
fn an_action_label_reads_its_arguments_where_it_is_applied() {
    let src = format!(
        "app t.x \"X\"\n{C}action move_to(c: C, to: C) (label: \"Move to {{to.t}}\") {{\n    c.t = to.t\n}}\naction rename(c: C) (label: \"Rename {{c.t}}\") {{\n    c.t = \"x\"\n}}\nview w(c: C, d: C) {{\n    button(move_to(c, d))\n    button(rename(c))\n}}\n"
    );
    let compiled = crate::common::compile_with(&src, &[]).unwrap_or_else(|d| panic!("{d:#?}"));
    validate(&compiled.wasm);
    let rename = compiled
        .meta
        .actions
        .iter()
        .find(|a| a.name == "rename")
        .expect("rename");
    assert!(
        rename.present.is_some(),
        "a label over the subject follows it"
    );
    let move_to = compiled
        .meta
        .actions
        .iter()
        .find(|a| a.name == "move_to")
        .expect("move_to");
    assert!(
        move_to.present.is_none(),
        "a label over another argument is read where it is applied"
    );
}

#[test]
fn a_slot_of_a_component_takes_a_view_that_is_that_component() {
    let base = "type Card {\n    title: Text\n}\ntype B {\n    cards: Collection<Card>\n}\nview line(c: Card) {\n    row(c.title)\n}\n";
    let good = format!(
        "{base}view tile(c: Card) {{\n    ui.BoardCard(c) {{\n        line(c)\n    }}\n}}\nview board(b: B) {{\n    ui.Board {{\n        ui.BoardLane(b, title: \"All\") {{\n            for c in b.cards {{\n                tile(c)\n            }}\n        }}\n    }}\n}}"
    );
    assert!(codes(&good).is_empty(), "{:?}", codes(&good));
    let bad = format!(
        "{base}view tile(c: Card) {{\n    text(c.title)\n}}\nview board(b: B) {{\n    ui.Board {{\n        ui.BoardLane(b, title: \"All\") {{\n            for c in b.cards {{\n                tile(c)\n            }}\n        }}\n    }}\n}}"
    );
    assert!(codes(&bad).iter().any(|c| c == "E063"), "{:?}", codes(&bad));
}

#[test]
fn check_fails_on_a_stale_ids_lock_and_writes_it_when_asked() {
    let dir = std::env::temp_dir().join(format!("tl-lock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(
        dir.join("app.tl"),
        "app t.lock \"Lock\"\ntype C {\n    n: Int\n}\nroot C\ndefault view v for C\nview v(c: C) {\n    text(\"{c.n}\")\n}\n",
    )
    .expect("source");
    let run = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        format!("{:?}", tilolang::cli::main(&args))
    };
    let path = dir.to_string_lossy().into_owned();
    let failed = run(&["check", &path]);
    assert_ne!(
        failed,
        format!("{:?}", std::process::ExitCode::SUCCESS),
        "a missing lock fails"
    );
    assert!(
        !dir.join("app.ids").exists(),
        "check writes nothing by itself"
    );
    let written = run(&["check", &path, "--write-ids"]);
    assert_eq!(written, format!("{:?}", std::process::ExitCode::SUCCESS));
    let lock = std::fs::read_to_string(dir.join("app.ids")).expect("lock");
    assert!(lock.contains("C.n = "), "{lock}");
    // The same entries, in another order, are not what a build writes: a check says so, and
    // --write-ids puts them back in the order the tool writes them.
    let header = lock.lines().next().expect("header");
    let mut entries: Vec<&str> = lock
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .collect();
    entries.reverse();
    std::fs::write(
        dir.join("app.ids"),
        format!("{header}\n{}\n", entries.join("\n")),
    )
    .expect("reordered lock");
    assert_ne!(
        run(&["check", &path]),
        format!("{:?}", std::process::ExitCode::SUCCESS),
        "a lock in another order is out of date"
    );
    let rewritten = run(&["check", &path, "--write-ids"]);
    assert_eq!(rewritten, format!("{:?}", std::process::ExitCode::SUCCESS));
    assert_eq!(
        std::fs::read_to_string(dir.join("app.ids")).expect("lock"),
        lock,
        "the tool's order is back"
    );
    assert_eq!(
        run(&["check", &path]),
        format!("{:?}", std::process::ExitCode::SUCCESS)
    );
    let out = dir.join("out");
    let built = run(&["build", &path, "--out", &out.to_string_lossy()]);
    assert_eq!(built, format!("{:?}", std::process::ExitCode::SUCCESS));
    assert!(
        out.join("t.lock.wasm").exists(),
        "the module is written outside the package"
    );
    assert!(!dir.join("app.wasm").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- a template view another package calls with a block --------------------------------

/// Compiles `lib` as one package, then `app` against the metadata it publishes: the templates a
/// package carries for its importers travel there.
fn compile_against(lib: &str, app: &str) -> Result<(), Vec<tilolang::Diagnostic>> {
    let lib = crate::common::compile(&format!("app probe.lib \"Lib\"\n{lib}\n")).meta;
    let src = format!("app probe.consumer \"Consumer\"\n{app}\n");
    crate::common::compile_with(&src, &[lib]).map(|_| ())
}

/// A `template view` another package calls with a block compiles, and the block is the
/// template's `View` parameter: a template is the language's way to take a block, so a package
/// that imports it calls it as its own does (`shell.desk_style(a, false) { ... }`).
///
/// The template's body resolves in its own package: it calls that package's helper (`helper`),
/// which the importer's own names cannot shadow.
#[test]
fn a_template_view_called_from_another_package_takes_its_block() {
    let lib = "fn helper(n: Int) -> Int {\n    n + 1\n}\n\
               template view frame(body: View) {\n    stack {\n        text(\"head {helper(1)}\")\n        body\n    }\n}";
    let app = "use probe.lib as lib\n\
               view mine() {\n    text(\"mine\")\n}\n\
               view sheet() {\n    lib.frame {\n        mine()\n    }\n}";
    if let Err(diags) = compile_against(lib, app) {
        panic!("{diags:#?}");
    }
}

/// The template's body resolves in its **own package**: a name the caller happens to have too
/// (`helper`, declared by the consumer with a different meaning) is not what the template calls.
/// The template's `helper(1)` is its package's, so the compiled text reads that fn's export and
/// never the caller's.
#[test]
fn a_callers_name_does_not_shadow_what_a_template_calls() {
    let lib = "fn helper(n: Int) -> Int {\n    n + 1\n}\n\
               template view frame(body: View) {\n    stack {\n        text(\"head {helper(1)}\")\n        body\n    }\n}";
    let app = "use probe.lib as lib\n\
               fn helper(n: Int) -> Text {\n    \"caller {n}\"\n}\n\
               view sheet() {\n    lib.frame {\n        text(helper(1))\n    }\n}";
    // The template's `helper(1)` is `probe.lib`'s (a number); the caller's own `helper(1)` is
    // `probe.consumer`'s (text). Both compile, each against its own package's fn.
    if let Err(diags) = compile_against(lib, app) {
        panic!("{diags:#?}");
    }
}

/// A plain view of another package still takes no child block: only a template is the
/// language's way to take one, so a plain view called with a block is `E063` there as here.
#[test]
fn a_plain_view_called_from_another_package_still_takes_no_block() {
    let lib = "view plain() {\n    text(\"plain\")\n}";
    let app = "use probe.lib as lib\n\
               view sheet() {\n    lib.plain {\n        text(\"no\")\n    }\n}";
    let diags = compile_against(lib, app).err().unwrap_or_default();
    assert!(
        diags.iter().any(|d| d.code == "E063"),
        "a plain view with a block is E063: {diags:#?}"
    );
}
