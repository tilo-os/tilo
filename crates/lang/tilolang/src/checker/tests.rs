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
enum NoteOrder {\n    created\n    title\n}\n\
root Notebook\ndefault view notebook for Notebook\n\
fn listed(nb: Notebook, order: NoteOrder) -> List<Note> {\n    return match order {\n        .created => nb.notes.filter(n -> true),\n        .title => nb.notes.sort_by(n -> n.title),\n    }\n}\n\
view notebook(nb: Notebook) {\n    window order: NoteOrder = .created\n    std.list(nb, listed(nb, order), sort: order, empty: \"No notes yet\") { n ->\n        row(n.title, secondary: n.body, mark: if n.pinned { .pin }, checkbox: n.pinned)\n    }\n}";

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
        vec!["node ui.ListRow", "prop title", "prop badge", "prop value"]
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
    // A lambda reading a local of its caller, or text by order, is the loop's alone; so is a
    // list, which is no collection the store keeps. How many members a set holds is the
    // store's to count.
    for loop_only in ["outer", "ordered"] {
        assert!(asked(func(&m, loop_only)).is_empty(), "{loop_only}");
    }
    assert_eq!(asked(func(&m, "tagged")).len(), 1);
    assert_eq!(
        asked(func(&m, "listed")).len(),
        1,
        "the collection's filter alone"
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
fn std_list_is_a_shell_over_a_virtual_list_whose_rows_render_as_shown() {
    let (m, _) = lower(NOTES);
    let calls = ui_calls(func(&m, "notebook"));
    let at = |calls: &[String], want: &str| {
        calls
            .iter()
            .position(|c| c == want || c.starts_with(&format!("{want}:")))
            .unwrap_or_else(|| panic!("no {want} in {calls:#?}"))
    };
    // The library's frame, expanded in the view: a shell, its list one loop node whose rows are
    // each a view of their own, rendered when the host shows them.
    assert!(at(&calls, "node ui.NavShell") < at(&calls, "node ui.VirtualList"));
    let list = at(&calls, "node ui.VirtualList");
    assert!(
        calls[list..]
            .iter()
            .any(|c| c == "node for" || c.starts_with("node for:")),
        "the list holds its loop: {calls:#?}"
    );
    assert!(
        !has(&calls, "node ui.ListItem"),
        "no row is built with the list"
    );
    // The rows' view: the boundary of the list's loop (the header's More menu is one too).
    let row = m
        .funcs
        .iter()
        .filter(|f| f.name.starts_with("notebook for"))
        .map(ui_calls)
        .find(|calls| calls.iter().any(|c| c == "node ui.ListItem"))
        .expect("the rows' view");
    assert!(
        at(&row, "node ui.ListItem") < at(&row, "node ui.ListRow"),
        "{row:#?}"
    );
    for want in [
        "prop title",
        "prop secondary",
        "prop mark",
        "bind",
        "node ui.MenuItem",
        "node ui.SwipeAction",
        "prop presence",
    ] {
        assert!(has(&row, want), "{want} in {row:#?}");
    }
    let s = strings(func(&m, "notebook"));
    assert!(s.contains(&"No notes yet".to_string()));
}

#[test]
fn the_sort_is_a_menu_and_the_item_types_toggles_are_the_sidebars_places() {
    let (m, ids) = lower(NOTES);
    let f = func(&m, "notebook");
    let calls = ui_calls(f);
    assert!(has(&calls, "node ui.NavList"), "the places: {calls:#?}");
    assert!(has(&calls, "node ui.MenuButton"), "the sort menu");
    // The places are the item type's filters, asked of the platform where the list expands;
    // the list as the place chosen shows it, and each place's count, are queries the index
    // answers, of the items the view passed once.
    let note = ids.get("Note").expect("id").to_string();
    let mut filters = Vec::new();
    let mut queries = 0;
    for g in m.funcs.iter().filter(|g| g.name.starts_with("notebook")) {
        visit(g, &mut |e| match &e.kind {
            Ek::Host(Imp::Filters, args) => {
                filters.push(args.first().and_then(text).map(str::to_owned));
            }
            Ek::Host(Imp::Query, _) => queries += 1,
            _ => {}
        });
    }
    assert_eq!(filters, [Some(note)]);
    assert!(queries >= 2, "the list and the places' counts: {queries}");
    let listed = m
        .funcs
        .iter()
        .position(|x| x.name == "listed")
        .expect("listed");
    assert_eq!(calls_of(f, listed), 1, "the items are listed once");
    // Which place is chosen is the window's state, the library's own, kept under the view.
    let notebook = ids.get("notebook").expect("id");
    assert!(
        strings(f).contains(&format!("{notebook}:list.query")),
        "{:?}",
        strings(f)
    );
}

#[test]
fn a_document_page_and_transcript_are_shells_of_the_library() {
    let (m, _) = lower(
        "type Person {\n    name: Text (title)\n    vip: Bool = false\n    owner: Principal\n}\nenum Tab {\n    about\n    notes\n}\nroot Person\ndefault view person for Person\nview person(p: Person) {\n    lens tab: Tab = .about\n    let who = p.name\n    std.document(p) {\n        choice(bind: tab)\n        avatar(p.owner)\n        section(\"About\") {\n            facts {\n                \"Name\": who\n                \"VIP\": p.vip\n            }\n            form {\n                field(\"Name\") {\n                    input(bind: p.name)\n                }\n            }\n            toggle(\"VIP\", bind: p.vip)\n            card(p) {\n                text(p.name, role: .caption)\n            }\n        }\n    }\n}",
    );
    let calls = ui_calls(func(&m, "person"));
    for want in [
        "node ui.NavShell",
        "node ui.Scroll",
        "node page",
        "node choice",
        "on pick.about",
        "node ui.Section",
        "node ui.PropertyList",
        "node form",
        "node field",
        "node toggle",
        "node ui.Card",
        "node ui.Avatar",
        "prop field_role",
    ] {
        assert!(has(&calls, want), "{want} in {calls:#?}");
    }
    assert_eq!(
        calls.first().map(String::as_str),
        Some("node ui.NavShell"),
        "the shell is the root"
    );
    let (m, _) = lower(
        "type Chat {\n    draft: Text\n    lines: Collection<Line>\n}\ntype Line {\n    text: Text\n}\nview line(l: Line) {\n    text(l.text)\n}\nview chat(c: Chat) {\n    lens draft: Text\n    std.transcript(c, c.lines, row: line) {\n        composer: {\n            input(bind: draft)\n        }\n    }\n}",
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
        "type Note {\n    title: Text (title)\n    done: Bool = false\n}\ntype Notebook {\n    notes: Collection<Note>\n}\nroot Notebook\ndefault view notebook for Notebook\naction check(n: Note, done: Bool) {\n    n.done = done\n}\nview notebook(nb: Notebook) {\n    ui.VirtualList {\n        on select(n: Note): open(n, front: false)\n    }\n}\nview note(n: Note) {\n    ui.ListRow(title: n.title) {\n        on activate: open(n)\n    }\n}",
    );
    let mut placed = 0;
    visit(func(&m, "notebook"), &mut |e| {
        if matches!(e.kind, Ek::Host(Imp::EventValue, _)) {
            placed += 1;
        }
    });
    assert_eq!(placed, 1, "the action holds the event's value in its place");
    let wrong = diagnostics(
        "type Note {\n    title: Text (title)\n}\ntype Notebook {\n    notes: Collection<Note>\n}\nview notebook(nb: Notebook) {\n    ui.VirtualList {\n        on select(n: Note): open(nb.notes.first().unwrap_or(n))\n    }\n    ui.Button(\"Go\") {\n        on press(x: Note): open(x)\n    }\n}",
    );
    let codes: Vec<&str> = wrong.iter().map(|d| d.code.as_str()).collect();
    assert!(
        codes.iter().filter(|c| **c == "E063").count() >= 2,
        "{wrong:#?}"
    );
}

#[test]
fn actions_lenses_and_meta_are_values_a_view_reads() {
    let (m, _) = lower(
        "type Note {\n    title: Text (title)\n}\n/// Pins it.\naction pin(n: Note) (icon: .pin) {\n    n.title = n.title\n}\nview note(n: Note) {\n    let offered = actions(n)\n    let here = lenses(n)\n    stack {\n        text(meta(n).title)\n        text(meta(Note).noun)\n        for a in offered {\n            ui.Button(a.label, icon: a.icon, role: .primary) {\n                on press: a.run\n            }\n        }\n        text(\"{here.len()}\")\n    }\n}",
    );
    let mut imports = Vec::new();
    for f in &m.funcs {
        visit(f, &mut |e| {
            if let Ek::Host(imp, _) = &e.kind
                && matches!(
                    imp,
                    Imp::Actions | Imp::Lenses | Imp::MetaOf | Imp::TupleGetH
                )
            {
                imports.push(*imp);
            }
        });
    }
    for want in [Imp::Actions, Imp::Lenses, Imp::MetaOf, Imp::TupleGetH] {
        assert!(imports.contains(&want), "{want:?} in {imports:?}");
    }
    let wrong = diagnostics(
        "type Note {\n    title: Text\n}\nview note(n: Note) {\n    text(actions(n).first().unwrap().nothing)\n}",
    );
    assert!(!wrong.is_empty(), "a host value has the fields it declares");
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
        "type Room {\n    topic: Text\n}\nfn agents(r: Room) -> Int {\n    members(r).count(p -> p.kind() == .agent)\n}\nfn roles() -> List<Text> {\n    workspace.members.map(p -> p.role() ?? p.name())\n}\nview room(r: Room) {\n    stack {\n        for p in workspace.members {\n            avatar(p)\n        }\n        text(\"{agents(r)}\")\n    }\n}",
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
fn every_rich_input_is_given_its_drawers_people_by_the_system() {
    let (m, _) = lower(
        "type Chat {\n    topic: Text\n}\nview chat(c: Chat) {\n    lens draft: RichText = RichText(\"\")\n    stack {\n        rich_input(bind: draft)\n    }\n}\nview loose() {\n    lens draft: RichText = RichText(\"\")\n    rich_input(bind: draft)\n}",
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
        "type Item {\n    title: Text\n}\ntype Bag {\n    items: Collection<Item>\n}\nenum Size {\n    small\n    large\n}\nview shown<T, E>(things: List<T>, size: State<E>, label: (T) -> Text, extra: Option<View> = .none) {\n    stack {\n        for c in cases(E) {\n            button(c.label(), on: set(size, c))\n        }\n        for t in things {\n            text(label(t))\n        }\n        match extra {\n            .some(v) => v,\n            .none => text(\"none\"),\n        }\n    }\n}\nview bag(b: Bag) {\n    window size: Size = .small\n    shown(b.items, size: size, label: i -> i.title)\n}",
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
