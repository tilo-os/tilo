//! search.md, "What a type declares" and "Types, references and paths": a type's `search` block,
//! its defaults, identifiers, and `query(T, ...)` checked against the type's filters.

use crate::common::{codes, compile_body, compile_with};
use tilolang::meta::{FilterKind, SortOrder, TypeMeta};
use tilolang::pred::{CmpOp, Lit, Operand, Pred};
use tilolang::{parse, print_module};

const MAIL: &str = r#"/// A message in a mailbox.
type Message {
    subject: Text
    body: Text
    /// Who sent it.
    from: std.Identifier
    to: Set<std.Identifier>
    received: Time
    unread: Bool = true
    labels: Set<Label>
    attachments: Collection<File>

    search {
        filter unread "Unread" (icon: .mail)
        filter from
        filter received
        filter labels
        filter "Has attachment" = not attachments.is_empty() (icon: .paperclip)
        sort received newest
        hide to
    }
}

/// A label a message can carry.
type Label {
    name: Text
}

/// A file attached to a message.
type File {
    name: Text
}

/// The messages of one account.
type Mailbox {
    messages: Collection<Message>
}

"#;

fn type_meta<'m>(meta: &'m tilolang::PackageMeta, name: &str) -> &'m TypeMeta {
    meta.types
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("no type {name}"))
}

fn field_id(ty: &TypeMeta, name: &str) -> String {
    ty.fields
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no field {name}"))
        .id
        .clone()
}

#[test]
fn a_search_block_declares_the_filters_in_order_the_sort_and_the_hidden_fields() {
    let meta = compile_body(MAIL).meta;
    let message = type_meta(&meta, "Message");
    let search = &message.search;
    assert!(search.declared);
    let keys: Vec<&str> = search.filters.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(
        keys,
        ["unread", "from", "received", "labels", "has_attachment"]
    );
    let labels: Vec<&str> = search.filters.iter().map(|f| f.label.as_str()).collect();
    assert_eq!(
        labels,
        ["Unread", "From", "Received", "Labels", "Has attachment"]
    );
    let kinds: Vec<FilterKind> = search.filters.iter().map(|f| f.kind).collect();
    assert_eq!(
        kinds,
        [
            FilterKind::Bool,
            FilterKind::Identifier,
            FilterKind::Time,
            FilterKind::Reference,
            FilterKind::Predicate
        ]
    );
    // A filter's icon is the one its declaration names.
    assert_eq!(
        search.filter("unread").and_then(|f| f.icon.as_deref()),
        Some("mail")
    );
    assert_eq!(
        search
            .filter("has_attachment")
            .and_then(|f| f.icon.as_deref()),
        Some("paperclip")
    );
    assert_eq!(search.filter("from").and_then(|f| f.icon.clone()), None);
    let from = search.filter("from").expect("from");
    assert_eq!(from.doc, "Who sent it.");
    assert_eq!(
        from.field.as_deref(),
        Some(field_id(message, "from").as_str())
    );
    let labels = search.filter("labels").expect("labels");
    assert!(labels.many && labels.optional, "a set may hold none");
    // The named filter is a predicate over the message's own fields, by id.
    assert_eq!(
        search.filter("has_attachment").and_then(|f| f.pred.clone()),
        Some(Pred::Not(Box::new(Pred::Cmp(
            CmpOp::Eq,
            Operand::Len(field_id(message, "attachments")),
            Operand::Lit(Lit::Int(0)),
        ))))
    );
    let sort = search.sort.as_ref().expect("a sort");
    assert_eq!(
        (sort.key.as_str(), sort.order),
        ("received", SortOrder::Newest)
    );
    assert_eq!(search.hidden, [field_id(message, "to")]);
    // It prints as it was written.
    let src = format!("app t.x \"X\"\n\n{MAIL}");
    let printed = print_module(&parse(&src).expect("parses"));
    assert!(
        printed.contains("    search {\n        filter unread \"Unread\" (icon: .mail)\n"),
        "{printed}"
    );
    assert!(
        printed.contains("not attachments.is_empty() (icon: .paperclip)"),
        "{printed}"
    );
    assert_eq!(print_module(&parse(&printed).expect("parses")), printed);
}

#[test]
fn without_a_search_block_every_comparable_field_is_a_filter_and_the_first_time_the_sort() {
    let meta = compile_body(
        r"/// Something to do.
type Todo {
    title: Text
    done: Bool = false
    /// The day it is due by.
    due: Option<Date> = .none
    notes: Text
    tags: Set<Text>
    priority: Priority
    effort: Int
    owner: Option<Person>
    created: Time
    changed: Time
    counts: Map<Int>
    contact: std.Identifier
}

enum Priority {
    low
    high
}

type Person {
    name: Text
}",
    )
    .meta;
    let todo = type_meta(&meta, "Todo");
    let search = &todo.search;
    assert!(!search.declared);
    let keys: Vec<&str> = search.filters.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "done", "due", "tags", "priority", "effort", "owner", "created", "changed", "contact"
        ],
        "text is searched, not filtered; a map is neither"
    );
    let due = search.filter("due").expect("due");
    assert_eq!(
        (due.kind, due.optional, due.label.as_str()),
        (FilterKind::Date, true, "Due")
    );
    assert_eq!(
        search.filter("tags").map(|f| f.kind),
        Some(FilterKind::Text)
    );
    assert_eq!(
        search.filter("owner").map(|f| f.kind),
        Some(FilterKind::Reference)
    );
    let sort = search.sort.as_ref().expect("the first Time");
    assert_eq!(
        (sort.key.as_str(), sort.order),
        ("created", SortOrder::Newest)
    );
    // Search's own words are no filters.
    let meta =
        compile_body("type Thing {\n    sort: Int\n    is: Bool = false\n    n: Int\n}").meta;
    let keys: Vec<String> = type_meta(&meta, "Thing")
        .search
        .filters
        .iter()
        .map(|f| f.key.clone())
        .collect();
    assert_eq!(keys, ["n"]);
}

/// What a type is called is its name in words, one of it and several as English writes them: an irregular
/// plural is English's. A search block says nothing about it, and a query naming an app means its root.
#[test]
fn a_type_is_called_by_its_name() {
    let src = r"/// A mailbox.
type Mailbox {
    name: Text
}

/// A list of things to do.
type TodoList {
    name: Text
}

/// A box of entries.
type Entry {
    text: Text
}

/// The workspace's tasks.
type Tasks {
    items: Collection<TodoList>
}

/// Someone here.
type Person {
    name: Text
}

/// The day at a glance.
type Today {
}";
    let meta = compile_body(src).meta;
    let nouns = |name: &str| {
        let ty = type_meta(&meta, name);
        (ty.noun(), ty.plural())
    };
    assert_eq!(nouns("Mailbox"), ("mailbox".into(), "mailboxes".into()));
    assert_eq!(nouns("TodoList"), ("todo list".into(), "todo lists".into()));
    assert_eq!(nouns("Entry"), ("entry".into(), "entries".into()));
    assert_eq!(
        nouns("Tasks"),
        ("task".into(), "tasks".into()),
        "a name that already names several is one of them and all of them"
    );
    assert_eq!(
        nouns("Person"),
        ("person".into(), "people".into()),
        "an irregular plural is English's"
    );
    assert_eq!(
        nouns("Today"),
        ("today".into(), "today".into()),
        "a word English counts no number of keeps one form"
    );
    for line in ["noun \"person\" \"people\"", "primary name == \"Inbox\""] {
        let body =
            format!("type Box {{\n    name: Text\n\n    search {{\n        {line}\n    }}\n}}");
        let err = match compile_with(&format!("app t.x \"X\"\n{body}\n"), &[]) {
            Ok(_) => String::new(),
            Err(diags) => diags
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        };
        assert!(
            err.contains("a search block holds `filter`, `sort` and `hide` lines"),
            "`{line}` is not a search line: {err}"
        );
    }
}

fn first_message(body: &str) -> String {
    let src = format!("app t.x \"X\"\n{body}\n");
    match compile_with(&src, &[]) {
        Ok(_) => panic!("expected an error for:\n{body}"),
        Err(d) => d
            .into_iter()
            .map(|d| format!("{} {}", d.code, d.message))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

#[test]
fn a_bad_declaration_is_an_error_that_names_the_type_by_its_doc() {
    let ty = |search: &str| {
        format!(
            "/// Something to do.\ntype Todo {{\n    title: Text\n    done: Bool = false\n    due: Option<Date> = .none\n    tags: Set<Text>\n    owner: Option<Todo>\n\n    search {{\n{search}\n    }}\n}}"
        )
    };
    let cases = [
        (
            "        filter dne",
            "E100",
            "`Todo` (Something to do.) has no field `dne`",
        ),
        (
            "        filter title",
            "E101",
            "text is found by searching for it",
        ),
        (
            "        filter \"Late\" = due < today + 3h",
            "E102",
            "moves by whole days",
        ),
        (
            "        filter \"Theirs\" = owner.done",
            "E102",
            "reads another resource",
        ),
        (
            "        filter \"Odd\" = nope",
            "E102",
            "`nope` is not a field of `Todo`",
        ),
        (
            "        filter \"Tagged\" = tags == \"x\"",
            "E102",
            "`tags` is Set<Text>",
        ),
        ("        filter \"Open\" = title", "E102", "not a Bool"),
        ("        sort done newest", "E103", "which has no order"),
        (
            "        sort due a_z",
            "E103",
            "it sorts `newest` or `oldest`",
        ),
        ("        sort due latest", "E103", "is not an order"),
        (
            "        filter done (colour: .red)",
            "E090",
            "takes the option `icon:`",
        ),
        ("        filter done (icon: .nope)", "E090", "not an icon"),
        (
            "        filter done\n        filter done",
            "E104",
            "each filter is declared once",
        ),
        ("        filter done\n        hide done", "E104", "keep one"),
        (
            "        sort due newest\n        sort due oldest",
            "E104",
            "one default order",
        ),
        (
            "        filter \"Sort\" = done",
            "E104",
            "search's own word",
        ),
    ];
    for (search, code, says) in cases {
        let got = first_message(&ty(search));
        assert!(
            got.contains(code) && got.contains(says),
            "{search}: expected {code} saying {says:?}, got:\n{got}"
        );
    }
    assert!(codes(&ty("        find done")).contains(&"E003".to_owned()));
    assert!(codes(&ty("        filter \"Open\" = not done\n        filter done\n        filter due\n        filter tags\n        filter owner\n        sort due oldest")).is_empty());
}

#[test]
fn an_identifier_is_held_as_its_uri_from_a_literal_checked_when_compiled() {
    let meta = compile_body(
        "type Contact {\n    email: std.Identifier = std.Identifier(\"Jane@YC.com\")\n    phones: Set<std.Identifier>\n}\nfn scheme(c: Contact) -> Text {\n    return c.email.scheme\n}\nfn domain(c: Contact) -> Bool {\n    return c.email == std.Identifier(\"mailto:jane@yc.com\")\n}",
    )
    .meta;
    let contact = type_meta(&meta, "Contact");
    let email = contact
        .fields
        .iter()
        .find(|f| f.name == "email")
        .expect("email");
    assert_eq!(email.ty, "std.Identifier");
    assert_eq!(email.default, Some(serde_json::json!("mailto:jane@yc.com")));
    let kinds: Vec<(String, FilterKind, bool)> = contact
        .search
        .filters
        .iter()
        .map(|f| (f.key.clone(), f.kind, f.many))
        .collect();
    assert_eq!(
        kinds,
        [
            ("email".into(), FilterKind::Identifier, false),
            ("phones".into(), FilterKind::Identifier, true)
        ]
    );
    let got = first_message("type C {\n    e: std.Identifier = std.Identifier(\"janedoe\")\n}");
    assert!(
        got.contains("E099") && got.contains("not an identifier"),
        "{got}"
    );
    let got = first_message("fn f() -> Text {\n    return std.Identifier(\"tel:+49\").country\n}");
    assert!(
        got.contains("E021") && got.contains("`scheme` and `value`"),
        "{got}"
    );
    assert!(codes("type C {\n    e: std.Address\n}").contains(&"E013".to_owned()));
}

const TODOS: &str = r#"/// Something to do.
type Todo {
    title: Text
    done: Bool = false
    due: Option<Date> = .none
    status: Status
    effort: Int
    contact: std.Identifier
    tags: Set<Text>

    search {
        filter "Open" = not done
        filter done
        filter due
        filter status
        filter effort
        filter contact
        filter tags
        sort due oldest
    }
}

enum Status {
    waiting
    started
    blocked
}

type TodoList {
    items: Collection<Todo>
    later: Collection<Todo>
}

type Inbox {
    items: Collection<Todo>
}
"#;

#[test]
fn a_query_sets_the_types_filters_with_values_written_as_a_typed_search_writes_them() {
    let compiled = compile_body(&format!(
        "{TODOS}
view soon(box: Inbox, n: Int) {{
    for t in query(Todo, in: box, open: true, due: today..today + 7d, status: [.waiting, .blocked], effort: n..20, contact: \"*@YC.com\", tags: \"home\", sort: .effort_newest) {{
        text(t.title)
    }}
    for t in query(Todo, in: box.items, due: -7d, tags: none) {{
        text(t.title)
    }}
}}

action overdue(l: TodoList) -> Int {{
    return query(Todo, due: ..today - 1d, done: false).len()
}}"
    ));
    let data = String::from_utf8_lossy(&compiled.wasm);
    for want in [
        "today+7d",
        "waiting,blocked",
        "mailto:*@yc.com",
        "-effort",
        "-7d",
        "none",
        "today-1d",
        "home",
    ] {
        assert!(data.contains(want), "{want} is among the module's texts");
    }
}

#[test]
fn a_query_that_does_not_fit_the_types_filters_is_an_error() {
    let cases = [
        (
            "query(Todo, in: box, nope: true)",
            "E105",
            "has no filter `nope`",
        ),
        (
            "query(Todo, in: box, status: .done)",
            "E105",
            "has no case `.done`",
        ),
        (
            "query(Todo, in: box, contact: \"janedoe\")",
            "E105",
            "not an identifier",
        ),
        ("query(Todo, in: box, done: 3)", "E020", "expected Bool"),
        (
            "query(Todo, in: box, effort: \"x\")",
            "E105",
            "a number filter",
        ),
        (
            "query(Todo, in: box, due: 3)",
            "E105",
            "takes a Date or a Time",
        ),
        (
            "query(Todo, in: box, sort: .title_newest)",
            "E105",
            "not an order of `Todo`",
        ),
        (
            "query(Todo, in: box, done: true, done: false)",
            "E105",
            "set twice",
        ),
        (
            "query(Todo, in: box, contact: none)",
            "E105",
            "always holds a value",
        ),
        (
            "query(Todo, done: false)",
            "E105",
            "looks inside a resource",
        ),
        ("query(Todo, in: l)", "E105", "holds several collections"),
        ("query(Todo, in: n)", "E105", "`in:` takes a resource"),
        ("query(n, in: box)", "E105", "takes a record type first"),
    ];
    for (q, code, says) in cases {
        let got = first_message(&format!(
            "{TODOS}\nview v(box: Inbox, l: TodoList, n: Int) {{\n    for t in {q} {{\n        text(t.title)\n    }}\n}}"
        ));
        assert!(
            got.contains(code) && got.contains(says),
            "{q}: expected {code} saying {says:?}, got:\n{got}"
        );
    }
    // A length of time is a value of its own, so `7d` is no Int; a stored number of seconds is read as one
    // with `Duration(n)`.
    let compile_at = |body: &str| compile_with(&format!("app t.x \"X\"\n{body}\n"), &[]);
    let got = first_message("fn f() -> Int {\n    return 7d\n}");
    assert!(
        got.contains("E020") && got.contains("found Duration") && got.contains("`.seconds()`"),
        "`7d` is a Duration where an Int goes, got:\n{got}"
    );
    assert!(
        compile_at("fn f(n: Int) -> Duration {\n    return Duration(n)\n}").is_ok(),
        "`Duration(n)` reads a number of seconds as a length of time"
    );
    let got = first_message("fn f(a: Int) -> Int {\n    return a..3\n}");
    assert!(got.contains("E105") && got.contains("a range"), "{got}");
}

#[test]
fn a_named_filter_compares_a_field_with_the_time_it_is_asked_at() {
    let meta = compile_body(
        r#"/// Something to do.
type Todo {
    title: Text
    done: Bool = false
    due: Option<Date>
    seen: Option<Time>

    search {
        filter "Today" = not done and due != .none and due <= today (icon: .star)
        filter "This week" = due > today and due <= today + 7d
        filter "Seen lately" = seen >= now - 1h
        filter "Unplanned" = due == .none (icon: .inbox)
    }
}

/// A list.
type Todos {
    items: Collection<Todo>
}
"#,
    )
    .meta;
    let todo = type_meta(&meta, "Todo");
    let pred = |key: &str| {
        todo.search
            .filter(key)
            .and_then(|f| f.pred.clone())
            .unwrap_or_else(|| panic!("no filter {key}"))
    };
    let due = field_id(todo, "due");
    let today = Pred::Cmp(
        CmpOp::Le,
        Operand::Field(due.clone()),
        Operand::Clock {
            day: true,
            offset: 0,
        },
    );
    let asked = pred("today");
    assert_eq!(asked.conjuncts().last().copied(), Some(&today), "{asked:?}");
    assert!(asked.reads_clock());
    // A case before the options is the case alone, and they are the filter's.
    assert_eq!(
        pred("unplanned"),
        Pred::Not(Box::new(Pred::Present(due.clone())))
    );
    assert_eq!(
        todo.search
            .filter("unplanned")
            .and_then(|f| f.icon.as_deref()),
        Some("inbox")
    );
    assert_eq!(
        todo.search.filter("today").and_then(|f| f.icon.as_deref()),
        Some("star")
    );
    // Read when it is asked: on 2026-10-29 at 10:00, the week ends on 2026-11-05.
    let at = tilolang::calendar::instant("2026-10-29T10:00:00Z").expect("a time");
    assert_eq!(
        pred("this_week").at(at, 0),
        Pred::And(vec![
            Pred::Cmp(
                CmpOp::Gt,
                Operand::Field(due.clone()),
                Operand::Lit(Lit::Date("2026-10-29".into()))
            ),
            Pred::Cmp(
                CmpOp::Le,
                Operand::Field(due),
                Operand::Lit(Lit::Date("2026-11-05".into()))
            ),
        ])
    );
    assert_eq!(
        pred("seen_lately").at(at, 0),
        Pred::Cmp(
            CmpOp::Ge,
            Operand::Field(field_id(todo, "seen")),
            Operand::Lit(Lit::Time("2026-10-29T09:00:00Z".into()))
        )
    );
}

const PLACES: &str = r#"/// A thing to do.
type Task {
    title: Text
    done: Bool = false
    due: Option<Date> = .none
    project: Option<Project> = .none

    search {
        filter "Inbox" = project == .none and due == .none and not done (icon: .inbox)
        filter "Today" = due <= today and not done (icon: .star)
        filter "Upcoming" = due > today and not done (icon: .calendar)
        filter "Done" = done (icon: .circle_check)
        filter project
        sort due oldest
    }
}

/// What tasks are filed under.
type Project {
    name: Text
    tasks: Collection<Task>
}

/// Tasks, in one place.
type Board {
    tasks: Collection<Task>
}
"#;

#[test]
fn a_named_filter_is_the_predicate_it_writes() {
    let meta = compile_body(PLACES).meta;
    let task = type_meta(&meta, "Task");
    let keys: Vec<&str> = task.search.filters.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, ["inbox", "today", "upcoming", "done", "project"]);
    let due = field_id(task, "due");
    let done = field_id(task, "done");
    let project = field_id(task, "project");
    let open = Pred::Cmp(
        CmpOp::Eq,
        Operand::Field(done.clone()),
        Operand::Lit(Lit::Bool(true)),
    );
    let place = |key: &str| {
        let filter = task
            .search
            .filter(key)
            .unwrap_or_else(|| panic!("no filter {key}"));
        assert_eq!(filter.kind, FilterKind::Predicate, "{key}");
        assert!(filter.field.is_none(), "{key} is on no field");
        filter.pred.clone().unwrap_or_else(|| panic!("{key}"))
    };
    // What is not sorted yet: open, filed under nothing and for no day.
    assert_eq!(
        place("inbox"),
        Pred::And(vec![
            Pred::And(vec![
                Pred::Not(Box::new(Pred::Present(project))),
                Pred::Not(Box::new(Pred::Present(due.clone()))),
            ]),
            Pred::Not(Box::new(open.clone())),
        ])
    );
    let today = Operand::Clock {
        day: true,
        offset: 0,
    };
    assert_eq!(
        place("today"),
        Pred::And(vec![
            Pred::Cmp(CmpOp::Le, Operand::Field(due.clone()), today.clone()),
            Pred::Not(Box::new(open.clone())),
        ])
    );
    assert_eq!(
        place("upcoming"),
        Pred::And(vec![
            Pred::Cmp(CmpOp::Gt, Operand::Field(due), today),
            Pred::Not(Box::new(open)),
        ])
    );
    assert_eq!(
        place("done"),
        Pred::Cmp(
            CmpOp::Eq,
            Operand::Field(done),
            Operand::Lit(Lit::Bool(true))
        )
    );
    // A field filter written beside them is still a field filter.
    let project_filter = task.search.filter("project").expect("project");
    assert_eq!(project_filter.kind, FilterKind::Reference);
}

#[test]
fn a_filter_takes_the_option_icon() {
    let meta = compile_body(PLACES).meta;
    let task = type_meta(&meta, "Task");
    assert_eq!(
        task.search.filter("today").and_then(|f| f.icon.clone()),
        Some("star".to_owned())
    );
    let unknown = first_message(
        r"/// Something to do.
type Todo {
    title: Text
    done: Bool = false

    search {
        filter done (tone: .danger)
    }
}
",
    );
    assert!(
        unknown.contains("E090") && unknown.contains("a filter takes the option `icon:`"),
        "{unknown}"
    );
}

#[test]
fn a_field_is_read_as_the_field_it_is_whatever_it_is_called() {
    let meta = compile_body(
        r#"/// A thing to do.
type Task {
    title: Text
    inbox: Bool = false

    search {
        filter "Filed" = inbox
    }
}
"#,
    )
    .meta;
    let task = type_meta(&meta, "Task");
    let filed = task.search.filter("filed").expect("filed");
    assert_eq!(
        filed.pred,
        Some(Pred::Cmp(
            CmpOp::Eq,
            Operand::Field(field_id(task, "inbox")),
            Operand::Lit(Lit::Bool(true))
        )),
        "a field called `inbox` is a field"
    );
}

/// A field naming who an item belongs to is a filter of its own: `assignee:me` reads the same in every app
/// that has one.
const MEMBERS: &str = r"
/// A task someone does.
type Task {
    title: Text
    done: Bool = false
    /// Who is doing it.
    assignee: Option<Principal> = .none
}

/// The tasks a workspace shares.
type Board {
    tasks: Collection<Task>
}
";

#[test]
fn a_field_naming_a_member_is_a_filter_without_declaring_one() {
    // `me` reads as the person looking rather than as a binding, so the view compiles.
    let meta = compile_body(&format!(
        "{MEMBERS}
view mine(b: Board) {{
    for t in query(Task, in: b.tasks, assignee: me) {{
        text(t.title)
    }}
}}
"
    ))
    .meta;
    let task = type_meta(&meta, "Task");
    let assignee = task
        .search
        .filter("assignee")
        .expect("the member field is a filter");
    assert_eq!(assignee.kind, FilterKind::Person);
    assert!(assignee.optional, "nobody may hold it");
    assert_eq!(assignee.label, "Assignee");
    assert_eq!(
        assignee.field.as_deref(),
        Some(field_id(task, "assignee").as_str())
    );
}

#[test]
fn a_named_filter_reads_the_person_it_is_asked_for() {
    let meta = compile_body(&format!(
        "{MEMBERS}
/// What the people of a workspace hold between them.
type Shared {{
    title: Text
    done: Bool = false
    /// Who is doing it.
    assignee: Option<Principal> = .none

    search {{
        filter \"Mine\" = assignee == workspace.me and not done
    }}
}}
"
    ))
    .meta;
    let shared = type_meta(&meta, "Shared");
    let mine = shared.search.filter("mine").expect("mine");
    let pred = mine.pred.as_ref().expect("a predicate");
    let done = field_id(shared, "done");
    assert_eq!(
        pred,
        &Pred::And(vec![
            Pred::Cmp(
                CmpOp::Eq,
                Operand::Field(field_id(shared, "assignee")),
                Operand::Viewer
            ),
            Pred::Not(Box::new(Pred::Cmp(
                CmpOp::Eq,
                Operand::Field(done),
                Operand::Lit(Lit::Bool(true))
            ))),
        ]),
        "the filter asks which item's assignee is the person looking"
    );
    assert!(pred.reads_viewer(), "it is asked for someone");
    assert_eq!(
        pred.viewer("ben"),
        Pred::And(vec![
            Pred::Cmp(
                CmpOp::Eq,
                Operand::Field(field_id(shared, "assignee")),
                Operand::Lit(Lit::Principal("ben".to_owned()))
            ),
            Pred::Not(Box::new(Pred::Cmp(
                CmpOp::Eq,
                Operand::Field(field_id(shared, "done")),
                Operand::Lit(Lit::Bool(true))
            ))),
        ]),
        "and reads for whoever it is asked for"
    );
}

#[test]
fn the_member_filter_is_declared_once_or_hidden_away() {
    // Declaring it names it, without making a second one.
    let meta = compile_body(&format!(
        "{MEMBERS}
type Named {{
    title: Text
    /// Who is doing it.
    assignee: Option<Principal> = .none

    search {{
        filter assignee
    }}
}}
"
    ))
    .meta;
    let named = type_meta(&meta, "Named");
    let people: Vec<&tilolang::meta::FilterMeta> = named
        .search
        .filters
        .iter()
        .filter(|f| f.kind == FilterKind::Person)
        .collect();
    assert_eq!(people.len(), 1, "one member filter, declared: {people:?}");
    assert_eq!(people[0].key, "assignee");
    // Hiding it keeps it out, as hiding any field does.
    let meta = compile_body(&format!(
        "{MEMBERS}
type Quiet {{
    title: Text
    /// Who is doing it.
    assignee: Option<Principal> = .none

    search {{
        hide assignee
    }}
}}
"
    ))
    .meta;
    let quiet = type_meta(&meta, "Quiet");
    assert!(
        quiet.search.filter("assignee").is_none(),
        "a hidden member field is no filter"
    );
}
