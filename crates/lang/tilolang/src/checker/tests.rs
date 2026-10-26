//! The lowering of the native vocabulary, read off the IR: which nodes a view builds, the
//! properties and events it gives them and what binds them.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert by panicking"
)]

use crate::abi::Imp;
use crate::ids::{IdsLock, SeededEntropy, declared_names};
use crate::ir::{Ek, Expr, Func, Module, Stmt};

/// The package `body` (under a fixed header) lowered to IR, and its ids.
fn lower(body: &str) -> (Module, IdsLock) {
    let src = format!("app t.x \"X\"\n{body}\n");
    let (module, diags) = crate::parser::parse_module(&src);
    assert!(diags.is_empty(), "{diags:#?}");
    let module = module.expect("parses");
    let mut lock = IdsLock::new();
    lock.sync(&declared_names(&module), &mut SeededEntropy(3));
    let mut diags = Vec::new();
    let env = crate::resolve::resolve(&module, &lock, &[], &mut diags);
    let ir = super::check_module(&env, &module, &mut diags);
    assert!(diags.is_empty(), "{diags:#?}");
    (ir, lock)
}

fn func<'a>(m: &'a Module, name: &str) -> &'a Func {
    m.funcs
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no function {name}"))
}

fn text(e: &Expr) -> Option<&str> {
    match &e.kind {
        Ek::Str(s) => Some(s),
        _ => None,
    }
}

/// The UI calls of a function in order, as `node <kind>`, `prop <name>`, `on <event>`,
/// `bind`, `bind_state` and `child`, wherever they sit (loops, branches, sequences).
fn ui_calls(f: &Func) -> Vec<String> {
    let mut out = Vec::new();
    for s in &f.body {
        walk_stmt(s, &mut out);
    }
    out
}

fn walk_stmt(s: &Stmt, out: &mut Vec<String>) {
    match s {
        Stmt::Set(_, e) | Stmt::Eval(e) => walk(e, out),
        Stmt::Return(e) => {
            if let Some(e) = e {
                walk(e, out);
            }
        }
        Stmt::If(c, a, b) => {
            walk(c, out);
            for s in a.iter().chain(b) {
                walk_stmt(s, out);
            }
        }
        Stmt::ForList(fl) => {
            walk(&fl.list, out);
            for s in &fl.body {
                walk_stmt(s, out);
            }
        }
        Stmt::Break => {}
    }
}

fn walk(e: &Expr, out: &mut Vec<String>) {
    match &e.kind {
        Ek::Host(imp, args) => {
            for a in args {
                walk(a, out);
            }
            let name = args.get(1).and_then(text).unwrap_or("?");
            match imp {
                Imp::UiNode => out.push(format!(
                    "node {}",
                    args.first().and_then(text).unwrap_or("?")
                )),
                Imp::UiPropH | Imp::UiPropI | Imp::UiPropF => out.push(format!("prop {name}")),
                Imp::UiOn => out.push(format!("on {name}")),
                Imp::UiBind => out.push("bind".into()),
                Imp::UiBindState => out.push("bind_state".into()),
                Imp::UiChild => out.push("child".into()),
                Imp::ActSet => out.push("set".into()),
                _ => {}
            }
        }
        Ek::Call(_, args) => {
            for a in args {
                walk(a, out);
            }
        }
        Ek::Bin(_, a, b) => {
            walk(a, out);
            walk(b, out);
        }
        Ek::Neg(a) | Ek::Not(a) | Ek::Conv(_, a) => walk(a, out),
        Ek::If(c, a, b) => {
            walk(c, out);
            walk(a, out);
            walk(b, out);
        }
        Ek::Seq(stmts, v) => {
            for s in stmts {
                walk_stmt(s, out);
            }
            walk(v, out);
        }
        _ => {}
    }
}

/// Calls `on` for every expression of a function, outer ones first.
fn visit(f: &Func, on: &mut dyn FnMut(&Expr)) {
    fn expr(e: &Expr, on: &mut dyn FnMut(&Expr)) {
        on(e);
        match &e.kind {
            Ek::Host(_, args) | Ek::Call(_, args) => {
                for a in args {
                    expr(a, on);
                }
            }
            Ek::Bin(_, a, b) => {
                expr(a, on);
                expr(b, on);
            }
            Ek::Neg(a) | Ek::Not(a) | Ek::Conv(_, a) => expr(a, on),
            Ek::If(c, a, b) => {
                expr(c, on);
                expr(a, on);
                expr(b, on);
            }
            Ek::Seq(stmts, v) => {
                for s in stmts {
                    stmt(s, on);
                }
                expr(v, on);
            }
            _ => {}
        }
    }
    fn stmt(s: &Stmt, on: &mut dyn FnMut(&Expr)) {
        match s {
            Stmt::Set(_, e) | Stmt::Eval(e) | Stmt::Return(Some(e)) => expr(e, on),
            Stmt::If(c, a, b) => {
                expr(c, on);
                for s in a.iter().chain(b) {
                    stmt(s, on);
                }
            }
            Stmt::ForList(fl) => {
                expr(&fl.list, on);
                for s in &fl.body {
                    stmt(s, on);
                }
            }
            Stmt::Return(None) | Stmt::Break => {}
        }
    }
    for s in &f.body {
        stmt(s, on);
    }
}

/// Every string constant a function uses.
fn strings(f: &Func) -> Vec<String> {
    let mut out = Vec::new();
    visit(f, &mut |e| {
        if let Ek::Str(s) = &e.kind {
            out.push(s.clone());
        }
    });
    out
}

/// How often a function calls the function `target` of its module.
fn calls_of(f: &Func, target: usize) -> usize {
    let mut n = 0;
    visit(f, &mut |e| {
        if matches!(&e.kind, Ek::Call(t, _) if *t == target) {
            n += 1;
        }
    });
    n
}

fn has(calls: &[String], want: &str) -> bool {
    calls.iter().any(|c| c == want)
}

fn count(calls: &[String], want: &str) -> usize {
    calls.iter().filter(|c| *c == want).count()
}

const NOTES: &str = "type Note {\n    title: Text (title)\n    body: Text (body)\n    pinned: Bool = false\n}\n\
type Notebook {\n    notes: Collection<Note>\n}\n\
/// Which notes a notebook lists.\nenum NoteFilter {\n    /// All notes\n    all\n    /// Pinned\n    pinned\n}\n\
enum NoteOrder {\n    created\n    title\n}\n\
root Notebook\ndefault view notebook for Notebook\n\
fn listed(nb: Notebook, order: NoteOrder, filter: NoteFilter) -> List<Note> {\n    return match filter {\n        .all => nb.notes.filter(n -> true),\n        .pinned => nb.notes.filter(n -> n.pinned),\n    }\n}\n\
view notebook(nb: Notebook) {\n    window order: NoteOrder = .created\n    window filter: NoteFilter = .all\n    list(listed(nb, order, filter), sort: order, filter: filter, empty: \"No notes yet\") { n ->\n        row(n.title, secondary: n.body, mark: if n.pinned { .pin }, checkbox: n.pinned)\n    }\n}";

#[test]
fn a_list_is_a_list_node_with_an_item_per_resource_holding_its_row() {
    let (m, ids) = lower(NOTES);
    let calls = ui_calls(func(&m, "notebook"));
    let at = |calls: &[String], want: &str| {
        calls
            .iter()
            .position(|c| c == want || c.starts_with(&format!("{want}:")))
            .unwrap_or_else(|| panic!("no {want} in {calls:#?}"))
    };
    // The list holds one loop node: each row a view of its own on one item, rendered when the
    // host shows it.
    assert!(at(&calls, "node list") < at(&calls, "prop item_type"));
    assert!(at(&calls, "node list") < at(&calls, "node for"));
    assert!(has(&calls, "prop items") && has(&calls, "prop each"));
    assert!(!has(&calls, "node item"), "no row is built with the list");
    let row = ui_calls(func(&m, "notebook for n"));
    assert!(
        at(&row, "node item") < at(&row, "prop item")
            && at(&row, "prop item") < at(&row, "node list_row"),
        "{row:#?}"
    );
    for want in ["prop title", "prop secondary", "prop mark", "bind"] {
        assert!(has(&row, want), "{want} in {row:#?}");
    }
    assert!(has(&calls, "prop empty"));
    let s = strings(func(&m, "notebook"));
    assert!(
        s.contains(&ids.get("Note").expect("id").to_string()),
        "the item type's id"
    );
    assert!(s.contains(&"No notes yet".to_string()));
    assert!(
        strings(func(&m, "notebook for n")).contains(&"pin".to_string()),
        "the mark's icon in kebab case"
    );
}

#[test]
fn sort_and_filter_become_the_enum_its_selection_and_one_event_per_variant() {
    let (m, ids) = lower(NOTES);
    let calls = ui_calls(func(&m, "notebook"));
    for want in [
        "prop sort.enum",
        "prop sort.selected",
        "on sort.created",
        "on sort.title",
        "prop filter.enum",
        "prop filter.selected",
        "on filter.all",
        "on filter.pinned",
    ] {
        assert!(has(&calls, want), "{want} in {calls:#?}");
    }
    // Each event is exactly what `set(state, .variant)` lowers to.
    assert_eq!(count(&calls, "set"), 4);
    let s = strings(func(&m, "notebook"));
    for id in ["NoteOrder", "NoteFilter"] {
        assert!(
            s.contains(&ids.get(id).expect("id").to_string()),
            "{id}'s id"
        );
    }
}

#[test]
fn a_filter_counts_the_items_under_each_of_its_variants() {
    let (m, _) = lower(NOTES);
    let calls = ui_calls(func(&m, "notebook"));
    assert_eq!(count(&calls, "prop filter.count.all"), 1);
    assert_eq!(count(&calls, "prop filter.count.pinned"), 1);
    // The counts re-evaluate the items with the filter's local set to each variant and put it
    // back: one evaluation of `listed` per variant, plus the list's own.
    let f = func(&m, "notebook");
    let listed = m
        .funcs
        .iter()
        .position(|x| x.name == "listed")
        .expect("listed");
    assert_eq!(calls_of(f, listed), 3);
}

#[test]
fn a_type_with_a_row_view_lists_without_an_item_block() {
    let (m, _) = lower(
        "type Note {\n    title: Text\n}\ntype Notebook {\n    notes: Collection<Note>\n}\nrow view note_row for Note\nview note_row(n: Note) {\n    row(n.title, badge: \"new\", value: 3)\n}\nview notebook(nb: Notebook) {\n    list(nb.notes)\n}",
    );
    let calls = ui_calls(func(&m, "notebook"));
    assert!(
        calls.iter().any(|c| c.starts_with("node for:")),
        "{calls:#?}"
    );
    // Each row is an item holding a call of the row view, its own boundary.
    let rows = ui_calls(func(&m, "notebook for $item"));
    let item = rows.iter().position(|c| c == "node item").expect("item");
    let call = rows
        .iter()
        .position(|c| c.starts_with("node view:"))
        .expect("row");
    assert!(item < call, "{rows:#?}");
    let row = ui_calls(func(&m, "note_row"));
    assert_eq!(
        row,
        vec!["node list_row", "prop title", "prop badge", "prop value"]
    );
}

#[test]
fn frames_sections_facts_forms_and_controls_build_their_nodes() {
    let (m, _) = lower(
        "type Person {\n    name: Text (title)\n    vip: Bool = false\n    owner: Principal\n}\nenum Tab {\n    about\n    notes\n}\nroot Person\ndefault view person for Person\nview person(p: Person) {\n    lens tab: Tab = .about\n    let who = p.name\n    document {\n        sidebar {\n            choice(bind: tab)\n        }\n        inspector(p) {\n            avatar(p.owner)\n        }\n        section(\"About\") {\n            facts {\n                \"Name\": who\n                \"VIP\": p.vip\n            }\n            form {\n                field(\"Name\") {\n                    input(bind: p.name)\n                }\n            }\n            toggle(\"VIP\", bind: p.vip)\n            card(p) {\n                text(p.name, role: .caption)\n            }\n        }\n    }\n}",
    );
    let calls = ui_calls(func(&m, "person"));
    for want in [
        "node document",
        "node sidebar",
        "node choice",
        "prop enum",
        "prop selected",
        "on pick.about",
        "on pick.notes",
        "node inspector",
        "node section",
        "node facts",
        "node fact",
        "prop label",
        "prop value",
        "node form",
        "node field",
        "node toggle",
        "bind",
        "node card",
        "node avatar",
        "prop who",
        "prop role",
    ] {
        assert!(has(&calls, want), "{want} in {calls:#?}");
    }
    assert_eq!(count(&calls, "node fact"), 2);
    assert_eq!(
        calls.first().map(String::as_str),
        Some("node document"),
        "the frame is the root"
    );
}

#[test]
fn a_button_for_a_declared_action_takes_its_label_icon_and_role() {
    let (m, _) = lower(
        "type Draft {\n    text: Text\n}\n/// Send.\nprimary action send_draft(d: Draft) (icon: .send) {\n    d.text = \"\"\n}\nview draft_bar(d: Draft) {\n    stack {\n        input(bind: d.text)\n        button(send_draft(d))\n    }\n}",
    );
    let f = func(&m, "draft_bar");
    let calls = ui_calls(f);
    for want in [
        "node button",
        "prop label",
        "prop icon",
        "prop role",
        "on press",
    ] {
        assert!(has(&calls, want), "{want} in {calls:#?}");
    }
    let s = strings(f);
    for want in ["Send", "send", "primary"] {
        assert!(s.contains(&want.to_string()), "{want} in {s:?}");
    }
}

#[test]
fn a_scenario_is_a_hidden_action_that_reaches_its_root() {
    let (m, ids) = lower(
        "type Note {\n    title: Text\n}\ntype Notebook {\n    notes: Collection<Note>\n}\nroot Notebook\ndefault view nb for Notebook\nview nb(n: Notebook) {\n    page {\n        text(\"x\")\n    }\n}\nscenario \"a week of notes\" {\n    let nb = root Notebook\n    nb.notes.insert(Note(title: \"Q3 plan\"))\n}",
    );
    let id = ids.get("scenario \"a week of notes\"").expect("an id");
    let f = m
        .funcs
        .iter()
        .find(|f| f.export == format!("a_{id}"))
        .expect("the scenario's action");
    assert_eq!(f.nparams, 0);
    assert!(f.facts.mutates);
    let notebook = ids.get("Notebook").expect("id");
    let s = strings(f);
    assert!(s.contains(&format!("root-{notebook}")), "{s:?}");
}

#[test]
fn a_composer_is_a_node_of_the_frame() {
    let (m, _) = lower(
        "type Chat {\n    draft: Text (title)\n}\nview chat(c: Chat) {\n    page {\n        text(c.draft)\n        composer {\n            input(bind: c.draft)\n        }\n    }\n}",
    );
    let calls = ui_calls(func(&m, "chat"));
    let at = |s: &str| calls.iter().position(|c| c == s).expect(s);
    assert!(
        at("node page") < at("node composer") && at("node composer") < at("node input"),
        "{calls:#?}"
    );
}
