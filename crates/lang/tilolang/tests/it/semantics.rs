//! Merge rules, effect discipline, effect classes and capability sets.

use crate::common::{codes, compile_body as compile, compile_fixture, compile_with, todo_meta};
use tilolang::types::{EffectClass, MergeKind};

// USER REQUIREMENT (2026-10-20): "The compiler rejects mutations that would lose intent, for
// example `+=` on a `register`."
#[test]
fn counter_takes_plus_equals_and_register_does_not() {
    let ok =
        "type C {\n    n: Int merge counter\n}\naction a(c: C) {\n    c.n += 1\n    c.n -= 2\n}";
    assert!(codes(ok).is_empty());
    let bad = "type C {\n    n: Int\n}\naction a(c: C) {\n    c.n += 1\n}";
    assert_eq!(codes(bad), vec!["E030"]);
}

#[test]
fn writing_the_read_and_write_out_is_allowed_on_a_register() {
    assert!(codes("type C {\n    n: Int\n}\naction a(c: C) {\n    c.n = c.n + 1\n}").is_empty());
}

#[test]
fn text_assignment_is_a_splice_and_a_register_text_is_an_atom() {
    let c = compile(
        "type T {\n    body: Text\n    tag: Text merge register\n}\naction a(t: T) {\n    t.body = \"x\"\n    t.tag = \"y\"\n}",
    );
    let fields = &c.meta.types[0].fields;
    assert_eq!(fields[0].merge, MergeKind::Text);
    assert_eq!(fields[1].merge, MergeKind::Register);
}

#[test]
fn a_view_cannot_mutate() {
    let src = "type C {\n    items: Collection<C>\n}\nview v(c: C) {\n    let x = c.items.insert(C())\n    text(\"x\")\n}";
    assert_eq!(codes(src), vec!["E040"]);
}

#[test]
fn a_flow_cannot_mutate_fields_directly() {
    let src = "type C {\n    n: Int\n}\nflow f(c: C) {\n    c.n = 1\n}";
    assert_eq!(codes(src), vec!["E040"]);
}

#[test]
fn an_action_cannot_call_a_foreign_action() {
    let got = crate::common::codes_with(
        "use so.tilo.todo as todo\naction a(l: todo.TodoList, t: todo.Todo) {\n    todo.remove(l, t)\n}",
        &[todo_meta()],
    );
    assert_eq!(got, vec!["E041"]);
}

#[test]
fn an_action_may_call_its_own_actions() {
    let src = "type C {\n    n: Int merge counter\n}\naction inner(c: C) {\n    c.n += 1\n}\naction outer(c: C) {\n    inner(c)\n    inner(c)\n}";
    let c = compile(src);
    let outer = c
        .meta
        .actions
        .iter()
        .find(|a| a.name == "outer")
        .expect("outer");
    assert_eq!(outer.class, EffectClass::Undoable);
    assert!(outer.reaches.iter().any(|r| r.name == "inner" && r.own));
}

#[test]
fn effect_classes_are_derived() {
    let src = "type C {\n    n: Int merge counter\n    t: Text\n}\n\
        action peek(c: C) -> Int {\n    return c.n\n}\n\
        action bump(c: C) {\n    c.n += 1\n}\n\
        action send(c: C) {\n    outside(\"post\", c.t)\n}\n\
        action both(c: C) {\n    bump(c)\n    send(c)\n}\n\
        action indirect(c: C) {\n    peek(c)\n}";
    let c = compile(src);
    let class = |n: &str| {
        c.meta
            .actions
            .iter()
            .find(|a| a.name == n)
            .expect("action")
            .class
    };
    assert_eq!(class("peek"), EffectClass::Read);
    assert_eq!(class("bump"), EffectClass::Undoable);
    assert_eq!(class("send"), EffectClass::Outside);
    assert_eq!(class("both"), EffectClass::Outside);
    assert_eq!(class("indirect"), EffectClass::Read);
}

#[test]
fn a_view_reaches_the_actions_its_buttons_apply() {
    let c = compile_fixture("counter.tl", &[]);
    let inc = c
        .meta
        .actions
        .iter()
        .find(|a| a.name == "increment")
        .expect("increment");
    assert_eq!(inc.class, EffectClass::Undoable);
    let view = &c.meta.views[0];
    assert_eq!(view.reaches.len(), 1);
    assert_eq!(view.reaches[0].action, inc.id);
    assert!(view.reaches[0].own);
    assert_eq!(view.reaches[0].class, EffectClass::Undoable);
    assert_eq!(view.anchors.len(), 1);
    assert_eq!(view.anchors[0].name, "plus");
}

#[test]
fn a_flow_reaches_foreign_actions_with_their_class() {
    let todo = todo_meta();
    let c = compile_with(
        &crate::common::fixture("cleanup.tl"),
        std::slice::from_ref(&todo),
    )
    .expect("compiles");
    let flow = &c.meta.flows[0];
    let remove = todo
        .actions
        .iter()
        .find(|a| a.name == "remove")
        .expect("remove");
    assert_eq!(flow.reaches.len(), 1);
    assert_eq!(flow.reaches[0].action, remove.id);
    assert_eq!(flow.reaches[0].app, "so.tilo.todo");
    assert!(!flow.reaches[0].own);
    assert_eq!(flow.class, EffectClass::Undoable);
    assert_eq!(flow.result.as_deref(), Some("Int"));
    // The view applies the flow as an action; the flow is own.
    let view = &c.meta.views[0];
    assert!(view.reaches.iter().any(|r| r.name == "clear_done" && r.own));
}

#[test]
fn the_view_of_the_todo_app_reaches_its_actions() {
    let c = compile_fixture("todo.tl", &[]);
    let view = &c.meta.views[0];
    let names: Vec<&str> = view.reaches.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&"remove") && names.contains(&"add"),
        "{names:?}"
    );
}

#[test]
fn own_and_foreign_types_keep_their_apps() {
    let todo = todo_meta();
    let c = compile_with(&crate::common::fixture("cleanup.tl"), &[todo]).expect("compiles");
    let param = &c.meta.flows[0].params[0];
    assert_eq!(param.ty, "so.tilo.todo.TodoList");
}

#[test]
fn lambdas_are_inlined_and_restricted() {
    let ok = "fn f(l: List<Int>) -> Int {\n    let ys = l.filter(x -> x > 1).map(x -> x * 2)\n    return ys.count(x -> x > 2)\n}\nfn g(l: List<Int>) -> Bool {\n    return l.any(x -> x == 1) and l.all(x -> x > 0)\n}\nfn h(l: List<Int>) -> Option<Int> {\n    return l.find(x -> x == 2)\n}";
    assert!(codes(ok).is_empty());
    assert_eq!(
        codes("fn f() -> Int {\n    let g = x -> x\n    return 1\n}"),
        vec!["E050"]
    );
}

#[test]
fn options_records_and_text_interpolation_check() {
    let src = "type T {\n    name: Text\n    n: Int\n    o: Option<Int>\n}\nfn f(t: T) -> Text {\n    let x: Option<Int> = .none\n    if t.o == .none {\n        let shown = match t.o {\n            .some(v) => v,\n            .none => 3,\n        }\n        return \"none {t.n} {t.name} {shown}\"\n    }\n    return \"{x == .some(2)} {t == t}\"\n}";
    assert!(codes(src).is_empty(), "{:?}", codes(src));
}

// USER REQUIREMENT (2026-10-20): "Remove the old `none` literal, `is_some`, `.value`/unwrap-style
// access from the surface language".
#[test]
fn options_are_taken_apart_only_by_match() {
    let head = "type T {\n    o: Option<Int>\n}\n";
    for body in [
        "fn f(t: T) -> Bool {\n    return t.o.is_some()\n}",
        "fn f(t: T) -> Int {\n    return t.o.unwrap()\n}",
        "fn f(t: T) -> Int {\n    return t.o.unwrap_or(1)\n}",
        "fn f(t: T) -> Int {\n    return t.o.value\n}",
    ] {
        let got = codes(&format!("{head}{body}"));
        assert!(!got.is_empty(), "accepted: {body}");
    }
    assert_eq!(
        codes("fn f() -> Option<Int> {\n    return none\n}"),
        vec!["E065"],
        "`none` is no longer a literal: it is the variant `.none`"
    );
}

const SHAPES: &str = "enum Shape {\n    dot\n    line(len: Int)\n    box(w: Int, h: Int)\n}\n";

// USER REQUIREMENT (2026-10-20): "Sum types: `enum Name { a, b(x: T, y: U), c }` with payload
// fields; values written as dot literals `.a`, `.b(x: 1, y: \"z\")`, resolved from the expected
// type (and `Name.a` when there is no expected type)."
#[test]
fn enums_build_from_dot_literals_and_match_takes_them_apart() {
    let src = format!(
        "{SHAPES}type Pic {{\n    shape: Shape\n    last: Option<Shape>\n}}\nfn area(s: Shape) -> Int {{\n    match s {{\n        .dot => 0,\n        .line(n) => n,\n        .box(w, h) => {{\n            let a = w * h\n            a\n        }}\n    }}\n}}\nfn make(big: Bool) -> Shape {{\n    if big {{ .box(w: 2, h: 3) }} else {{ .line(len: 1) }}\n}}\nfn named() -> Int {{\n    let s = Shape.box(w: 1, h: 1)\n    let d = Shape.dot\n    if s == d {{\n        return 0\n    }}\n    area(s)\n}}\naction draw(p: Pic) {{\n    p.last = .some(p.shape)\n    p.shape = .dot\n}}"
    );
    assert!(codes(&src).is_empty(), "{:?}", codes(&src));
}

#[test]
fn an_enum_field_is_a_register_with_its_first_variant_as_default() {
    let c = crate::common::compile_body(&format!(
        "{SHAPES}type Pic {{\n    shape: Shape\n    other: Shape = .line(len: 3)\n}}"
    ));
    let pic = c.meta.types.iter().find(|t| t.name == "Pic").expect("Pic");
    let shape = &pic.fields[0];
    assert_eq!(shape.ty, "enum Shape");
    assert_eq!(shape.merge, tilolang::types::MergeKind::Register);
    let default = shape.default.clone().expect("a default");
    assert_eq!(default["variant"], c.ids.get("Shape.dot").expect("an id"));
    let other = pic.fields[1].default.clone().expect("a default");
    let len = c.ids.get("Shape.line.len").expect("an id");
    assert_eq!(other["fields"][len], 3);
    assert!(
        !codes("enum E {\n    a(x: Int)\n}\ntype T {\n    e: E\n}").is_empty(),
        "a first variant with payload needs an explicit default"
    );
    let e = &c.meta.enums[0];
    assert_eq!(e.variants.len(), 3);
    assert_eq!(e.variants[2].fields[1].name, "h");
}

// USER REQUIREMENT (2026-10-20): "One `if`: `if cond { ... } else { ... }` is an expression whose
// value is the block's last expression (also usable as a statement and inside view blocks,
// where branches yield UI). Remove `if ... then ... else ...`. Blocks as expressions in general
// (last expression is the value)."
#[test]
fn if_and_blocks_are_expressions() {
    let src = "fn pick(b: Bool) -> Text {\n    let t = if b { \"yes\" } else { \"no\" }\n    let n = {\n        let a = 2\n        a * 2\n    }\n    if n > 3 {\n        t\n    } else {\n        \"small\"\n    }\n}\nview v(b: Bool) {\n    if b {\n        text(\"a\")\n    } else if not b {\n        text(\"b\")\n    } else {\n        text(\"c\")\n    }\n    text(if b { \"x\" } else { \"y\" })\n}";
    assert!(codes(src).is_empty(), "{:?}", codes(src));
    assert_eq!(
        codes("fn f(b: Bool) -> Int {\n    return if b then 1 else 2\n}"),
        vec!["E003"],
        "`if ... then ... else` is gone"
    );
    assert_eq!(
        codes("fn f(b: Bool) -> Int {\n    if b { 1 } else { \"x\" }\n}"),
        vec!["E020"]
    );
}

// USER REQUIREMENT (2026-10-20): "Flows: remove the `do` keyword; plain `act(args)` invokes."
// and "`try act(args)` in flows returns `Result<T, Text>` (unit actions: `Result<(), Text>`)".
#[test]
fn flows_invoke_by_calling_and_try_gives_a_result() {
    let src = "type L {\n    n: Int merge counter\n}\naction bump(l: L) {\n    l.n += 1\n}\naction size(l: L) -> Int {\n    l.n\n}\nflow f(l: L) -> Text {\n    bump(l)\n    let r: Result<(), Text> = try bump(l)\n    let s: Result<Int, Text> = try size(l)\n    match s {\n        .ok(v) => \"{v}\",\n        .err(why) => match r {\n            .ok => why,\n            .err(e) => e,\n        },\n    }\n}";
    assert!(codes(src).is_empty(), "{:?}", codes(src));
    assert_eq!(
        codes("type L {\n    n: Int\n}\naction a(l: L) {\n}\nflow f(l: L) {\n    do a(l)\n}"),
        vec!["E003"]
    );
}

// USER REQUIREMENT (2026-10-20): "Anchors: `button(...) as plus` instead of postfix `#plus`."
#[test]
fn anchors_are_written_with_as() {
    let c = crate::common::compile_body(
        "type C {\n    n: Int merge counter\n}\naction inc(c: C) {\n    c.n += 1\n}\nview v(c: C) {\n    button(\"+1\", on: inc(c)) as plus\n}",
    );
    assert_eq!(c.meta.views[0].anchors[0].name, "plus");
    assert_eq!(
        codes("type C {\n    n: Int\n}\nview v(c: C) {\n    text(\"x\") #plus\n}"),
        vec!["E003"]
    );
}

// USER REQUIREMENT (2026-10-20): package-level `root T` and `default view v for T`
// (workspace-model.md sections 2 and 7) "in the compiler and metadata".
#[test]
fn roots_and_default_views_are_in_the_metadata() {
    let c = crate::common::compile_body(
        "type Book {\n    title: Text\n}\ntype Shelf {\n    books: Collection<Book>\n}\nroot Shelf\ndefault view shelf for Shelf\ndefault view book for Book\nview shelf(s: Shelf) {\n    for b in s.books {\n        button(b.title, on: open(b))\n    }\n}\nview book(b: Book) {\n    text(b.title)\n}\nview cover(b: Book) {\n    button(b.title, on: open(b, view: book))\n}",
    );
    let id = |name: &str| c.ids.get(name).expect("an id").to_string();
    assert_eq!(c.meta.roots, vec![id("Shelf")]);
    assert_eq!(
        c.meta.default_view(&id("Shelf")),
        Some(id("shelf").as_str())
    );
    assert_eq!(c.meta.default_view(&id("Book")), Some(id("book").as_str()));
}

// USER REQUIREMENT (2026-10-20): view state per workspace-model.md: "inside a view body,
// `window name: T = default` (shared per window) and `lens name: T = default` (per person per
// window) declarations, readable in the view; changed only via bindings (`bind: name`) or a
// platform action reference `set(name, value)` usable in `on:`."
#[test]
fn view_state_is_declared_read_bound_and_set() {
    let c = crate::common::compile_body(
        "enum Order {\n    recent\n    title\n}\nview v() {\n    window order: Order = .recent\n    lens query: Text\n    lens open: Bool = false\n    stack {\n        input(bind: query)\n        checkbox(\"Open\", bind: open)\n        button(\"By title\", on: set(order, .title))\n        if order == .title {\n            text(query)\n        }\n    }\n}",
    );
    crate::common::validate(&c.wasm);
    let state = &c.meta.views[0].state;
    let names: Vec<(&str, &str)> = state
        .iter()
        .map(|s| (s.name.as_str(), s.layer.as_str()))
        .collect();
    assert_eq!(
        names,
        vec![("order", "window"), ("query", "lens"), ("open", "lens")]
    );
    assert_eq!(state[0].ty, "enum Order");
    assert_eq!(
        codes(
            "view v() {\n    window n: Int = 0\n    text(\"{n}\")\n}\nfn f() -> Int {\n    return n\n}"
        ),
        vec!["E014"],
        "view state is the view's own"
    );
}
