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
        "app t.x \"X\"\n{TALK}view talk(t: Talk) {{\n    lens draft: Text\n    std.transcript(t, t.lines, row: line) {{\n        composer: {{\n            row {{\n                input(bind: draft, fill)\n                button(send(t, draft).then(set(draft, \"\")), icon_only: true)\n            }}\n        }}\n    }}\n}}\n"
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
    let src = "app t.x \"X\"\ntype A {\n    a: Text\n}\ntype B {\n    b: Text\n}\ntype Both = Collection<A | B>\nroot both: Both\ndefault view shown for both\nview shown(all: Both) {\n    text(\"{all.len()}\")\n}\naction add_a(all: Both) -> A {\n    all.insert(A(a: \"x\"))\n}\naction named(all: Both) -> Text {\n    let b = all.insert(B(b: \"y\"))\n    b.b\n}\n";
    let compiled = crate::common::compile_with(src, &[]).unwrap_or_else(|d| panic!("{d:#?}"));
    validate(&compiled.wasm);
}

#[test]
fn an_item_block_names_the_other_slots_beside_the_row() {
    // The row sees the item and the view's state; the composer beside it sees the state only.
    let src = format!(
        "app t.x \"X\"\n{TALK}view said(t: Talk, l: Line, draft: State<Text>) {{\n    text(l.said)\n}}\nview talk(t: Talk) {{\n    lens draft: Text\n    std.transcript(t, t.lines) {{ l ->\n        said(t, l, draft)\n        composer: {{\n            input(bind: draft, fill)\n        }}\n    }}\n}}\n"
    );
    let compiled = crate::common::compile_with(&src, &[]).unwrap_or_else(|d| panic!("{d:#?}"));
    validate(&compiled.wasm);
    let item_in_composer = format!(
        "{TALK}view talk(t: Talk) {{\n    lens draft: Text\n    std.transcript(t, t.lines) {{ l ->\n        text(l.said)\n        composer: {{\n            text(l.said)\n        }}\n    }}\n}}"
    );
    assert!(
        !codes(&item_in_composer).is_empty(),
        "the composer does not see the item"
    );
}

#[test]
fn an_action_a_view_applies_takes_its_view_state_as_it_is_at_the_press() {
    // Given as itself, view state is read when the event fires.
    let pressed = format!(
        "app t.x \"X\"\n{TALK}view talk(t: Talk) {{\n    lens draft: Text\n    lens n: Int = 1\n    stack {{\n        input(bind: draft)\n        button(send(t, draft).then(set(draft, \"\")))\n        button(\"More\", on: set(n, n))\n    }}\n}}"
    );
    let compiled = crate::common::compile_with(&pressed, &[]).unwrap_or_else(|d| panic!("{d:#?}"));
    validate(&compiled.wasm);
    // An argument computed from view state is a function of the module, run at the press on
    // the view state as it is then: a toggle, a count, a draft with more around it.
    let computed = format!(
        "app t.x \"X\"\n{TALK}view talk(t: Talk) {{\n    lens draft: Text\n    lens open: Bool = false\n    lens n: Int = 1\n    stack {{\n        input(bind: draft)\n        button(send(t, \"{{draft}}!\"))\n        button(\"Toggle\", on: set(open, not open))\n        button(\"More\", on: set(n, n + 1))\n    }}\n}}"
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
        "{TALK}view talk(t: Talk) {{\n    lens draft: Text\n    button(send(t, said(t, draft)))\n}}\naction said(t: Talk, d: Text) -> Text {{\n    d\n}}"
    );
    assert!(
        codes(&acting).iter().any(|c| c == "E043"),
        "{:?}",
        codes(&acting)
    );
    // A flow or an action reads its own arguments as they are.
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
    let base =
        "type Card {\n    title: Text (title)\n}\ntype B {\n    cards: Collection<Card>\n}\n";
    let good = format!(
        "{base}view tile(c: Card) {{\n    ui.BoardCard(c, title: c.title)\n}}\nview board(b: B) {{\n    ui.Board {{\n        ui.BoardLane(b, title: \"All\") {{\n            for c in b.cards {{\n                tile(c)\n            }}\n        }}\n    }}\n}}"
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
