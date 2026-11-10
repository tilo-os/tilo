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
use crate::types::Ty;

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
                Imp::UiPropH | Imp::UiPropI | Imp::UiPropF | Imp::UiPropPic => {
                    out.push(format!("prop {name}"));
                }
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

fn has(calls: &[String], want: &str) -> bool {
    calls.iter().any(|c| c == want)
}

fn count(calls: &[String], want: &str) -> usize {
    calls.iter().filter(|c| *c == want).count()
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
    let rows = ui_calls(func(&m, "notebook row"));
    let item = rows.iter().position(|c| c == "node item").expect("item");
    let call = rows
        .iter()
        .position(|c| c.starts_with("node view:"))
        .expect("row");
    assert!(item < call, "{rows:#?}");
    let row = ui_calls(func(&m, "note_row"));
    assert_eq!(
        row,
        vec!["node ui.ListRow", "prop title", "prop badge", "prop value"]
    );
}

#[test]
fn a_mark_and_a_chip_read_the_tone_their_cases_name() {
    let (m, _) = lower(
        "enum Priority {\n    low\n    high (icon: .flag, tone: .warning)\n    urgent (icon: .triangle_alert, tone: .danger)\n}\n\
type Task {\n    title: Text\n    priority: Priority = .low\n}\n\
type Board {\n    tasks: Collection<Task>\n}\n\
root Board\ndefault view board for Board\n\
row view task_row for Task\n\
view task_row(t: Task) {\n    row(t.title, mark: t.priority)\n}\n\
view priority_chip(p: Priority) {\n    stack {\n        ui.Chip(p.label(), tone: p)\n    }\n}\n\
view board(b: Board) {\n    list(b.tasks)\n}",
    );
    let row = ui_calls(func(&m, "task_row"));
    assert_eq!(
        row,
        vec!["node ui.ListRow", "prop title", "prop mark", "prop tone"],
        "the mark and its tone, off the case"
    );
    let tones = strings(func(&m, "task_row"));
    for want in ["flag", "warning", "neutral"] {
        assert!(tones.contains(&want.to_string()), "{want} in {tones:?}");
    }
    let chip = ui_calls(func(&m, "priority_chip"));
    assert_eq!(
        chip,
        vec![
            "node stack",
            "node ui.Chip",
            "prop label",
            "prop tone",
            "child"
        ]
    );
    let tones = strings(func(&m, "priority_chip"));
    for want in ["warning", "danger", "neutral"] {
        assert!(tones.contains(&want.to_string()), "{want} in {tones:?}");
    }
    let wrong = diagnostics(
        "enum Priority {\n    high\n}\ntype Task {\n    title: Text\n    priority: Priority = .high\n}\nview task(t: Task) {\n    stack {\n        ui.Chip(t.title, tone: t.title)\n    }\n}",
    );
    assert!(wrong.iter().any(|d| d.code == "E065"), "{wrong:#?}");

    // A case that names an icon but no tone says nothing about importance: its glyph never
    // reaches the row.
    let (m, _) = lower(
        "enum Priority {\n    low (icon: .pin)\n    high (icon: .flag, tone: .warning)\n}\n\
type Task {\n    title: Text\n    priority: Priority = .low\n}\n\
row view task_row for Task\n\
view task_row(t: Task) {\n    row(t.title, mark: t.priority)\n}",
    );
    let marks = strings(func(&m, "task_row"));
    assert!(marks.contains(&"flag".to_string()), "{marks:?}");
    assert!(
        !marks.contains(&"pin".to_string()),
        "a case that reads in no tone draws no mark: {marks:?}"
    );
}

/// A prop the checker sets on a described component that the component does not declare is the
/// compiler's own defect: the window refuses a batch that sets one, so the compile that would
/// have sent it has to fail first.
#[test]
fn a_prop_a_component_does_not_declare_fails_the_compile() {
    let module = |props: &[(&str, &str)]| {
        let node = Expr::host(
            Imp::UiNode,
            vec![Expr::str("ui.ListRow"), Expr::str("row")],
            crate::types::Ty::View,
        );
        let mut body = vec![Stmt::Set(0, node)];
        for (name, value) in props {
            body.push(Stmt::Eval(Expr::host(
                Imp::UiPropH,
                vec![
                    Expr::local(0, crate::types::Ty::View),
                    Expr::str(name),
                    Expr::str(value),
                ],
                crate::types::Ty::Unit,
            )));
        }
        Module {
            funcs: vec![Func {
                at: None,
                kind: crate::ir::FuncKind::View,
                name: "task_row".into(),
                export: "v_task_row".into(),
                nparams: 0,
                result: crate::types::Ty::View,
                locals: vec![crate::types::Ty::View],
                body,
                facts: crate::ir::Facts::default(),
            }],
            ..Module::default()
        }
    };
    let declared = [
        ("title", "Buy oat milk"),
        ("meta", "2026-11-03"),
        ("meta:as", "date"),
        ("meta:time", "2026-11-03T17:00:00Z"),
        ("meta_tone", "warning"),
    ];
    let mut diags = Vec::new();
    super::props::undeclared(&module(&declared), &mut diags);
    assert!(diags.is_empty(), "{diags:#?}");
    let mut diags = Vec::new();
    super::props::undeclared(&module(&[("meta:tmie", "2026-11-03")]), &mut diags);
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(diags[0].code, "E000");
    assert!(diags[0].message.contains("meta:tmie"), "{diags:#?}");
}

/// An item's page opens on what the item is: its title is the document's own title, in place
/// and in its large type, and it takes the width the done checkbox leaves so a long one wraps
/// beside it — never a small field of a form.

#[test]
fn a_button_for_a_declared_action_takes_its_label_icon_and_role() {
    let (m, _) = lower(
        "type Draft {\n    text: Text\n}\n/// Send.\nprimary action send_draft(d: Draft) (icon: .send) {\n    d.text = \"\"\n}\nview draft_bar(d: Draft) {\n    stack {\n        input(bind: d.text)\n        button(send_draft(d))\n    }\n}",
    );
    let f = func(&m, "draft_bar");
    let calls = ui_calls(f);
    for want in [
        "node ui.Button",
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
fn a_button_for_an_action_reads_its_label_icon_and_state_with_what_it_applies() {
    let (m, _) = lower(
        "type Note {\n    title: Text\n    pinned: Bool = false\n}\naction pin(n: Note, pinned: Bool = not n.pinned) (label: if pinned { \"Pin\" } else { \"Unpin\" }, icon: if n.pinned { .star } else { .pin }, on: n.pinned) {\n    n.pinned = pinned\n}\nview note(n: Note) {\n    stack {\n        button(pin(n), icon_only: true)\n    }\n}",
    );
    let f = func(&m, "note");
    let calls = ui_calls(f);
    assert!(has(&calls, "prop toggled"), "{calls:#?}");
    let s = strings(f);
    for want in ["Pin", "Unpin", "pin", "star"] {
        assert!(s.contains(&want.to_string()), "{want} in {s:?}");
    }
}

#[test]
fn a_primitive_a_component_draws_takes_the_components_events() {
    let (m, _) = lower(
        "type Chat {\n    topic: Text\n}\naction say(c: Chat, body: RichText) {\n    c.topic = body.excerpt\n}\nview chat(c: Chat) {\n    state draft: RichText = RichText(\"\")\n    stack {\n        rich_input(bind: draft) {\n            on submit: say(c, draft).then(set(draft, RichText(\"\")))\n        }\n    }\n}",
    );
    let calls = ui_calls(func(&m, "chat"));
    assert!(has(&calls, "on submit"), "{calls:#?}");
    let wrong = diagnostics(
        "type Chat {\n    topic: Text\n}\nview chat(c: Chat) {\n    state draft: RichText = RichText(\"\")\n    rich_input(bind: draft) {\n        on sent: set(draft, RichText(\"\"))\n    }\n}",
    );
    assert!(wrong.iter().any(|d| d.code == "E063"), "{wrong:#?}");
}

#[test]
fn a_version_and_an_offered_action_are_asked_for_by_the_action_itself() {
    let (m, lock) = lower(
        "type Note {\n    title: Text\n}\n/// Renames it.\naction rename(n: Note, title: Text = n.title) (label: \"Edit\") {\n    n.title = title\n}\nfn editable(n: Note) -> Bool {\n    actions(n).any(a -> a.is(rename))\n}\nview note(n: Note) {\n    stack {\n        if history(n).any(v -> v.is(rename)) {\n            text(\"edited\")\n        }\n        if editable(n) {\n            text(\"editable\")\n        }\n    }\n}",
    );
    let f = func(&m, "note");
    let id = lock.get("rename").expect("rename's id").to_owned();
    assert!(
        strings(f).contains(&id),
        "compared by the action's id, {id}"
    );
    assert!(
        !strings(f).iter().any(|s| s == "Edit"),
        "never by its label"
    );
    for wrong in ["v.is(renamed)", "v.is(\"Edit\")", "v.is(n)"] {
        let found = diagnostics(&format!(
            "type Note {{\n    title: Text\n}}\naction rename(n: Note) {{\n    n.title = n.title\n}}\nview note(n: Note) {{\n    if history(n).any(v -> {wrong}) {{\n        text(\"edited\")\n    }}\n}}"
        ));
        assert!(
            found.iter().any(|d| d.code == "E022"),
            "{wrong}: {found:#?}"
        );
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

fn asked(f: &Func) -> Vec<String> {
    let mut out = Vec::new();
    visit(f, &mut |e| {
        if let Ek::Host(Imp::CollWhere, args) = &e.kind
            && let Some(pred) = args.get(1).and_then(text)
        {
            out.push(pred.to_owned());
        }
    });
    out
}

/// The predicate of a filter handed to the store with the values the code holds.
fn asked_with(f: &Func) -> Vec<String> {
    let mut out = Vec::new();
    visit(f, &mut |e| {
        if let Ek::Host(Imp::CollWhereWith, args) = &e.kind
            && let Some(pred) = args.get(1).and_then(text)
        {
            out.push(pred.to_owned());
        }
    });
    out
}

#[test]
fn an_option_compares_with_the_value_it_holds() {
    let (m, ids) = lower(
        "type Note {\n    title: Text\n    parent: Option<Note>\n    owner: Option<Text>\n}\n\
type Book {\n    notes: Collection<Note>\n}\n\
fn children(b: Book, p: Note) -> List<Note> {\n    return b.notes.filter(n -> n.parent == .some(p))\n}\n\
fn owned(b: Book, who: Text) -> List<Note> {\n    return b.notes.filter(n -> n.owner == .some(who))\n}\n\
fn unowned(b: Book) -> List<Note> {\n    return b.notes.filter(n -> n.owner == .none)\n}",
    );
    let field = |name: &str| ids.get(&format!("Note.{name}")).expect("id").to_string();
    let parent = asked_with(func(&m, "children"));
    assert_eq!(parent.len(), 1, "{parent:?}");
    let pred: crate::pred::Pred<String> = serde_json::from_str(&parent[0]).expect("a predicate");
    assert_eq!(pred.fields(), [&field("parent")]);
    assert_eq!(
        pred,
        crate::pred::Pred::Cmp(
            crate::pred::CmpOp::Eq,
            crate::pred::Operand::Field(field("parent")),
            crate::pred::Operand::Arg(0),
        ),
        "the value the option holds, bound when it is asked"
    );
    assert_eq!(asked_with(func(&m, "owned")).len(), 1);
    let unowned = asked(func(&m, "unowned"));
    assert_eq!(unowned.len(), 1, "{unowned:?}");
    assert_eq!(
        serde_json::from_str::<crate::pred::Pred<String>>(&unowned[0]).expect("a predicate"),
        crate::pred::Pred::Not(Box::new(crate::pred::Pred::Present(field("owner"))))
    );
}

#[test]
fn a_filter_or_count_over_item_fields_asks_the_store_beside_its_loop() {
    let (m, ids) = lower(
        "type Item {\n    title: Text\n    done: Bool = false\n    votes: Int merge counter\n    tags: Set<Text>\n}\n\
type Bag {\n    items: Collection<Item>\n}\n\
fn unfinished(b: Bag) -> Int {\n    return b.items.count(i -> not i.done and i.votes >= 2)\n}\n\
fn titled(b: Bag) -> List<Item> {\n    return b.items.filter(i -> i.title == \"x\")\n}\n\
fn outer(b: Bag, t: Text) -> List<Item> {\n    return b.items.filter(i -> i.title == t)\n}\n\
fn tagged(b: Bag) -> Int {\n    return b.items.count(i -> i.tags.len() > 0)\n}\n\
fn ordered(b: Bag) -> List<Item> {\n    return b.items.filter(i -> i.title < \"x\")\n}\n\
fn listed(b: Bag) -> Int {\n    let l = b.items.filter(i -> true)\n    return l.count(i -> i.done)\n}",
    );
    let field = |name: &str| ids.get(&format!("Item.{name}")).expect("id").to_string();
    let open = asked(func(&m, "unfinished"));
    assert_eq!(open.len(), 1, "{open:?}");
    let pred: crate::pred::Pred<String> = serde_json::from_str(&open[0]).expect("a predicate");
    assert_eq!(pred.fields(), [&field("done"), &field("votes")]);
    assert_eq!(asked(func(&m, "titled")).len(), 1);
    // The loop stays beside it, for where the host does not answer.
    assert!(
        func(&m, "unfinished")
            .body
            .iter()
            .any(|s| format!("{s:?}").contains("ForList")),
        "the loop"
    );
    // A lambda reading text by order is the loop's alone. How many members a set holds is the
    // store's to count.
    for loop_only in ["outer", "ordered"] {
        assert!(asked(func(&m, loop_only)).is_empty(), "{loop_only}");
    }
    assert_eq!(asked(func(&m, "tagged")).len(), 1);
    // A list read of a collection is the collection's items still, whatever its type says: the
    // count over it is answered where the items were read, not item by item.
    assert_eq!(
        asked(func(&m, "listed")).len(),
        2,
        "the collection's filter, and the list it read"
    );
}

/// A template's parameters are names of the template: a caller's block is read where it was
/// written, so a name there that a parameter shares is the caller's. Reading it in the
/// template's place made the block call the parameter itself, expanding it forever.
#[test]
fn a_callers_block_reads_its_names_in_the_callers_scope() {
    let got = diagnostics(
        "type Note {\n    title: Text\n}\ntype Notes {\n    items: Collection<Note>\n}\n\
         template view frame<T>(items: List<T>, row: (T, Int) -> View) {\n\
             ui.VirtualList {\n        for x in items {\n            row(x, 0)\n        }\n    }\n}\n\
         view notes(ns: Notes) {\n    frame(ns.items) { (x, at) ->\n        row(x.title)\n    }\n}",
    );
    assert_eq!(
        got.iter().map(|d| d.code.as_str()).collect::<Vec<_>>(),
        ["E063"],
        "{got:#?}"
    );
}

/// The package `body` checked: its diagnostics.
fn diagnostics(body: &str) -> Vec<crate::diag::Diagnostic> {
    let src = format!("app t.x \"X\"\n{body}\n");
    let (module, diags) = crate::parser::parse_module(&src);
    assert!(diags.is_empty(), "{diags:#?}");
    let module = module.expect("parses");
    let mut lock = IdsLock::new();
    lock.sync(&declared_names(&module), &mut SeededEntropy(3));
    let mut diags = Vec::new();
    let env = crate::resolve::resolve(&module, &lock, &[], &mut diags);
    let _ = super::check_module(&env, &module, &mut diags);
    diags
}

#[test]
fn a_condition_reading_an_argument_inside_a_lambda_waits_for_it() {
    const TYPES: &str = "type Task {\n    title: Text\n    parent: Option<Task>\n}\ntype Tasks {\n    items: Collection<Task>\n}\n";
    // The condition reads `t` in the lambda's body: the offer cannot answer it, since it knows
    // the tasks alone. Before, the lambda hid `t`, the condition was compiled into the applies
    // guard, and the guard failed to name it.
    let (m, ids) = lower(&format!(
        "{TYPES}action duplicate(tasks: Tasks, t: Task) {{\n    applies tasks.items.filter(x -> x.parent == .some(t)).is_empty() \"It has tasks under it\"\n    t.title = \"copy\"\n}}"
    ));
    let id = ids.get("duplicate").expect("duplicate's id");
    assert!(
        !m.funcs
            .iter()
            .any(|f| f.export == crate::ir::applies_export(id)),
        "a condition reading an argument waits for it, rather than being offered"
    );
    // A name the lambda binds itself is its own, not the caller's: the condition reads nothing
    // beyond the tasks, so the offer decides it.
    let (m, ids) = lower(&format!(
        "{TYPES}action sweep(tasks: Tasks, t: Task) {{\n    applies tasks.items.any(t -> t.title == \"\") \"It has no title\"\n    t.title = \"x\"\n}}"
    ));
    let id = ids.get("sweep").expect("sweep's id");
    assert!(
        m.funcs
            .iter()
            .any(|f| f.export == crate::ir::applies_export(id)),
        "a lambda's own name is not the argument it is named after"
    );
}

#[test]
fn a_filter_named_by_its_type_is_the_value_a_query_and_a_sidebar_take() {
    const FILTERS: &str = "type Task {\n    title: Text\n    done: Bool = false\n    project: Option<Project> = .none\n    search {\n        filter \"Inbox\" = not done (icon: .inbox)\n        filter \"Done\" = done\n        filter project\n    }\n}\ntype Project {\n    name: Text\n}\ntype Board {\n    tasks: Collection<Task>\n    projects: Collection<Project>\n}\n";
    let view = "view board(b: Board) {\n    state place: Filter<Task> = Task.inbox\n    ui.NavList(selected: place) {\n        for f in filters(Task).filter(f -> f.toggle) {\n            ui.NavItem(f, f.label) {\n                on select: set(place, f)\n            }\n        }\n        for p in b.projects {\n            ui.NavItem(Task.project(p), p.name, icon: .folder) {\n                on select: set(place, Task.project(p))\n            }\n        }\n    }\n    for t in b.tasks.matching(place) {\n        text(t.title)\n    }\n}";
    let (m, ids) = lower(&format!("{FILTERS}{view}"));
    let task = ids.get("Task").expect("id").to_string();
    let query_field = crate::host::value_field("Filter", "query").expect("query");
    // The values the view names are the filters the type declares, by its id and their keys:
    // `Task.inbox`, `Task.project(p)`. The host turns each into what search says, so no query
    // text is written here.
    let mut lits = Vec::new();
    let mut from_filters = 0;
    for g in m.funcs.iter().filter(|g| g.name.starts_with("board")) {
        visit(g, &mut |e| match &e.kind {
            Ek::Host(Imp::FilterLit, args) => lits.push((
                args.first().and_then(text).map(str::to_owned),
                args.get(1).and_then(text).map(str::to_owned),
            )),
            Ek::Host(Imp::TupleGetH, args) if matches!(args.get(1).map(|e| &e.kind), Some(Ek::I32(at)) if *at as usize == query_field) =>
            {
                from_filters += 1;
            }
            _ => {}
        });
    }
    assert_eq!(
        lits,
        vec![
            (Some(task.clone()), Some("inbox".to_owned())),
            (Some(task.clone()), Some("project".to_owned())),
            (Some(task), Some("project".to_owned())),
        ],
        "the filter value the state starts in, and the project places"
    );
    assert!(
        from_filters >= 4,
        "the query the state asks, the two places chosen, and the list's `selected`: {from_filters}"
    );
    for g in m.funcs.iter().filter(|g| g.name.starts_with("board")) {
        assert!(
            !strings(g)
                .iter()
                .any(|s| s.starts_with("is:") || s.starts_with("project:")),
            "no search text is written: {:?}",
            strings(g)
        );
    }
    // A filter the type does not declare is a compile error, not a query the index answers
    // with nothing.
    for wrong in ["Task.todays", "Task.inbo"] {
        let found = diagnostics(&format!(
            "{FILTERS}view board(b: Board) {{\n    for t in b.tasks.matching({wrong}) {{\n        text(t.title)\n    }}\n}}"
        ));
        let said = found.iter().find(|d| d.code == "E105").unwrap_or_else(|| {
            panic!("{wrong} is a compile error: {found:#?}");
        });
        assert!(
            said.message.contains("declares no filter") && said.message.contains("Task.inbox"),
            "{wrong}: {}",
            said.message
        );
    }
}

#[test]
fn a_document_page_and_transcript_are_shells_of_the_library() {
    let (m, _) = lower(
        "type Person {\n    name: Text\n    vip: Bool = false\n    owner: Principal\n}\nenum Tab {\n    about\n    notes\n}\nroot Person\ndefault view person for Person\nview person(p: Person) {\n    state tab: Tab = .about\n    std.document(p) {\n        choice(bind: tab)\n        avatar(p.owner)\n        section(\"About\") {\n            facts(p)\n            form {\n                field(\"Name\") {\n                    input(bind: p.name)\n                }\n            }\n            toggle(\"VIP\", bind: p.vip)\n            ui.Card(item: p, title: meta(p).title) {\n                text(p.name, role: .caption)\n            }\n        }\n    }\n}",
    );
    let calls = ui_calls(func(&m, "person"));
    for want in [
        "node ui.NavShell",
        "node ui.Scroll",
        "node page",
        "node choice",
        "on pick.about",
        "node ui.Section",
        "node ui.PersonField",
        "node form",
        "node field",
        "node ui.Toggle",
        "node ui.Card",
        "node ui.Avatar",
        "prop title",
    ] {
        assert!(has(&calls, want), "{want} in {calls:#?}");
    }
    assert_eq!(
        calls.first().map(String::as_str),
        Some("node ui.NavShell"),
        "the shell is the root"
    );
    let (m, _) = lower(
        "type Chat {\n    draft: Text\n    lines: Collection<Line>\n}\ntype Line {\n    text: Text\n}\nview line(l: Line) {\n    text(l.text)\n}\nview chat(c: Chat) {\n    state draft: Text\n    std.transcript(c, c.lines, row: line) {\n        composer: {\n            input(bind: draft)\n        }\n    }\n}",
    );
    let calls = ui_calls(func(&m, "chat"));
    let at = |s: &str| calls.iter().position(|c| c == s).expect(s);
    assert!(
        at("node ui.Docked") < at("node ui.TextInput")
            && at("node ui.Docked") < at("node ui.VirtualList"),
        "{calls:#?}"
    );
}

#[test]
fn an_event_that_carries_its_value_passes_it_to_the_action_as_it_is() {
    let (m, _) = lower(
        "type Note {\n    title: Text\n    done: Bool = false\n}\ntype Notebook {\n    notes: Collection<Note>\n}\nroot Notebook\ndefault view notebook for Notebook\naction check(n: Note, done: Bool) {\n    n.done = done\n}\nview notebook(nb: Notebook) {\n    ui.VirtualList {\n        on select(n: Note): open(n, front: false)\n    }\n}\nview note(n: Note) {\n    ui.ListRow(title: n.title) {\n        on activate: open(n)\n    }\n}",
    );
    let mut placed = 0;
    visit(func(&m, "notebook"), &mut |e| {
        if matches!(e.kind, Ek::Host(Imp::EventValue, _)) {
            placed += 1;
        }
    });
    assert_eq!(placed, 1, "the action holds the event's value in its place");
    let wrong = diagnostics(
        "type Note {\n    title: Text\n}\ntype Notebook {\n    notes: Collection<Note>\n}\nview notebook(nb: Notebook) {\n    ui.VirtualList {\n        on select(n: Note): open(nb.notes.first().unwrap_or(n))\n    }\n    ui.Button(\"Go\") {\n        on press(x: Note): open(x)\n    }\n}",
    );
    let codes: Vec<&str> = wrong.iter().map(|d| d.code.as_str()).collect();
    assert!(
        codes.iter().filter(|c| **c == "E063").count() >= 2,
        "{wrong:#?}"
    );
}

#[test]
fn lenses_and_meta_are_values_a_view_reads() {
    let (m, _) = lower(
        "type Note {\n    title: Text\n}\nview note(n: Note) {\n    let here = lenses(n)\n    stack {\n        text(meta(n).title)\n        text(meta(Note).noun)\n        text(\"{here.len()}\")\n    }\n}",
    );
    let mut imports = Vec::new();
    for f in &m.funcs {
        visit(f, &mut |e| {
            if let Ek::Host(imp, _) = &e.kind
                && matches!(imp, Imp::Lenses | Imp::MetaOf | Imp::TupleGetH)
            {
                imports.push(*imp);
            }
        });
    }
    for want in [Imp::Lenses, Imp::MetaOf, Imp::TupleGetH] {
        assert!(imports.contains(&want), "{want:?} in {imports:?}");
    }
}

/// The host imports `f` calls.
fn host_imports(f: &Func) -> Vec<Imp> {
    let mut imports = Vec::new();
    visit(f, &mut |e| {
        if let Ek::Host(imp, _) = &e.kind {
            imports.push(*imp);
        }
    });
    imports
}

#[test]
fn workspace_members_and_members_of_a_resource_are_principals_with_kind_and_role() {
    let (m, _) = lower(
        "type Room {\n    topic: Text\n}\nfn agents(r: Room) -> Int {\n    members(r).count(p -> p.kind() == .agent)\n}\nfn roles() -> List<Text> {\n    workspace.members.map(p -> p.role() ?? p.name())\n}\nview room(r: Room) {\n    stack {\n        for p in workspace.members {\n            avatar(p)\n        }\n        text(\"{agents(r)}\")\n    }\n}\nview mine(r: Room) {\n    text(\"{workspace.me.name()}\")\n}",
    );
    let agents = host_imports(func(&m, "agents"));
    for want in [Imp::Members, Imp::PrincipalKind] {
        assert!(agents.contains(&want), "{want:?} in {agents:?}");
    }
    let roles = host_imports(func(&m, "roles"));
    for want in [
        Imp::WorkspaceMembers,
        Imp::PrincipalRole,
        Imp::PrincipalName,
    ] {
        assert!(roles.contains(&want), "{want:?} in {roles:?}");
    }
    assert!(host_imports(func(&m, "room")).contains(&Imp::WorkspaceMembers));
    let mine = host_imports(func(&m, "mine"));
    assert!(
        mine.contains(&Imp::Viewer),
        "`workspace.me` is the viewer, read where the view runs: {mine:?}"
    );
    for (source, code) in [
        (
            "type Room {\n    topic: Text\n}\nfn f() -> Int {\n    workspace.people.len()\n}",
            "E021",
        ),
        ("fn f() -> Int {\n    let w = workspace\n    0\n}", "E064"),
        ("fn f() -> Int {\n    members(3).len()\n}", "E020"),
        (
            "fn f(p: Principal) -> Bool {\n    p.kind() == .robot\n}",
            "",
        ),
    ] {
        let found = diagnostics(source);
        if code.is_empty() {
            assert!(
                !found.is_empty(),
                "a principal is a person or an agent: {source}"
            );
        } else {
            assert!(
                found.iter().any(|d| d.code == code),
                "{code} for {source}: {found:#?}"
            );
        }
    }
    // A variable of that name is the variable.
    let local = diagnostics(
        "type W {\n    members: Int\n}\nfn f(workspace: W) -> Int {\n    workspace.members\n}",
    );
    assert!(local.is_empty(), "{local:#?}");
}

#[test]
fn linked_and_made_from_are_read_from_the_log() {
    let (m, _) = lower(
        "type Note {\n    title: Text\n}\nfn links(n: Note) -> List<host.Link> {\n    linked(n)\n}\nfn made_from(n: Note) -> Option<host.Resource> {\n    created(n)?.from\n}",
    );
    let links = func(&m, "links");
    assert!(host_imports(links).contains(&Imp::Links));
    let mut link_ty = None;
    visit(links, &mut |e| {
        if matches!(&e.kind, Ek::Host(Imp::Links, _)) {
            link_ty = Some(e.ty.clone());
        }
    });
    assert_eq!(
        link_ty,
        Some(Ty::List(Box::new(super::platform::host_ty("Link")))),
        "`linked(r)` is a list of what links and how"
    );
    // What it was made from is a fact of the version that made it.
    let made = func(&m, "made_from");
    assert!(host_imports(made).contains(&Imp::VersionEnd));
    let mut from = false;
    visit(made, &mut |e| {
        if let Ek::Host(Imp::TupleGetH, _) = &e.kind
            && e.ty == Ty::Option(Box::new(Ty::Record(super::resource_key())))
        {
            from = true;
        }
    });
    assert!(from, "`created(r)?.from` is an optional resource");
    // A resource is what it takes.
    let found = diagnostics("fn f() -> Int {\n    linked(1).len()\n}");
    assert!(found.iter().any(|d| d.code == "E020"), "{found:#?}");
}

#[test]
fn an_action_applied_made_from_a_resource_is_invoked_from_it() {
    let (m, _) = lower(
        "type Note {\n    title: Text\n}\ntype Notes {\n    items: Collection<Note>\n}\nroot notes: Notes\ndefault view all for Notes\naction keep(all: Notes, title: Text) -> Note {\n    all.items.insert(Note(title: title))\n}\nview all(all: Notes) {\n    text(meta(all).title)\n}\nview note(n: Note) {\n    button(keep(notes, n.title).made_from(n))\n}",
    );
    // What a view shows of an action is the action the host runs; made from a resource, it runs
    // from that resource, so what it makes records what it was made from.
    let mut made_from = false;
    for f in &m.funcs {
        visit(f, &mut |e| {
            if let Ek::Host(Imp::ActMadeFrom, _) = &e.kind {
                made_from = true;
            }
        });
    }
    assert!(made_from, "`made_from` is the host's own step: {m:#?}");
}

#[test]
fn a_links_how_is_a_case_of_the_hosts_link_how_enum() {
    let (m, _) = lower(
        "type Note {\n    title: Text\n}\nfn mentions(n: Note) -> Bool {\n    linked(n).any(l -> l.how == .mention)\n}",
    );
    let mentions = func(&m, "mentions");
    let mut cases: Vec<(Option<String>, Option<String>)> = Vec::new();
    visit(mentions, &mut |e| {
        if let Ek::Host(Imp::EnumNew, args) = &e.kind {
            cases.push((
                args.first().and_then(text).map(str::to_owned),
                args.get(1).and_then(text).map(str::to_owned),
            ));
        }
    });
    assert_eq!(
        cases,
        vec![(
            Some(crate::host::LINK_HOW_TYPE.to_owned()),
            Some("mention".to_owned())
        )],
        "`.mention` is the host's `LinkHow` case"
    );
    assert!(
        host_imports(mentions).contains(&Imp::ValueEq),
        "a how is compared by value"
    );
    // A word the host never answers is not a case of it.
    let found = diagnostics(
        "type Note {\n    title: Text\n}\nfn mentions(n: Note) -> Bool {\n    linked(n).any(l -> l.how == .mentioned)\n}",
    );
    assert!(found.iter().any(|d| d.code == "E036"), "{found:#?}");
}

#[test]
fn every_rich_input_is_given_its_drawers_people_by_the_system() {
    let (m, _) = lower(
        "type Chat {\n    topic: Text\n}\nview chat(c: Chat) {\n    state draft: RichText = RichText(\"\")\n    stack {\n        rich_input(bind: draft)\n    }\n}\nview loose() {\n    state draft: RichText = RichText(\"\")\n    rich_input(bind: draft)\n}",
    );
    let chat = host_imports(func(&m, "chat"));
    for want in [Imp::Speakers, Imp::Members] {
        assert!(chat.contains(&want), "{want:?} in {chat:?}");
    }
    assert!(
        strings(func(&m, "chat")).iter().any(|s| s == "people")
            && strings(func(&m, "chat")).iter().any(|s| s == "speakers")
    );
    let loose = host_imports(func(&m, "loose"));
    assert!(
        loose.contains(&Imp::WorkspaceMembers) && !loose.contains(&Imp::Speakers),
        "without a resource, the workspace's members: {loose:?}"
    );
}

#[test]
fn a_generic_view_with_a_lambda_and_view_state_expands_where_it_is_called() {
    let (m, _) = lower(
        "type Item {\n    title: Text\n}\ntype Bag {\n    items: Collection<Item>\n}\nenum Size {\n    small\n    large\n}\nview shown<T, E>(things: List<T>, size: State<E>, label: (T) -> Text, extra: Option<View> = .none) {\n    stack {\n        for c in cases(E) {\n            button(c.label(), on: set(size, c))\n        }\n        for t in things {\n            text(label(t))\n        }\n        match extra {\n            .some(v) => v,\n            .none => text(\"none\"),\n        }\n    }\n}\nview bag(b: Bag) {\n    state size: Size = .small\n    shown(b.items, size: size, label: i -> i.title)\n}",
    );
    let calls = ui_calls(func(&m, "bag"));
    assert_eq!(
        count(&calls, "set"),
        2,
        "one per case, unrolled: {calls:#?}"
    );
    assert!(
        m.funcs.iter().all(|f| f.name != "shown"),
        "a template has no function of its own"
    );
    let strings = strings(func(&m, "bag"));
    assert!(
        strings.contains(&"none".to_string()),
        "the static `.none` arm"
    );
    let wrong = diagnostics(
        "type Bag {\n    n: Int\n}\nview shown<E>(size: State<E>) {\n    text(\"x\")\n}\nview bag(b: Bag) {\n    shown(size: b.n)\n}",
    );
    assert!(
        wrong.iter().any(|d| d.code == "E066"),
        "view state is passed by its name: {wrong:#?}"
    );
}

/// A `State<T>` parameter left out keeps the state the template declares as its default, so a
/// view that lists declares none: a caller that wants the state passes its own, and the
/// template then declares none of it.
#[test]
fn a_state_parameter_left_out_is_the_state_the_template_declares() {
    let template = "enum Size {\n    small\n    large\n}\n\
        view shown(items: List<Text>, size: State<Size> = own, extra: Option<State<Size>> = .none) {\n    \
        state own: Size = .small\n    \
        for c in cases(Size) {\n        button(c.label(), on: set(size, c))\n    }\n    \
        if let e = extra {\n        button(\"e\", on: set(e, .small), label: \"e\")\n    }\n}\n\
        type Bag {\n    items: Collection<Text>\n}\n";
    // Left out: the template's own state, declared where it expands.
    let (m, _) = lower(&format!(
        "{template}view bag(b: Bag) {{\n    shown(b.items)\n}}"
    ));
    assert_eq!(
        m.states.len(),
        1,
        "the template's own state: {:#?}",
        m.states
    );
    assert!(
        m.states[0].1.id.ends_with(":shown.own"),
        "kept under the view's id and the template's: {:#?}",
        m.states
    );
    // A caller's state of the same type takes its place, and the template declares none of it.
    let (m, _) = lower(&format!(
        "{template}view bag(b: Bag) {{\n    state mine: Size = .small\n    shown(b.items, size: mine)\n}}"
    ));
    assert!(
        m.states.is_empty(),
        "nothing of the template's own is declared: {:#?}",
        m.states
    );
    // A template that takes an optional state keeps its own when a caller passes on none of it.
    let (m, _) = lower(&format!(
        "{template}view middle(items: List<Text>, opening: Option<State<Size>> = .none) {{\n    shown(items, size: opening)\n}}\n\
         view bag(b: Bag) {{\n    middle(b.items)\n}}"
    ));
    assert_eq!(
        m.states.len(),
        1,
        "the template's own state, through the absent option: {:#?}",
        m.states
    );
    // The default must name a state the template declares, and a parameter without one is still
    // the caller's to give.
    let undeclared = diagnostics(
        "type Bag {\n    n: Int\n}\nview shown(size: State<Int> = missing) {\n    text(\"x\")\n}\nview bag(b: Bag) {\n    shown()\n}",
    );
    assert!(
        undeclared.iter().any(|d| d.code == "E066"),
        "the default names no state of the template: {undeclared:#?}"
    );
    let missing = diagnostics(
        "type Bag {\n    n: Int\n}\nview shown(size: State<Int>) {\n    text(\"x\")\n}\nview bag(b: Bag) {\n    shown()\n}",
    );
    assert!(
        missing.iter().any(|d| d.code == "E022"),
        "a state parameter with no default is the caller's: {missing:#?}"
    );
}

/// A package may name a view as the library names one: only the same package's view is the same
/// view, so an app's own `composer` calling `std.composer` expands the library's, not itself.
#[test]
fn an_apps_view_named_as_the_libraries_expands_the_libraries() {
    let (m, _) = lower(
        "type Chat {\n    topic: Text\n}\naction send(c: Chat, text: RichText) {\n}\nview composer(c: Chat) {\n    state draft: RichText = RichText(\"\")\n    std.composer(draft, \"Message {c.topic}\", send(c, draft))\n}\nview chat(c: Chat) {\n    composer(c)\n}",
    );
    let chat = ui_calls(func(&m, "chat"));
    assert!(
        chat.iter().any(|c| c.starts_with("node view:")),
        "the app's composer stands in its chat: {chat:#?}"
    );
    let calls = ui_calls(func(&m, "composer"));
    assert!(
        calls.iter().any(|c| c.starts_with("node ui.RichInput")),
        "the library's field is the one drawn: {calls:#?}"
    );
    assert!(has(&calls, "on submit"), "and it sends: {calls:#?}");
    assert!(
        has(&calls, "on press"),
        "and so does its button: {calls:#?}"
    );
}

/// `ui.Time(at)` of a `Time` is the toolkit's relative time: a node whose `at` is the moment,
/// marked as a time for the host to read it as one.
#[test]
fn a_time_shows_as_the_toolkits_relative_time() {
    let (m, _) = lower(
        "type Note {\n    title: Text\n    seen: Time\n}\n\
         view when(n: Note) {\n    ui.Time(n.seen, secondary: true)\n}\n",
    );
    let calls = ui_calls(func(&m, "when"));
    assert!(has(&calls, "node ui.Time"), "{calls:#?}");
    assert!(has(&calls, "prop at"), "{calls:#?}");
    assert!(has(&calls, "prop at:as"), "{calls:#?}");
    assert!(has(&calls, "prop secondary"), "{calls:#?}");
}

/// A view that writes a list of filters as a type (`let side: List<Filter<Task>>`) holds
/// filters: the written type resolves in a view as it does in an action, so the places a
/// window draws are the ones the app names.
#[test]
fn a_views_list_of_filters_is_a_list_of_filters() {
    let (m, _) = lower(
        r#"
type Task {
    title: Text
    done: Bool = false
    search {
        filter "Inbox" = not done (icon: .inbox)
        filter "Done" = done
    }
}
type Tasks { items: Collection<Task> }
root tasks: Tasks
default view home for Tasks
view home(t: Tasks) {
    state place: Filter<Task> = Task.inbox
    let side: List<Filter<Task>> = [Task.inbox, Task.done]
    let chosen = side.find(f -> f == place)
    text(chosen?.label ?? "x")
}
"#,
    );
    let key = crate::types::TypeKey {
        app: "t.x".to_owned(),
        name: "Task".to_owned(),
    };
    let filter = Ty::Filter(key);
    let home = func(&m, "home");
    assert!(
        home.locals.contains(&Ty::List(Box::new(filter.clone()))),
        "the written type is the list it names: {:#?}",
        home.locals
    );
    assert!(
        home.locals
            .iter()
            .any(|t| *t == Ty::Option(Box::new(filter.clone()))),
        "and choosing one asks the list for it: {:#?}",
        home.locals
    );
    assert!(
        !home
            .locals
            .iter()
            .any(|t| *t == Ty::List(Box::new(Ty::Error)) || *t == Ty::Option(Box::new(Ty::Error))),
        "and neither is a list of it: {:#?}",
        home.locals
    );
}

/// The value a function returns.
fn returned(f: &Func) -> &Expr {
    f.body
        .iter()
        .find_map(|s| match s {
            Stmt::Return(Some(e)) => Some(e),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{} returns nothing", f.name))
}

/// Every host import a function uses, with its arguments.
fn imports(f: &Func) -> Vec<crate::abi::Imp> {
    let mut out = Vec::new();
    for s in &f.body {
        fn walk(e: &Expr, out: &mut Vec<crate::abi::Imp>) {
            match &e.kind {
                Ek::Host(imp, args) => {
                    out.push(*imp);
                    for a in args {
                        walk(a, out);
                    }
                }
                Ek::Call(_, args) => args.iter().for_each(|a| walk(a, out)),
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
                Ek::Seq(inner, v) => {
                    for s in inner {
                        if let Stmt::Return(Some(e)) | Stmt::Eval(e) | Stmt::Set(_, e) = s {
                            walk(e, out);
                        }
                    }
                    walk(v, out);
                }
                _ => {}
            }
        }
        match s {
            Stmt::Set(_, e) | Stmt::Eval(e) | Stmt::Return(Some(e)) => walk(e, &mut out),
            _ => {}
        }
    }
    out
}

#[test]
fn a_length_of_time_is_a_value_of_its_own() {
    let (m, _) = lower(
        "fn span() -> Duration { 2h + 30m }\n\
         fn shorter() -> Duration { 1w - 2d }\n\
         fn seconds(d: Duration) -> Int { d.seconds() }\n\
         fn shown(d: Duration) -> Text { \"{d}\" }\n\
         fn ordered(a: Duration, b: Duration) -> Bool { a < b }\n\
         fn counted(n: Int) -> Duration { Duration(n) }",
    );
    for (name, ty) in [
        ("span", Ty::Duration),
        ("shorter", Ty::Duration),
        ("seconds", Ty::Int),
        ("shown", Ty::Text),
        ("ordered", Ty::Bool),
        ("counted", Ty::Duration),
    ] {
        assert_eq!(returned(func(&m, name)).ty, ty, "{name}");
    }
    // The length shows in the largest unit that divides it, which only the runtime knows.
    assert!(imports(func(&m, "shown")).contains(&Imp::DurationText));
    // A day and a moment move by one, and two of a kind are one apart.
    let (m, _) = lower(
        "type Note {\n    stamp: Date = Date(\"1970-01-01\")\n    at: Time = Time(\"1970-01-01T00:00:00Z\")\n}\n\
         action soon(n: Note) -> Time uses time { n.at = now + 2h \n now }\n\
         action tomorrow(n: Note) -> Date uses time { n.stamp = today + 1d \n today }\n\
         action last_week(n: Note) -> Date uses time { today - 1w }\n\
         fn apart(a: Time, b: Time) -> Duration { b - a }\n\
         fn days(a: Date, b: Date) -> Duration { b - a }",
    );
    for (name, want) in [
        ("soon", Ty::Time),
        ("tomorrow", Ty::Date),
        ("last_week", Ty::Date),
        ("apart", Ty::Duration),
        ("days", Ty::Duration),
    ] {
        assert_eq!(returned(func(&m, name)).ty, want, "{name}");
    }

    for (body, wants) in [
        // A day moves by whole days: an afternoon cannot move it.
        (
            "type N {\n    d: Date = Date(\"1970-01-01\")\n}\naction late(n: N) { n.d = today + 2h }",
            "whole days",
        ),
        (
            "type N {\n    d: Date = Date(\"1970-01-01\")\n}\naction late(n: N) { n.d = today + 1 }",
            "length of time",
        ),
        // A length and a number are not one another; `Duration(n)` reads a number as one.
        ("fn mix() -> Duration { 2h + 3 }", "Duration(n)"),
        ("fn mix() -> Duration { 2h + \"x\" }", "two Durations"),
        // A stored length of time is a number the app reads in its own unit.
        ("type S { hold: Duration }", "cannot hold a `Duration`"),
    ] {
        let found = diagnostics(body);
        assert!(
            found.iter().any(|d| d.message.contains(wants)),
            "{body}: {found:#?}"
        );
    }
}

#[test]
fn a_day_and_a_moment_are_read_by_their_parts() {
    let (m, _) = lower(
        "fn weekday(d: Date) -> Text {\n    match d.weekday() {\n        .monday => \"Mon\",\n        .tuesday => \"Tue\",\n        .wednesday => \"Wed\",\n        .thursday => \"Thu\",\n        .friday => \"Fri\",\n        .saturday => \"Sat\",\n        .sunday => \"Sun\",\n    }\n}\n\
         fn part(d: Date) -> Int { d.day() }\n\
         fn month(d: Date) -> Int { d.month() }\n\
         fn year(d: Date) -> Int { d.year() }\n\
         fn week(d: Date) -> Date { d.start_of_week() }\n\
         fn day(m: Time) -> Date { m.day() }\n\
         fn at(m: Time) -> Duration { m.time_of_day() }",
    );
    for (name, ty) in [
        ("weekday", Ty::Text),
        ("part", Ty::Int),
        ("month", Ty::Int),
        ("year", Ty::Int),
        ("week", Ty::Date),
        ("day", Ty::Date),
        ("at", Ty::Duration),
    ] {
        assert_eq!(returned(func(&m, name)).ty, ty, "{name}");
    }
    assert!(imports(func(&m, "weekday")).contains(&Imp::DayWeekday));
    assert!(imports(func(&m, "day")).contains(&Imp::TimeDay));
    assert!(imports(func(&m, "at")).contains(&Imp::TimeOfDay));
    for (body, wants) in [
        ("fn w(d: Date) -> Bool { d.fortnight() }", "start_of_week"),
        ("fn w(d: Date) -> Bool { d.parse(\"x\") }", "Date.parse"),
        ("fn w() -> Bool { Date.weekday() }", "a type, not a value"),
    ] {
        let found = diagnostics(body);
        assert!(
            found.iter().any(|d| d.message.contains(wants)),
            "{body}: {found:#?}"
        );
    }
}

#[test]
fn a_day_or_a_moment_is_read_from_a_text() {
    let (m, _) = lower(
        "action day(t: Text) -> Option<Date> uses time { Date.parse(t, near: now) }\n\
         action at(t: Text) -> Option<Time> uses time { Time.parse(t, near: now) }",
    );
    assert_eq!(returned(func(&m, "day")).ty, Ty::Option(Box::new(Ty::Date)));
    assert_eq!(returned(func(&m, "at")).ty, Ty::Option(Box::new(Ty::Time)));
    assert!(imports(func(&m, "day")).contains(&Imp::DayParse));
    assert!(imports(func(&m, "at")).contains(&Imp::TimeParse));

    for (body, wants) in [
        // Words are read against the moment they are said at, named.
        ("action d(t: Text) { Date.parse(t) }", "near"),
        ("action d(t: Text) { Date.parse(t, now) }", "near"),
        ("action d() { Date.parse(2, near: now) }", "Text"),
    ] {
        let found = diagnostics(body);
        assert!(
            found.iter().any(|d| d.message.contains(wants)),
            "{body}: {found:#?}"
        );
    }
}

#[test]
fn only_an_action_reads_the_day_it_is() {
    for name in ["today", "now"] {
        let found = diagnostics(&format!("fn d() -> Bool {{ {name} == {name} }}"));
        let said = found
            .iter()
            .find(|d| d.code == "E049")
            .unwrap_or_else(|| panic!("{name} is an action's: {found:#?}"));
        assert!(
            said.message.contains(name) && said.message.contains("only an action"),
            "{name}: {}",
            said.message
        );
    }
    // The action's day and moment read as one: the day is the one the moment falls on.
    let (m, _) = lower(
        "type Note {\n    stamp: Date = Date(\"1970-01-01\")\n}\naction mark(n: Note) uses time { n.stamp = now.day() }",
    );
    assert!(imports(func(&m, "mark")).contains(&Imp::TimeDay));
}

/// A file is a value: a field holds one, an action takes one as its parameter, its parts read
/// on it, an action makes one of what it holds, and a view asks the person for one and runs an
/// action with it.
#[test]
fn a_file_is_a_field_a_parameter_and_the_persons_choice() {
    let (m, _) = lower(
        "type N {\n    body: RichText\n    picture: Option<File>\n}\n
         action put(n: N, f: File) (did: \"added a file\") {\n    n.picture = f\n    n.body = RichText(f.name, f.url, \"{f.size}\")\n}\n
         action read(n: N, f: File) {\n    match f.text() {\n        .ok(words) => { n.body = RichText(words) },\n        .err(why) => { n.body = RichText(why) }\n    }\n}\n
         action write(n: N) -> File {\n    File.of_text(\"n.md\", n.body.text)\n}\n
         action archive(n: N) -> File {\n    File.zip(\"n.zip\", [File.of_text(\"a.md\", n.body.text)])\n}\n
         action bytes(n: N) -> File uses web {\n    File.of_bytes(\"n.html\", web.get(\"https://example.com\").body)\n}\n
         action again(n: N) -> File {\n    File.at(\"n.png\", n.picture?.url ?? \"\")\n}\n
         action export(n: N) -> Text {\n    n.body.text\n}\n
         view v(n: N) {\n    stack {\n        button(\"Put\", on: file.choose(put(n), accept: [\"image/png\"]))\n        button(\"Save\", on: file.save(export(n), name: \"n.md\"))\n    }\n}",
    );
    let put = imports(func(&m, "put"));
    assert!(put.contains(&Imp::FilePart), "its parts: {put:?}");
    assert!(put.contains(&Imp::FileSize), "its size: {put:?}");
    assert!(imports(func(&m, "read")).contains(&Imp::FileText));
    assert!(imports(func(&m, "write")).contains(&Imp::FileOfText));
    assert!(imports(func(&m, "archive")).contains(&Imp::FileZip));
    assert!(imports(func(&m, "bytes")).contains(&Imp::FileOfBytes));
    assert!(imports(func(&m, "again")).contains(&Imp::FileAt));
    let view = imports(func(&m, "v"));
    assert!(view.contains(&Imp::ActChoose), "the chooser: {view:?}");
    assert!(view.contains(&Imp::ActSave), "the save: {view:?}");
    // The parameter is the file itself: no form asks for it, the person's chooser fills it.
    assert!(!Ty::File.asked_by_form());
}
