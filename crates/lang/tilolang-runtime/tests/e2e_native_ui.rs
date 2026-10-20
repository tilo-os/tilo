//! The native vocabulary's values run: an action's presented state, dates and times, the clock
//! of an invocation, scenarios acting as named people, and rows of resources.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{AmStore, Runtime, SimpleAuthority, Value};

    fn runtime(app: &App) -> Runtime {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        rt
    }

    const PIN: &str = r#"app so.tilo.pins "Pins"

type Note {
    title: Text (title)
    pinned: Bool = false
}

action toggle_pin(n: Note) (label: if n.pinned { "Unpin" } else { "Pin" }, icon: if n.pinned { .bell_off } else { .pin }, on: n.pinned) {
    n.pinned = not n.pinned
}

action rename(n: Note, t: Text) (label: "Rename", on: n.title != "") {
    n.title = t
}
"#;

    #[test]
    fn the_presentation_view_of_an_action_shows_its_subjects_state() {
        let app = App::compile(PIN, &[]);
        let rt = runtime(&app);
        let note = rt.create("Note").unwrap();
        let present = |name: &str| {
            let a = app.meta().actions.iter().find(|a| a.name == name).unwrap();
            let view = a.present.clone().expect("a presentation view");
            let v = rt
                .open_view(&me(), &view, vec![Value::resource(&note)])
                .unwrap();
            let tree = v.tree().clone();
            assert_eq!(tree.kind, "present", "{}", dump(&tree));
            assert!(tree.children.is_empty());
            tree
        };
        let off = present("toggle_pin");
        assert_eq!(text_prop(&off, "label"), "Pin");
        assert_eq!(text_prop(&off, "icon"), "pin");
        assert_eq!(off.props["on"], Value::Int(0));
        rt.invoke_action(
            &me(),
            (
                app.action("toggle_pin").as_str(),
                vec![Value::resource(&note)],
            ),
        )
        .unwrap();
        let on = present("toggle_pin");
        assert_eq!(text_prop(&on, "label"), "Unpin");
        assert_eq!(text_prop(&on, "icon"), "bell-off");
        assert_eq!(on.props["on"], Value::Int(1));
        let rename = present("rename");
        assert_eq!(
            rename.props.keys().collect::<Vec<_>>(),
            ["on"],
            "only what is given dynamically"
        );
        assert!(
            rt.catalogue().iter().all(|s| !s.name.contains('~')),
            "the catalogue never lists it"
        );
    }

    const TASKS: &str = r#"app so.tilo.tasks "Tasks"

type Task {
    title: Text (title)
    due: Option<Date> = .none
    start: Date = Date("2026-10-24")
    done_at: Option<Time> = .none
    seen: Text
}

type Tasks {
    items: Collection<Task>
}

action finish(t: Task) {
    t.done_at = now
    t.seen = "{now}"
    stamp(t)
}

action stamp(t: Task) {
    t.title = "{now}"
}

action plan(t: Task) {
    t.due = .some(Date("2026-11-02"))
}

view task(t: Task) {
    stack {
        text(t.start)
        date(bind: t.due)
        facts {
            "Due": t.due
            "Done": t.done_at
        }
    }
}

view rows(l: Tasks) {
    list(l.items) { t ->
        row(t.title, meta: t.done_at, secondary: t.start)
    }
}
"#;

    #[test]
    fn now_is_the_clocks_time_once_per_invocation() {
        let app = App::compile(TASKS, &[]);
        let rt = runtime(&app);
        let t = rt.create("Task").unwrap();
        let finish = |rt: &Runtime| {
            rt.invoke_action(
                &me(),
                (app.action("finish").as_str(), vec![Value::resource(&t)]),
            )
            .unwrap();
        };
        finish(&rt);
        assert_eq!(
            rt.read(&t, "done_at").unwrap(),
            Value::some(Value::text("1970-01-01T00:00:00Z")),
            "without a clock, the epoch"
        );
        let ticks = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let counter = std::rc::Rc::clone(&ticks);
        rt.set_clock(move || {
            counter.set(counter.get() + 1);
            format!("2026-10-21T09:30:{:02}Z", counter.get())
        });
        finish(&rt);
        assert_eq!(ticks.get(), 1, "one invocation reads the clock once");
        let at = Value::text("2026-10-21T09:30:01Z");
        assert_eq!(rt.read(&t, "done_at").unwrap(), Value::some(at.clone()));
        assert_eq!(
            rt.read(&t, "seen").unwrap(),
            at,
            "a Date interpolates as its ISO text"
        );
        assert_eq!(
            rt.read(&t, "title").unwrap(),
            at,
            "a nested call sees the same time"
        );
        finish(&rt);
        assert_eq!(
            rt.read(&t, "title").unwrap(),
            Value::text("2026-10-21T09:30:02Z")
        );
        rt.set_clock(|| "yesterday".to_owned());
        assert!(
            rt.invoke_action(
                &me(),
                (app.action("finish").as_str(), vec![Value::resource(&t)])
            )
            .is_err(),
            "a clock that answers no time fails the action, which changes nothing"
        );
    }

    #[test]
    fn dates_reach_the_ui_marked_and_a_date_control_edits_them() {
        let app = App::compile(TASKS, &[]);
        let rt = runtime(&app);
        let t = rt.create("Task").unwrap();
        let mut view = rt
            .open_view(&me(), &app.view("task"), vec![Value::resource(&t)])
            .unwrap();
        let text = of_kind(view.tree(), "text")[0].clone();
        assert_eq!(text_prop(&text, "value"), "2026-10-24");
        assert_eq!(text_prop(&text, "value:as"), "date");
        let facts = of_kind(view.tree(), "fact");
        assert_eq!(text_prop(facts[0], "value"), "", "none is the empty text");
        assert_eq!(text_prop(facts[0], "value:as"), "date");
        assert_eq!(text_prop(facts[1], "value:as"), "time");
        let control = of_kind(view.tree(), "date")[0].key.clone();
        assert_eq!(view.bound_value(&control), Some(Value::none()));
        view.edit(&control, &Value::text("2026-12-24")).unwrap();
        assert_eq!(
            rt.read(&t, "due").unwrap(),
            Value::some(Value::text("2026-12-24"))
        );
        assert!(view.edit(&control, &Value::text("Christmas")).is_err());
        view.edit(&control, &Value::text("")).unwrap();
        assert_eq!(rt.read(&t, "due").unwrap(), Value::none());
        rt.invoke_action(
            &me(),
            (app.action("plan").as_str(), vec![Value::resource(&t)]),
        )
        .unwrap();
        view.poll_patches().unwrap();
        let facts = of_kind(view.tree(), "fact");
        assert_eq!(text_prop(facts[0], "value"), "2026-11-02");
    }

    const BOARD: &str = r#"app so.tilo.board "Board"

type Task {
    title: Text (title)
    done_at: Option<Time> = .none
}

type Tasks {
    name: Text (title)
    items: Collection<Task>
}

row view tasks_row for Tasks

view tasks_row(l: Tasks) {
    let latest = match l.items.last() {
        .some(t) => t.title,
        .none => "Nothing yet",
    }
    let oldest = match l.items.first() {
        .some(t) => t.title,
        .none => "",
    }
    row(latest, secondary: oldest)
}

view tasks(l: Tasks) {
    list(l.items) { t ->
        row(t, meta: t.done_at)
    }
}

view summary(l: Tasks) {
    card(l, count: l.items.len()) {
        text(l.name)
    }
}

action add(l: Tasks, title: Text) {
    l.items.insert(Task(title: title))
}
"#;

    #[test]
    fn rows_show_derived_values_and_resources() {
        let app = App::compile(BOARD, &[]);
        let rt = runtime(&app);
        let l = rt.create("Tasks").unwrap();
        let open = |view: &str| {
            rt.open_view(&me(), &app.view(view), vec![Value::resource(&l)])
                .unwrap()
                .tree()
                .clone()
        };
        let row = open("tasks_row");
        assert_eq!(row.kind, "list_row", "{}", dump(&row));
        assert_eq!(text_prop(&row, "title"), "Nothing yet");
        assert_eq!(text_prop(&row, "secondary"), "");
        for title in ["Plan", "Build", "Ship"] {
            rt.invoke_action(
                &me(),
                (
                    app.action("add").as_str(),
                    vec![Value::resource(&l), Value::text(title)],
                ),
            )
            .unwrap();
        }
        let row = open("tasks_row");
        assert_eq!(text_prop(&row, "title"), "Ship", "last() is the newest");
        assert_eq!(text_prop(&row, "secondary"), "Plan", "first() the oldest");
        let list = open("tasks");
        let rows = of_kind(&list, "list_row");
        assert_eq!(rows.len(), 3, "{}", dump(&list));
        let Value::List(items) = rt.read(&l, "items").unwrap() else {
            panic!("a collection reads as a list");
        };
        for (row, item) in rows.iter().zip(&items) {
            let (Some(Value::Ref(shown)), Value::Ref(item)) = (row.props.get("resource"), item)
            else {
                panic!("a resource row holds its resource: {}", dump(row));
            };
            assert_eq!(shown.target, item.target);
            assert!(
                !row.props.contains_key("title"),
                "a resource row has no title"
            );
            assert_eq!(text_prop(row, "meta"), "");
            assert_eq!(text_prop(row, "meta:as"), "time");
        }
        let summary = open("summary");
        let card = of_kind(&summary, "card")[0];
        assert_eq!(card.props["count"], Value::Int(3), "{}", dump(&summary));
        assert!(matches!(&card.props["item"], Value::Ref(r) if r.target == l));
    }
}
