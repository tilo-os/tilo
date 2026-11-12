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

// A register written from itself recomputes wherever the change is applied, so the read and the
// write written out are refused as `+=` is (convergence design 1.1); `outcomes.rs` has the rest.
#[test]
fn writing_the_read_and_write_out_is_refused_on_a_register() {
    assert_eq!(
        codes("type C {\n    n: Int\n}\naction a(c: C) {\n    c.n = c.n + 1\n}"),
        vec!["E039"]
    );
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
fn a_view_presses_a_foreign_action_on_a_resource_named_by_its_id() {
    let got = crate::common::codes_with(
        "use so.tilo.todo as todo\ntype S {\n    key: Text\n}\nview v(s: S) {\n    \
         ui.Button(\"Done\") {\n        on press: todo.toggle(resource(s.key, of: todo.Todo))\n    }\n}",
        &[todo_meta()],
    );
    assert_eq!(got, Vec::<String>::new());
}

#[test]
fn resource_of_takes_another_packages_type() {
    let own = crate::common::codes_with(
        "type S {\n    key: Text\n}\nfn f(s: S) -> S {\n    return resource(s.key, of: S)\n}",
        &[],
    );
    assert_eq!(own, vec!["E022"]);
    let typed = crate::common::codes_with(
        "use so.tilo.todo as todo\nfn f(id: Text) -> todo.TodoList {\n    return resource(id, of: todo.Todo)\n}",
        &[todo_meta()],
    );
    assert_eq!(
        typed,
        vec!["E020"],
        "a `todo.Todo` is not a `todo.TodoList`"
    );
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
fn an_action_reaches_foreign_actions_with_their_class() {
    let todo = todo_meta();
    let c = compile_with(
        &crate::common::fixture("cleanup.tl"),
        std::slice::from_ref(&todo),
    )
    .expect("compiles");
    let action = &c.meta.actions[0];
    let remove = todo
        .actions
        .iter()
        .find(|a| a.name == "remove")
        .expect("remove");
    assert_eq!(action.reaches.len(), 2);
    let reached = action
        .reaches
        .iter()
        .find(|r| r.action == remove.id)
        .expect("the todo app's remove");
    assert_eq!(reached.app, "so.tilo.todo");
    assert!(!reached.own);
    assert!(
        action
            .reaches
            .iter()
            .any(|r| r.name == "clear_done" && r.own),
        "and itself"
    );
    assert_eq!(action.class, EffectClass::Undoable);
    assert_eq!(action.result.as_deref(), Some("Int"));
    // The view applies the action; the action is own.
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
    let param = &c.meta.actions[0].params[0];
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

const SHAPES: &str = "enum Shape {\n    dot\n    line\n    box\n}\n";

// An enum's cases are plain names (cases that hold data are types of their own, joined in a
// union: `unions.rs`). Values are dot literals resolved from the expected type, or `Shape.dot`.
#[test]
fn enums_build_from_dot_literals_and_match_takes_them_apart() {
    let src = format!(
        "{SHAPES}type Pic {{\n    shape: Shape\n    last: Option<Shape>\n}}\nfn area(s: Shape) -> Int {{\n    match s {{\n        .dot => 0,\n        .line => 1,\n        .box => {{\n            let a = 2 * 2\n            a\n        }}\n    }}\n}}\nfn make(big: Bool) -> Shape {{\n    if big {{ .box }} else {{ .line }}\n}}\nfn named() -> Int {{\n    let s = Shape.box\n    let d = Shape.dot\n    if s == d {{\n        return 0\n    }}\n    area(s)\n}}\naction draw(p: Pic) {{\n    p.last = .some(p.shape)\n    p.shape = .dot\n}}"
    );
    assert!(codes(&src).is_empty(), "{:?}", codes(&src));
}

#[test]
fn an_enum_field_is_a_register_with_its_first_variant_as_default() {
    let c = crate::common::compile_body(&format!(
        "{SHAPES}type Pic {{\n    shape: Shape\n    other: Shape = .line\n}}"
    ));
    let pic = c.meta.types.iter().find(|t| t.name == "Pic").expect("Pic");
    let shape = &pic.fields[0];
    assert_eq!(shape.ty, "enum Shape");
    assert_eq!(shape.merge, tilolang::types::MergeKind::Register);
    let default = shape.default.clone().expect("a default");
    assert_eq!(default["variant"], c.ids.get("Shape.dot").expect("an id"));
    let other = pic.fields[1].default.clone().expect("a default");
    assert_eq!(other["variant"], c.ids.get("Shape.line").expect("an id"));
    assert_eq!(c.meta.enums[0].variants.len(), 3);
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

// An action calls another by naming it, and `try` gives the call's `Result`.
#[test]
fn actions_invoke_by_calling_and_try_gives_a_result() {
    let src = "type L {\n    n: Int merge counter\n}\naction bump(l: L) {\n    l.n += 1\n}\naction size(l: L) -> Int {\n    l.n\n}\naction f(l: L) -> Text {\n    bump(l)\n    let r: Result<(), Text> = try bump(l)\n    let s: Result<Int, Text> = try size(l)\n    match s {\n        .ok(v) => \"{v}\",\n        .err(why) => match r {\n            .ok => why,\n            .err(e) => e,\n        },\n    }\n}";
    assert!(codes(src).is_empty(), "{:?}", codes(src));
    assert_eq!(
        codes("type L {\n    n: Int\n}\naction a(l: L) {\n}\naction f(l: L) {\n    do a(l)\n}"),
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
// (workspace-model.md, "Apps") "in the compiler and metadata".
#[test]
fn roots_and_default_views_are_in_the_metadata() {
    let c = crate::common::compile_body(
        "type Book {\n    title: Text\n}\ntype Shelf {\n    books: Collection<Book>\n}\nroot Shelf\ndefault view shelf for Shelf\ndefault view book for Book\nview shelf(s: Shelf) {\n    std.page(s) {\n        for b in s.books {\n            button(b.title, on: open(b))\n        }\n    }\n}\nview book(b: Book) {\n    std.document(b) {\n        text(b.title)\n    }\n}\nview cover(b: Book) {\n    button(b.title, on: open(b, view: book))\n}",
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
// `state name: T = default` (per person per window) declarations, readable in the view;
// changed only via bindings (`bind: name`) or a platform action reference
// `set(name, value)` usable in `on:`."
#[test]
fn view_state_is_declared_read_bound_and_set() {
    let c = crate::common::compile_body(
        "enum Order {\n    recent\n    title\n}\nview v() {\n    state order: Order = .recent\n    state query: Text\n    state open: Bool = false\n    stack {\n        input(bind: query)\n        checkbox(\"Open\", bind: open)\n        button(\"By title\", on: set(order, .title))\n        if order == .title {\n            text(query)\n        }\n    }\n}",
    );
    crate::common::validate(&c.wasm);
    let state = &c.meta.views[0].state;
    let names: Vec<&str> = state.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["order", "query", "open"]);
    assert_eq!(state[0].ty, "enum Order");
    assert_eq!(
        codes(
            "view v() {\n    state n: Int = 0\n    text(\"{n}\")\n}\nfn f() -> Int {\n    return n\n}"
        ),
        vec!["E014"],
        "view state is the view's own"
    );
}

/// A template's text parameter is words the view shows, as a component's prop of words is: the
/// literal a caller gives it is the caller's message, in its catalogue under the view that
/// writes it, its interpolation an argument (i18n.md §2).
#[test]
fn a_templates_text_parameter_carries_its_callers_message() {
    let c = compile(
        "type Talk {\n    topic: Text\n}\nroot Talk\ndefault view talk for Talk\nview talk(t: Talk) {\n    state draft: RichText = RichText(\"\")\n    std.composer(draft, \"Say something about {t.topic}\", set(draft, RichText(\"\")))\n}",
    );
    crate::common::validate(&c.wasm);
    let source = tilolang::messages::source_catalogue(&c.meta);
    assert!(
        source.contains(".w-say-something-about = Say something about"),
        "the caller's sentence is its message: {source}"
    );
}

/// Every word a person reads is a message: an action's label that follows its subject, why it
/// cannot run, and the words its body writes with `say` into a record, each under the action's
/// own message, its interpolation an argument (i18n.md §2).
#[test]
fn an_actions_label_its_reasons_and_what_it_says_are_messages() {
    let c = compile(
        "type Note {\n    title: Text\n    pinned: Bool\n}\ntype Book {\n    notes: Collection<Note>\n}\nroot Book\ndefault view book for Book\naction pin(n: Note, pinned: Bool = not n.pinned) (\n    label: if pinned { \"Pin\" } else { \"Unpin\" },\n) {\n    require n.title != \"\" \"Name the note first\"\n    n.pinned = pinned\n}\naction tally(b: Book) {\n    let n = b.notes.count(x -> x.pinned)\n    b.notes.insert(Note(title: say(\"{n} pinned\")))\n}\nview book(b: Book) {\n    for n in b.notes {\n        button(pin(n))\n    }\n}",
    );
    crate::common::validate(&c.wasm);
    let source = tilolang::messages::source_catalogue(&c.meta);
    let pin = tilolang::messages::key_of(c.ids.get("pin").expect("the action"));
    let tally = tilolang::messages::key_of(c.ids.get("tally").expect("the action"));
    for line in [
        format!("{pin} = Pin"),
        "    .w-pin = Pin".to_owned(),
        "    .w-unpin = Unpin".to_owned(),
        "    .w-name-the-note-first = Name the note first".to_owned(),
        format!("{tally} = Tally"),
        "    .w-pinned = { $n } pinned".to_owned(),
    ] {
        assert!(source.contains(&line), "{line:?} in {source}");
    }
    assert_eq!(
        codes("action a() {\n    let t = \"x\"\n    let s = say(t)\n}"),
        vec!["E022"],
        "`say` takes the words themselves"
    );
}

// A list's rows are a view of their own (`<view>~row<n>`), and a component called in a row that
// declares `state` keeps it there: the host binds the state by the id the row's
// body reads (`<row view id>:<component>.<state>`), so the row's view meta carries it.
#[test]
fn view_state_of_a_component_called_in_a_lists_row_is_the_rows() {
    let c = compile(
        "type Note {\n    title: Text\n}\ntype Book {\n    notes: Collection<Note>\n}\nroot Book\ndefault view book for Book\naction add(b: Book, title: Text) {\n    b.notes.insert(Note(title: title))\n}\nview book(b: Book) {\n    stack {\n        for n in b.notes {\n            text(n.title)\n            std.add_entry(\"New note\", add(b, \"\"))\n        }\n    }\n}",
    );
    crate::common::validate(&c.wasm);
    let view = c.ids.get("book").expect("the view");
    let row = c
        .meta
        .views
        .iter()
        .find(|v| v.id == format!("{view}~row0"))
        .unwrap_or_else(|| panic!("the row of the notes list: {:?}", c.meta.views));
    let ids: Vec<&str> = row.state.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec![format!("{}:add_entry.typed", row.id)]);
    assert_eq!(row.state[0].name, "typed");
}

// A view answers a request to show an item of a type it lists (`on show(x: T): ...`): the action
// it makes in its own state, run with the item. Anything else it answers is not a show, so a
// staging that asks a window to show a task reads the window's declaration, never a shape guessed
// from the data.
#[test]
fn a_view_answers_a_request_to_show_an_item_of_a_type_it_lists() {
    let c = compile(
        "type Note {\n    title: Text\n}\nview v(n: Note) {\n    state opened: Option<Note> = .none\n    \
         on show(x: Note): set(opened, x)\n    text(\"the list\")\n}",
    );
    crate::common::validate(&c.wasm);
    let show = c.meta.views[0]
        .show
        .as_ref()
        .expect("the view answers `on show`");
    assert_eq!(show.ty, c.meta.types[0].id);
    assert_eq!(show.func.params.len(), 1, "it is given the item alone");
    assert_eq!(show.func.params[0].name, "x");
    // A type that is no item of the app's, and a show that names none: both are refused.
    let int = "type Note {\n    title: Text\n}\naction ping(n: Note) {\n    n.title = \"p\"\n}\n\
               view v(n: Note) {\n    on show(x: Int): ping(n)\n    text(\"the list\")\n}";
    assert_eq!(codes(int), vec!["E063"]);
    let unnamed = "type Note {\n    title: Text\n}\naction ping(n: Note) {\n    n.title = \"p\"\n}\n\
                    view v(n: Note) {\n    on show: ping(n)\n    text(\"the list\")\n}";
    assert_eq!(codes(unnamed), vec!["E063"]);
    // The view's state is the live view's to start: a default that reads the view's own
    // parameter is no part of the frame that answers with the item.
    let held = compile(
        "type Note {\n    title: Text\n}\ntype Book {\n    notes: Collection<Note>\n}\n\
         root Book\ndefault view book for Book\n\
         view book(b: Book) {\n    state opened: Option<Note> = b.notes.first()\n    \
         on show(x: Note): set(opened, x)\n    text(\"the list\")\n}",
    );
    crate::common::validate(&held.wasm);
}
