//! A field declared `personal`: each person has a value of their own in a record everyone
//! shares. Reading it gives the person looking's, a filter and an order ask it for them, an
//! action writes the one who runs it, and the values of everyone else stay readable.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{AmStore, Principal, ResourceRef, Runtime, SimpleAuthority, Value};

    const SOURCE: &str = r#"app so.tilo.shared "Shared"

/// Something to do.
type Task {
    title: Text (title)
    done: Bool = false
    due: Option<Date> = .none
    personal today: Bool = false
    personal order: Option<Float> = .none

    search {
        filter "Today" = today and not done
        filter "Open" = not done
    }
}

type Board {
    tasks: Collection<Task>
}

action seed(b: Board) {
    b.tasks.insert(Task(title: "rent", due: Date("2026-10-28")))
    b.tasks.insert(Task(title: "call landlord"))
    b.tasks.insert(Task(title: "groceries"))
}

action pick(t: Task, on: Bool) {
    t.today = on
}

action place(t: Task, at: Float) {
    t.order = at
}

view today(b: Board) {
    for t in b.tasks.matching(Task.today) {
        text(t.title)
    }
}

view picked(b: Board) {
    for t in b.tasks {
        if t.today {
            text(t.title)
        }
    }
}

view picked_by(b: Board) {
    for t in b.tasks {
        for p in others(t.today) {
            text("{t.title} {p.name()}")
        }
    }
}

view ordered(b: Board) {
    for t in query(Task, in: b, sort: .order_oldest) {
        text(t.title)
    }
}
"#;

    fn ada() -> Principal {
        Principal::new("ada")
    }

    fn bo() -> Principal {
        Principal::new("bo")
    }

    struct Shared {
        rt: Runtime,
        app: App,
        board: ResourceRef,
    }

    impl Shared {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            app.load(&rt);
            let board = rt.create("Board").unwrap();
            let s = Self { rt, app, board };
            s.call(&ada(), "seed", vec![Value::resource(&s.board)]);
            s
        }

        fn call(&self, who: &Principal, action: &str, args: Vec<Value>) {
            let id = self.app.action(action);
            self.rt.invoke_action(who, (id.as_str(), args)).unwrap();
        }

        fn task(&self, title: &str) -> ResourceRef {
            let Value::List(items) = self.rt.read(&self.board, "tasks").unwrap() else {
                panic!("the board holds its tasks in a collection");
            };
            let found = items.into_iter().find(|item| {
                let Value::Ref(r) = item else { return false };
                self.rt.read(&r.target, "title").unwrap().as_text() == Some(title)
            });
            found.map_or_else(
                || panic!("no task titled {title}"),
                |item| match item {
                    Value::Ref(r) => ResourceRef::clone(&r.target),
                    _ => panic!("a task is a reference"),
                },
            )
        }

        fn pick(&self, who: &Principal, title: &str, on: bool) {
            let task = self.task(title);
            self.call(who, "pick", vec![Value::resource(&task), Value::Bool(on)]);
        }

        fn place(&self, who: &Principal, title: &str, at: f64) {
            let task = self.task(title);
            self.call(who, "place", vec![Value::resource(&task), Value::Float(at)]);
        }

        fn shows(&self, who: &Principal, view: &str) -> Vec<String> {
            let v = self
                .rt
                .open_view(
                    who,
                    &self.app.view(view),
                    vec![Value::resource(&self.board)],
                )
                .unwrap();
            of_kind(v.tree(), "ui.Text")
                .iter()
                .map(|t| text_prop(t, "value"))
                .collect()
        }
    }

    #[test]
    fn each_person_has_a_value_of_their_own_and_reads_it() {
        let s = Shared::new();
        assert_eq!(s.shows(&ada(), "picked"), Vec::<String>::new());
        s.pick(&ada(), "rent", true);
        // Ada's pick is hers: her Today holds the rent, and Bo's holds nothing.
        assert_eq!(s.shows(&ada(), "today"), ["rent"]);
        assert_eq!(s.shows(&ada(), "picked"), ["rent"]);
        assert_eq!(s.shows(&bo(), "today"), Vec::<String>::new());
        // Bo picks one of his own, and neither pick moves the other person's.
        s.pick(&bo(), "groceries", true);
        assert_eq!(s.shows(&bo(), "today"), ["groceries"]);
        assert_eq!(s.shows(&ada(), "today"), ["rent"]);
        // Setting it off takes it out again, for that person alone.
        s.pick(&ada(), "rent", false);
        assert_eq!(s.shows(&ada(), "today"), Vec::<String>::new());
        assert_eq!(s.shows(&bo(), "today"), ["groceries"]);
    }

    #[test]
    fn everyone_reads_who_else_picked_a_shared_task() {
        let s = Shared::new();
        s.pick(&ada(), "rent", true);
        s.pick(&bo(), "groceries", true);
        // As Bo: what Ada picked, and his own; as Ada, the other way round.
        assert_eq!(s.shows(&bo(), "picked_by"), ["rent ada"]);
        assert_eq!(s.shows(&ada(), "picked_by"), ["groceries bo"]);
    }

    #[test]
    fn an_order_on_a_personal_field_is_each_persons_own() {
        let s = Shared::new();
        for (title, at) in [("rent", 1.0), ("call landlord", 2.0), ("groceries", 3.0)] {
            s.place(&ada(), title, at);
        }
        for (title, at) in [("rent", 3.0), ("call landlord", 2.0), ("groceries", 1.0)] {
            s.place(&bo(), title, at);
        }
        assert_eq!(
            s.shows(&ada(), "ordered"),
            ["rent", "call landlord", "groceries"]
        );
        assert_eq!(
            s.shows(&bo(), "ordered"),
            ["groceries", "call landlord", "rent"]
        );
    }
}
