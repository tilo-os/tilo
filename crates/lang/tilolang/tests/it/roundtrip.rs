use crate::common::fixture;
use tilolang::{parse, print_module};

const DEMO: &str = r#"app so.tilo.demo "Demo \"quoted\""

use so.tilo.todo as todo
use so.tilo.other

/// A record.
type T {
    /// A counter.
    a: Int merge counter = -1
    b: Float = 1.5
    c: Text merge register = "x\n\{y\}"
    d: Option<Int> = none
    e: List<(Int, Text)>
}

fn calc(x: Int, y: Int) -> Int {
    let z: Int = (x + y) * 2 - -x % 3
    if z > 1 and not (y < 2) or x == 0 {
        return z
    } else if z == 5 {
        return 5
    } else {
        return if x > 1 then x else y
    }
}

flow f(l: todo.TodoList) {
    let m = [1, 2].map(i -> i + 1)
    do todo.add(l, "a {m.len()} b {"nested {1}"}")
    let r = @todo.TodoList:01J8ZK4T
    let t = (1, "two", 3.0e2)
}

action a(l: T) -> Int proposal v(l) {
    for i in [1, 2, 3] {
        l.a += i
    }
    return 1
}

view v(l: todo.TodoList) {
    column {
        for i in l.items {
            if i.done {
                text("done")
            } else {
                text(i.title) #t
            }
        }
        list(l.items) { i -> text(i.title) }
    }
}
"#;

fn print_of(src: &str) -> String {
    print_module(&parse(src).unwrap_or_else(|d| panic!("{d:#?}")))
}

#[test]
fn printing_is_idempotent_on_the_demo() {
    let once = print_of(DEMO);
    let twice = print_of(&once);
    assert_eq!(once, twice);
}

#[test]
fn printed_text_parses_to_the_same_tree() {
    let a = parse(DEMO).expect("demo parses");
    let b = parse(&print_module(&a)).expect("printed demo parses");
    assert_eq!(print_module(&a), print_module(&b));
    assert_eq!(a.items.len(), b.items.len());
}

#[test]
fn fixtures_are_canonical() {
    for name in ["counter.tl", "todo.tl", "cleanup.tl"] {
        let src = fixture(name);
        assert_eq!(print_of(&src), src, "{name} is not in canonical form");
    }
}

#[test]
fn precedence_and_parentheses_survive() {
    let src = "app t.x \"X\"\n\nfn f(a: Int, b: Int) -> Int {\n    return (a + b) * (a - b) / -(a % b)\n}\n";
    assert_eq!(print_of(src), src);
}

#[test]
fn doc_comments_are_kept() {
    let out = print_of(&fixture("counter.tl"));
    assert!(out.contains("/// Add one to the counter."));
    assert!(out.contains("/// The current value."));
}

#[test]
fn comments_are_dropped_and_continuation_lines_join() {
    let src = "app t.x \"X\"\n// gone\nfn f(l: List<Int>) -> Int {\n    return l\n        .filter(x -> x > 1)\n        .count(x -> true)\n}\n";
    let out = print_of(src);
    assert!(
        out.contains("return l.filter(x -> x > 1).count(x -> true)"),
        "{out}"
    );
    assert!(!out.contains("gone"));
}

#[test]
fn every_declaration_and_ui_call_has_an_id_and_a_span() {
    let m = parse(&fixture("todo.tl")).expect("parses");
    let mut ids = std::collections::BTreeSet::new();
    for item in &m.items {
        match item {
            tilolang::ast::Item::Type(t) => {
                assert!(ids.insert(t.node));
                for f in &t.fields {
                    assert!(ids.insert(f.node));
                    assert!(f.span.end > f.span.start);
                }
            }
            tilolang::ast::Item::Action(a) => {
                assert!(ids.insert(a.node) && a.span.end > a.span.start)
            }
            tilolang::ast::Item::Fn(f) => assert!(ids.insert(f.node)),
            tilolang::ast::Item::View(v) => assert!(ids.insert(v.node)),
            _ => {}
        }
    }
    assert!(ids.len() > 8);
}
