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
    let c = compile_with(&crate::common::fixture("cleanup.tl"), &[todo.clone()]).expect("compiles");
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
    let src = "type T {\n    name: Text\n    n: Int\n    o: Option<Int>\n}\nfn f(t: T) -> Text {\n    let x: Option<Int> = none\n    if t.o == none {\n        return \"none {t.n} {t.name} {t.o.unwrap_or(3)}\"\n    }\n    return \"{x.is_some()} {t == t}\"\n}";
    assert!(codes(src).is_empty(), "{:?}", codes(src));
}
