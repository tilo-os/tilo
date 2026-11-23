//! The native vocabulary's values run: an action's presented state, dates and times, the clock
//! of an invocation, scenarios acting as named people, and rows of resources.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, Runtime, SimpleAuthority, Value};

    fn runtime(app: &App) -> Runtime {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        rt
    }

    const PIN: &str = r#"app so.tilo.pins "Pins"

type Note {
    title: Text
    pinned: Bool = false
}

action pin(n: Note, pinned: Bool = not n.pinned) (label: if pinned { "Pin" } else { "Unpin" }, icon: if pinned { .pin } else { .bell_off }, on: n.pinned) {
    n.pinned = pinned
}

action rename(n: Note, t: Text) (label: "Rename", on: n.title != "") {
    n.title = t
}
"#;

    tilo_test_env::scenario! {
        fn a_native_view_presents_an_action_and_reads_the_clock_once;

        phase the_presentation_view_of_an_action_shows_its_targets_state;
        {
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
            let off = present("pin");
            assert_eq!(text_prop(&off, "label"), "Pin");
            assert_eq!(text_prop(&off, "icon"), "pin");
            assert_eq!(off.props["on"], Value::Int(0));
            rt.invoke_action(
                &me(),
                (app.action("pin").as_str(), vec![Value::resource(&note)]),
            )
            .unwrap();
            let on = present("pin");
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

        phase now_is_the_clocks_time_once_per_invocation;
        {
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
            assert_eq!(
                ticks.get(),
                2,
                "each action reads the clock once for the change it makes: the call is its own"
            );
            let at = Value::text("2026-10-21T09:30:01Z");
            assert_eq!(rt.read(&t, "done_at").unwrap(), Value::some(at.clone()));
            assert_eq!(
                rt.read(&t, "seen").unwrap(),
                at,
                "a moment interpolated reads as its stored text where nothing names its day"
            );
            assert_eq!(
                rt.read(&t, "title").unwrap(),
                Value::text("2026-10-21T09:30:02Z"),
                "a call is a change of its own, and reads the clock for it"
            );
            finish(&rt);
            assert_eq!(
                rt.read(&t, "title").unwrap(),
                Value::text("2026-10-21T09:30:04Z")
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
    }

    const TASKS: &str = r#"app so.tilo.tasks "Tasks"

type Task {
    title: Text
    due: Option<Date> = .none
    at: Option<Time> = .none
    start: Date = Date("2026-10-24")
    done_at: Option<Time> = .none
    seen: Text
}

type Tasks {
    items: Collection<Task>
}

action finish(t: Task) uses time {
    t.done_at = now
    t.seen = "{now}"
    stamp(t)
}

action stamp(t: Task) uses time {
    t.title = "{now}"
}

action plan(t: Task) {
    t.due = .some(Date("2026-11-02"))
}

view task(t: Task) {
    stack {
        text(t.start)
        ui.DateField(bind: t.due, optional: true)
        ui.DateField(bind: t.start)
        facts(t)
    }
}

view rows(l: Tasks) {
    list(l.items) { t ->
        row(t.title, meta: t.done_at, secondary: t.start)
    }
}
"#;

    const LETTER: &str = r#"app so.tilo.letter "Letter"

type Letter {
    to: Text
    sent: Bool = false
}

action address(l: Letter, to: Text) {
    l.to = to
}

action send(l: Letter) {
    require l.to != "" "It goes to nobody yet"
    l.sent = true
}

view letter(l: Letter) {
    ui.Button("Send") {
        on press: send(l)
    }
}
"#;

    /// A view's button bound to an action stands dimmed while the action's `require` fails,
    /// saying why in that requirement's words, and is ready once it holds.
    #[test]
    fn a_button_bound_to_an_action_says_why_its_require_keeps_it_dimmed() {
        let app = App::compile(LETTER, &[]);
        let rt = runtime(&app);
        let l = rt.create("Letter").unwrap();
        let mut view = rt
            .open_view(&me(), &app.view("letter"), vec![Value::resource(&l)])
            .unwrap();
        let send = of_kind(view.tree(), "ui.Button")[0].clone();
        assert_eq!(
            send.props.get("enabled"),
            Some(&Value::Int(0)),
            "{}",
            dump(view.tree())
        );
        assert_eq!(text_prop(&send, "description"), "It goes to nobody yet");
        rt.invoke_action(
            &me(),
            (
                app.action("address").as_str(),
                vec![Value::resource(&l), Value::text("ada@analytical.example")],
            ),
        )
        .unwrap();
        view.poll_patches().unwrap();
        let send = of_kind(view.tree(), "ui.Button")[0].clone();
        assert_eq!(send.props.get("enabled"), None, "{}", dump(view.tree()));
        assert_eq!(send.props.get("description"), None);
    }

    #[test]
    fn dates_reach_the_ui_and_a_date_control_edits_them() {
        let app = App::compile(TASKS, &[]);
        let rt = runtime(&app);
        let t = rt.create("Task").unwrap();
        let mut view = rt
            .open_view(&me(), &app.view("task"), vec![Value::resource(&t)])
            .unwrap();
        let text = of_kind(view.tree(), "ui.Text")[0].clone();
        assert_eq!(text_prop(&text, "value"), "2026-10-24");
        assert_eq!(text_prop(&text, "value:as"), "date");
        let dates = of_kind(view.tree(), "ui.DateField");
        assert_eq!(
            dates[0].props.get("optional"),
            Some(&Value::Int(1)),
            "a date that may be none can be cleared"
        );
        assert!(!dates[1].props.contains_key("optional"));
        // The page's own facts are the chips its type's days make, after the controls the
        // view writes: the day it is due, then the day it starts. A time of day is the app's
        // data, so no field's time stands paired with a day.
        assert_eq!(
            dates.len(),
            4,
            "two controls, two day facts: {}",
            dump(view.tree())
        );
        let chip = dates[2].clone();
        assert_eq!(
            view.bound_value(&chip.key),
            Some(Value::none()),
            "none is the day a page holds none of"
        );
        assert!(
            of_kind(view.tree(), "ui.TimeField").is_empty(),
            "a time of day is the app's to draw: {}",
            dump(view.tree())
        );
        let control = dates[0].key.clone();
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
        let dates = of_kind(view.tree(), "ui.DateField");
        assert_eq!(
            view.bound_value(&dates[2].key),
            Some(Value::some(Value::text("2026-11-02"))),
            "the chip shows the day the action set"
        );
    }

    const BOARD: &str = r#"app so.tilo.board "Board"

type Task {
    title: Text
    done_at: Option<Time> = .none
}

type Tasks {
    name: Text
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
    ui.Card(item: l, title: meta(l).title, count: l.items.len()) {
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
        // The rows are read as a host that draws every row does.
        let open = |view: &str| {
            let mut opened = rt
                .open_view(&me(), &app.view(view), vec![Value::resource(&l)])
                .unwrap();
            opened.set_lazy_rows(false).unwrap();
            opened.tree().clone()
        };
        let row = open("tasks_row");
        assert_eq!(row.kind, "ui.ListRow", "{}", dump(&row));
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
        let rows = of_kind(&list, "ui.ListRow");
        assert_eq!(rows.len(), 3, "{}", dump(&list));
        let Value::List(items) = rt.read(&l, "items").unwrap() else {
            panic!("a collection reads as a list");
        };
        for ((row, item), title) in rows.iter().zip(&items).zip(["Plan", "Build", "Ship"]) {
            let (Some(Value::Ref(shown)), Value::Ref(item)) = (row.props.get("item"), item) else {
                panic!("a resource row holds its resource: {}", dump(row));
            };
            assert_eq!(shown.target, item.target);
            assert_eq!(
                text_prop(row, "title"),
                title,
                "a resource row is titled as its resource is"
            );
            // A time not yet set (`.none`) leaves the row without its meta.
            assert!(!row.props.contains_key("meta"), "{}", dump(row));
        }
        let summary = open("summary");
        let card = of_kind(&summary, "ui.Card")[0];
        assert_eq!(card.props["count"], Value::Int(3), "{}", dump(&summary));
        assert!(matches!(&card.props["item"], Value::Ref(r) if *r.target == l));
    }

    const SUMMARY: &str = r#"app so.tilo.summary "Summary"

type Task {
    title: Text
}

type Tasks {
    items: Collection<Task>
}

default view summary for Tasks

row view task_row for Task

view task_row(t: Task) {
    row(t.title)
}

template view card<R, T>(of: R, items: List<T>, row: (T) -> View) {
    ui.Card(item: of, title: "Tasks", count: items.len()) {
        list(items) { x -> row(x) }
        if items.len() > preview_rows() {
            ui.Button("Show all {items.len()}", role: .plain) {
                on press: open(of)
            }
        }
        on activate: open(of)
    }
}

view summary(l: Tasks) {
    card(l, l.items) { t ->
        task_row(t)
    }
}

action add(l: Tasks, title: Text) {
    l.items.insert(Task(title: title))
}
"#;

    #[test]
    fn a_card_shows_the_first_rows_of_a_list_and_says_how_many_it_leaves_out() {
        let app = App::compile(SUMMARY, &[]);
        let rt = runtime(&app);
        let l = rt.create("Tasks").unwrap();
        for title in [
            "One", "Two", "Three", "Four", "Five", "Six", "Seven", "Eight",
        ] {
            rt.invoke_action(
                &me(),
                (
                    app.action("add").as_str(),
                    vec![Value::resource(&l), Value::text(title)],
                ),
            )
            .unwrap();
        }
        let mut view = rt
            .open_view(&me(), &app.view("summary"), vec![Value::resource(&l)])
            .unwrap();
        view.set_lazy_rows(false).unwrap();
        let tree = view.tree().clone();
        let card = of_kind(&tree, "ui.Card")[0];
        assert_eq!(card.props["count"], Value::Int(8), "{}", dump(&tree));
        let list = of_kind(&tree, "list")[0];
        assert_eq!(list.props["len"], Value::Int(8), "{}", dump(&tree));
        let rows: Vec<String> = of_kind(list, "ui.ListRow")
            .iter()
            .map(|r| text_prop(r, "title"))
            .collect();
        assert_eq!(
            rows,
            ["One", "Two", "Three", "Four", "Five"],
            "{}",
            dump(&tree)
        );
        let show_all = of_kind(&tree, "ui.Button");
        assert_eq!(show_all.len(), 1, "{}", dump(&tree));
        assert_eq!(text_prop(show_all[0], "label"), "Show all 8");
    }

    const JOIN: &str = r#"app so.tilo.join "Join"

type Network {
    name: Text
    asks: List<Text> = []
    answered: List<Text> = []
    keys: List<Text> = []
}

view join_form(n: Network) {
    state typed: List<Text> = []
    stack {
        for i in n.asks.map((a, i) -> i) {
            ui.TextInput(bind: typed.at(i), placeholder: n.asks.at(i) ?? "")
        }
        ui.ShortcutRecorder(bind: typed.at(n.asks.len()), label: "Shortcut")
        ui.Button("Join") {
            on press: join(n, typed)
        }
    }
}

action join(n: Network, answers: List<Text>) (label: "Join") {
    n.answered = answers
}

action ask(n: Network, what: Text) {
    n.asks.insert(what)
}
"#;

    /// A form a view builds from a list: each row's control types into its own place of one
    /// `List<Text>` of view state (`bind: typed.at(i)`), in whatever order the person types,
    /// and the action the form submits takes the whole list.
    #[test]
    fn each_row_of_a_form_types_into_its_own_place_of_a_list() {
        let app = App::compile(JOIN, &[]);
        let rt = runtime(&app);
        let n = rt.create("Network").unwrap();
        for what in ["User name", "Password", "Domain"] {
            rt.invoke_action(
                &me(),
                (
                    app.action("ask").as_str(),
                    vec![Value::resource(&n), Value::text(what)],
                ),
            )
            .unwrap();
        }
        let mut view = rt
            .open_view(&me(), &app.view("join_form"), vec![Value::resource(&n)])
            .unwrap();
        let inputs: Vec<String> = of_kind(view.tree(), "ui.TextInput")
            .iter()
            .map(|i| i.key.clone())
            .collect();
        assert_eq!(inputs.len(), 3, "{}", dump(view.tree()));
        let chord = of_kind(view.tree(), "ui.ShortcutRecorder")[0].key.clone();
        assert_eq!(
            view.bound_value(&inputs[1]),
            Some(Value::text("")),
            "a place nobody typed into holds nothing"
        );
        // Typed out of order: the last field first, then the chord after every field.
        view.edit(&inputs[2], &Value::text("analytical")).unwrap();
        view.edit(&chord, &Value::text("Ctrl+Shift+K")).unwrap();
        view.edit(&inputs[0], &Value::text("ada")).unwrap();
        view.edit(&inputs[1], &Value::text("engine")).unwrap();
        view.poll_patches().unwrap();
        assert_eq!(view.bound_value(&inputs[0]), Some(Value::text("ada")));
        assert_eq!(
            view.bound_value(&inputs[2]),
            Some(Value::text("analytical"))
        );
        assert_eq!(view.bound_value(&chord), Some(Value::text("Ctrl+Shift+K")));
        // Typing again changes only its own place.
        view.edit(&inputs[1], &Value::text("difference engine"))
            .unwrap();
        let button = of_kind(view.tree(), "ui.Button")[0].key.clone();
        view.press(&button).unwrap();
        assert_eq!(
            rt.read(&n, "answered").unwrap(),
            Value::List(
                ["ada", "difference engine", "analytical", "Ctrl+Shift+K"]
                    .map(Value::text)
                    .to_vec()
            )
        );
        // A number is no text: the edit is refused and the place keeps what it held.
        assert!(view.edit(&inputs[0], &Value::Int(7)).is_err());
        assert_eq!(view.bound_value(&inputs[0]), Some(Value::text("ada")));
    }
}
