//! Queries (search.md, "A query" and "Where search appears"): a typed search and `query(T, ...)`
//! are one query, read against the type's filters, answered over a collection, and kept as its
//! items change.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang::pred::{CmpOp, Lit, Operand, Pred};
    use tilolang_runtime::query::{Clock, Query, Term, find, resolve};
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, SimpleAuthority, UiNode, Value};

    const SOURCE: &str = r#"app so.tilo.tasks "Tasks"

/// Something to do.
type Task {
    title: Text (title)
    done: Bool = false
    due: Option<Date> = .none
    status: Status
    effort: Int
    contact: std.Identifier = std.Identifier("tel:+10")
    tags: Set<Text>

    search {
        filter "Open" = not done
        filter done
        filter due
        filter status
        filter effort
        filter contact
        filter tags
    }
}

enum Status {
    waiting
    started
    blocked
}

/// A message in a mailbox.
type Message {
    subject: Text (title)
    from: std.Identifier = std.Identifier("tel:+10")
    unread: Bool = true
    received: Time = Time("2026-01-01T00:00:00Z")
}

type Board {
    tasks: Collection<Task>
    messages: Collection<Message>
}

action seed(b: Board) {
    let a = b.tasks.insert(Task(title: "a", effort: 3, due: Date("2026-10-27"), status: .waiting, contact: std.Identifier("jane@yc.com")))
    a.tags.add("home")
    b.tasks.insert(Task(title: "b", effort: 8, due: Date("2026-10-20"), status: .blocked, contact: std.Identifier("tel:+4930123")))
    let c = b.tasks.insert(Task(title: "c", effort: 5, status: .started, contact: std.Identifier("bo@acme.com")))
    c.tags.add("work")
    b.tasks.insert(Task(title: "d", effort: 1, done: true, due: Date("2026-11-02"), status: .waiting, contact: std.Identifier("ada@yc.com")))
    b.messages.insert(Message(subject: "fresh from yc", from: std.Identifier("jane@yc.com"), received: Time("2026-10-25T08:00:00Z")))
    b.messages.insert(Message(subject: "old from yc", from: std.Identifier("jane@yc.com"), received: Time("2026-09-01T08:00:00Z")))
    b.messages.insert(Message(subject: "read from yc", from: std.Identifier("bo@yc.com"), unread: false, received: Time("2026-10-26T08:00:00Z")))
    b.messages.insert(Message(subject: "fresh from acme", from: std.Identifier("bo@acme.com"), received: Time("2026-10-26T09:00:00Z")))
}

action check(t: Task) {
    t.done = true
}

view open_by_effort(b: Board) {
    for t in query(Task, in: b, open: true, sort: .effort_newest) {
        text(t.title)
    }
}

view coded(b: Board) {
    for m in query(Message, in: b, from: "*@yc.com", unread: true, received: -7d) {
        text(m.subject)
    }
}

view typed(b: Board, q: Text) {
    for t in b.tasks.matching(q) {
        text(t.title)
    }
}

view typed_mail(b: Board, q: Text) {
    for m in b.messages.matching(q) {
        text(m.subject)
    }
}

view sorted(b: Board, q: Text) {
    for t in b.tasks.sort_by(t -> t.title).matching(q) {
        text(t.title)
    }
}
"#;

    const NOW: &str = "2026-10-27T10:00:00Z";

    struct Tasks {
        rt: Runtime,
        app: App,
        board: ResourceRef,
    }

    impl Tasks {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            rt.set_clock(|| NOW.to_owned());
            app.load(&rt);
            let board = rt.create("Board").unwrap();
            let t = Self { rt, app, board };
            t.call("seed", vec![Value::resource(&t.board)]);
            t
        }

        fn call(&self, action: &str, args: Vec<Value>) {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap();
        }

        fn view(&self, name: &str, q: Option<&str>) -> tilolang_runtime::ViewInstance {
            let mut args = vec![Value::resource(&self.board)];
            if let Some(q) = q {
                args.push(Value::text(q));
            }
            self.rt
                .open_view(&me(), &self.app.view(name), args)
                .unwrap()
        }

        fn shows(&self, name: &str, q: Option<&str>) -> Vec<String> {
            texts(self.view(name, q).tree())
        }

        fn ty(&self, name: &str) -> &tilolang::meta::TypeMeta {
            self.app
                .meta()
                .types
                .iter()
                .find(|t| t.name == name)
                .unwrap()
        }

        fn field(&self, ty: &str, name: &str) -> String {
            self.ty(ty)
                .fields
                .iter()
                .find(|f| f.name == name)
                .unwrap()
                .id
                .clone()
        }
    }

    fn texts(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.Text")
            .iter()
            .map(|t| text_prop(t, "value"))
            .collect()
    }

    #[test]
    fn a_typed_search_and_a_query_in_code_are_one_query() {
        let t = Tasks::new();
        let message = t.ty("Message");
        let clock = Clock::at(NOW).unwrap();
        let typed = find("from:*@yc.com is:unread received:-7d").unwrap();
        // What `query(Message, from: "*@yc.com", unread: true, received: -7d)` writes.
        let coded = Query {
            terms: vec![
                Term::new("from", "mailto:*@yc.com"),
                Term::new("unread", "true"),
                Term::new("received", "-7d"),
            ],
            ..Query::default()
        };
        let enums = &t.app.meta().enums;
        let a = resolve(&typed, message, enums, &clock).unwrap();
        let b = resolve(&coded, message, enums, &clock).unwrap();
        assert_eq!(a, b);
        let field = |name: &str| Operand::Field(t.field("Message", name));
        assert_eq!(
            a.pred,
            Pred::And(vec![
                Pred::Like(t.field("Message", "from"), "mailto:*@yc.com".into()),
                Pred::Cmp(CmpOp::Eq, field("unread"), Operand::Lit(Lit::Bool(true))),
                Pred::And(vec![
                    Pred::Cmp(
                        CmpOp::Ge,
                        field("received"),
                        Operand::Lit(Lit::Time("2026-10-20T10:00:00Z".into()))
                    ),
                    Pred::Cmp(
                        CmpOp::Le,
                        field("received"),
                        Operand::Lit(Lit::Time(NOW.into()))
                    ),
                ]),
            ])
        );
        // And they find the same messages.
        assert_eq!(t.shows("coded", None), ["fresh from yc"]);
        assert_eq!(
            t.shows("typed_mail", Some("from:*@yc.com is:unread received:-7d")),
            ["fresh from yc"]
        );
    }

    #[test]
    fn a_query_lists_what_its_filters_hold_in_its_order_and_follows_changes() {
        let t = Tasks::new();
        let mut view = t.view("open_by_effort", None);
        assert_eq!(texts(view.tree()), ["b", "c", "a"]);
        let a = match t.rt.read(&t.board, "tasks").unwrap() {
            Value::List(items) => match items.first() {
                Some(Value::Ref(r)) => (*r.target).clone(),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        };
        t.call("check", vec![Value::resource(&a)]);
        view.poll_patches().unwrap();
        assert_eq!(texts(view.tree()), ["b", "c"], "a done task leaves");
    }

    #[test]
    fn each_kind_of_filter_reads_its_values() {
        let t = Tasks::new();
        let cases: &[(&str, &[&str])] = &[
            ("", &["a", "b", "c", "d"]),
            ("is:open", &["a", "b", "c"]),
            ("is:done", &["d"]),
            ("done:false", &["a", "b", "c"]),
            ("due:today", &["a"]),
            ("due:2026-10", &["a", "b"]),
            ("due:2026-10-21..", &["a", "d"]),
            ("due:..today", &["a", "b"]),
            ("due:-7d", &["a", "b"]),
            ("due:+7d", &["a", "d"]),
            ("due:none", &["c"]),
            ("due:any", &["a", "b", "d"]),
            ("status:waiting", &["a", "d"]),
            ("status:waiting,blocked", &["a", "b", "d"]),
            ("effort:>3", &["b", "c"]),
            ("effort:>=3", &["a", "b", "c"]),
            ("effort:3..5", &["a", "c"]),
            ("effort:<3", &["d"]),
            ("contact:*@yc.com", &["a", "d"]),
            ("contact:jane@yc.com", &["a"]),
            ("contact:tel:+49*", &["b"]),
            ("*@yc.com", &["a", "d"]),
            ("tel:+49*", &["b"]),
            ("tags:home", &["a"]),
            ("tags:none", &["b", "d"]),
            ("is:open effort:>3 sort:-effort", &["b", "c"]),
            ("sort:effort", &["d", "a", "c", "b"]),
            ("sort:-due", &["d", "a", "b", "c"]),
        ];
        for (q, want) in cases {
            assert_eq!(t.shows("typed", Some(q)), *want, "`{q}`");
        }
        // Over a list of resources the same query reads each one's fields.
        assert_eq!(t.shows("sorted", Some("is:open effort:>3")), ["b", "c"]);
        assert_eq!(
            t.shows("sorted", Some("sort:-effort")),
            ["b", "c", "a", "d"]
        );
    }

    #[test]
    fn a_token_that_does_not_fit_the_types_filters_says_why() {
        let t = Tasks::new();
        let task = t.ty("Task");
        let clock = Clock::at(NOW).unwrap();
        let enums = &t.app.meta().enums;
        let why = |q: &str| {
            resolve(&find(q).unwrap(), task, enums, &clock)
                .err()
                .unwrap_or_else(|| panic!("`{q}` resolves"))
        };
        assert!(why("nope>1").contains("text to search for"));
        assert!(why("status:done").contains("Status has no case `done`"));
        assert!(why("done:maybe").contains("a toggle is `true` or `false`"));
        assert!(why("effort:lots").contains("is not a whole number"));
        assert!(why("due:soon").contains("is not a day"));
        assert!(why("contact:janedoe").contains("is not an identifier"));
        assert!(why("sort:-tags").contains("which has no order"));
        assert!(why("owner.org:YC").contains("goes through a reference"));
        assert!(why("milk").contains("text to search for"));
        // A key that is no filter, of a type that holds no identifier, says so; an identifier
        // alone in such a type matches nothing.
        let board = t.ty("Board");
        assert!(
            resolve(&find("nope:1").unwrap(), board, enums, &clock)
                .err()
                .is_some_and(|why| why.contains("`nope` is no filter; Board has no filters")),
        );
        assert_eq!(
            resolve(&find("*@yc.com").unwrap(), board, enums, &clock)
                .unwrap()
                .pred,
            Pred::Const(false)
        );
    }
}
