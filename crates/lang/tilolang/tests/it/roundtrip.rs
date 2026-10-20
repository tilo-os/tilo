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
    d: Option<Int> = .none
    e: List<(Int, Text)>
    s: Shape = .dot
}

/// A sum type.
enum Shape {
    dot
    /// A line.
    line(len: Int)
    box(w: Int, h: Text)
}

root T

default view v for todo.TodoList

fn calc(x: Int, y: Int) -> Int {
    let z: Int = (x + y) * 2 - -x % 3
    if z > 1 and not (y < 2) or x == 0 {
        return z
    } else if z == 5 {
        return 5
    } else {
        return if x > 1 { x } else { y }
    }
}

fn area(s: Shape) -> Int {
    let k = {
        let base = 2
        base * 3
    }
    match s {
        .dot => 0,
        .line(n) => n * k,
        .box(w, _) => {
            let twice = w * 2
            twice
        }
    }
}

flow f(l: todo.TodoList) -> Result<(), Text> {
    let m = [1, 2].map(i -> i + 1)
    todo.add(l, "a {m.len()} b {"nested {1}"}")
    let r = @todo.TodoList:01J8ZK4T
    let t = (1, "two", 3.0e2)
    let s = Shape.box(w: 1, h: "x")
    try todo.rename(l, "n")
}

action a(l: T) -> Int proposal v(l) {
    for i in [1, 2, 3] {
        l.a += i
    }
    l.s = .line(len: 2)
    return 1
}

view v(l: todo.TodoList) {
    window order: Shape = .dot
    lens picked: Option<Int> = .none
    column(gap: .section, align: .center) {
        for i in l.items {
            if i.done {
                text("done", role: .caption)
            } else if picked == .some(1) {
                text("picked")
            } else {
                text(i.title) as t
            }
        }
        match order {
            .dot => text("dot"),
            .line(n) => text("{n}"),
            .box(w, h) => {
                text(h)
            }
        }
        button("Line", on: set(order, .line(len: 3)))
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
                assert!(ids.insert(a.node) && a.span.end > a.span.start);
            }
            tilolang::ast::Item::Fn(f) => assert!(ids.insert(f.node)),
            tilolang::ast::Item::View(v) => assert!(ids.insert(v.node)),
            _ => {}
        }
    }
    assert!(ids.len() > 8);
}

/// Every construct of the native vocabulary, in canonical form.
const NATIVE: &str = r#"app so.tilo.notes "Notes" (icon: .file_text, category: .office)

type Note {
    title: Text (title)
    body: Text merge text (body) = ""
    pinned: Bool = false
}

type Notebook {
    notes: Collection<Note>
}

enum NoteFilter {
    /// All notes
    all
    pinned
}

root Notebook

default view notebook for Notebook

row view note_row for Note

/// Start a note.
primary action new_note(nb: Notebook) -> Note (icon: .plus) {
    return nb.notes.insert(Note(title: ""))
}

destructive action delete(nb: Notebook, n: Note) (icon: .trash_2, label: "Delete note") {
    nb.notes.remove(n)
}

action pin(n: Note) -> Bool proposal note_row (icon: .pin) {
    n.pinned = true
    return true
}

view note_row(n: Note) {
    row(n.title, secondary: n.body, mark: if n.pinned { .pin })
}

view notebook(nb: Notebook) {
    window filter: NoteFilter = .all
    list(nb.notes, filter: filter, empty: "No notes yet") { n ->
        row(n.title, checkbox: n.pinned)
    }
}

view note(n: Note) {
    document {
        section("About") {
            facts {
                "Title": n.title
                "Pinned": n.pinned
            }
        }
        button(pin(n))
    }
}

/// A notebook to take shots of.
scenario "a week of notes" {
    let nb = root Notebook
    new_note(nb)
}
"#;

#[test]
fn the_native_vocabulary_is_canonical_and_round_trips() {
    assert_eq!(print_of(NATIVE), NATIVE);
    crate::common::compile(NATIVE);
    let a = parse(NATIVE).expect("parses");
    let b = parse(&print_module(&a)).expect("printed text parses");
    assert_eq!(print_module(&a), print_module(&b));
}

#[test]
fn facts_on_one_line_and_lists_in_braces_print_canonically() {
    let src = "app t.x \"X\"\nview v(l: L) {\n    facts { \"a\": 1; \"b\": l.x }\n    list(l.items) { i -> text(i.t) }\n}\n";
    let want = "app t.x \"X\"\n\nview v(l: L) {\n    facts {\n        \"a\": 1\n        \"b\": l.x\n    }\n    list(l.items) { i ->\n        text(i.t)\n    }\n}\n";
    assert_eq!(print_of(src), want);
}

#[test]
fn the_app_header_names_its_icon_and_category_and_they_reach_the_metadata() {
    let src = "app so.tilo.lang.notes \"Notes\" (icon: .file_text, category: .office)\n\ntype N {\n    a: Int\n}\n";
    let module = parse(src).expect("parses");
    assert_eq!(module.app.icon.as_deref(), Some("file_text"));
    assert_eq!(module.app.category.as_deref(), Some("office"));
    assert_eq!(
        print_module(&module),
        src,
        "the canonical form keeps the options"
    );
    let compiled = crate::common::compile_with(src, &[]).expect("compiles");
    assert_eq!(compiled.meta.icon.as_deref(), Some("file_text"));
    assert_eq!(compiled.meta.category.as_deref(), Some("office"));
    let plain = crate::common::compile_with("app t.x \"X\"\ntype N {\n    a: Int\n}\n", &[])
        .expect("compiles");
    assert_eq!((plain.meta.icon, plain.meta.category), (None, None));
    for bad in [
        "app t.x \"X\" (category: .weird)\n",
        "app t.x \"X\" (colour: .red)\n",
        "app t.x \"X\" (icon: .a, icon: .b)\n",
        "app t.x \"X\" (icon: \"a.svg\")\n",
    ] {
        assert!(parse(bad).is_err(), "{bad}");
    }
}
