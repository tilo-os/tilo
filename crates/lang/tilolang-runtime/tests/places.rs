//! Places (search.md, "What a type declares"): a filter that declares a role makes a place the
//! platform reads — what it holds, the day each of its items stands at, the order they come in,
//! and whether it counts them — rather than the app writing the predicate, the grouping lambdas
//! and the counts itself.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::members::Member;
    use tilolang_runtime::query::{Clock, find, resolve};
    use tilolang_runtime::{
        AmStore, Principal, ResourceRef, Runtime, SimpleAuthority, UiNode, Value,
    };

    const NOW: &str = "2026-10-27T10:00:00Z";

    const SOURCE: &str = r#"app so.tilo.tasks "Tasks"

type Task {
    title: Text (title)
    done: Bool (done) = false
    due: Option<Date> = .none
    project: Option<Project> = .none
    /// Who is doing it.
    assignee: Option<Principal> = .none

    search {
        filter "Inbox" = inbox(filed: project, day: due) (icon: .inbox)
        filter "Today" = agenda(day: due) (icon: .star)
        filter "Upcoming" = upcoming(day: due) (icon: .calendar)
        filter "Done" = logbook (icon: .circle_check)
        filter project
    }
}

type Project {
    name: Text (title)
}

type Board {
    tasks: Collection<Task>
    projects: Collection<Project>
}

action seed(b: Board) {
    b.tasks.insert(Task(title: "undated"))
    b.tasks.insert(Task(title: "overdue", due: Date("2026-10-20")))
    b.tasks.insert(Task(title: "today", due: Date("2026-10-27")))
    b.tasks.insert(Task(title: "later", due: Date("2026-10-30")))
    let p = b.projects.insert(Project(name: "Work"))
    b.tasks.insert(Task(title: "filed", project: .some(p), due: Date("2026-10-28")))
    b.tasks.insert(Task(title: "done", done: true, due: Date("2026-10-21")))
}

action finish(t: Task) {
    t.done = true
}

action assign(t: Task, who: Principal) {
    t.assignee = .some(who)
}

view titles(b: Board, q: Text) {
    for t in b.tasks.matching(q) {
        text(t.title)
    }
}

view days(b: Board, place: Text) {
    for t in b.tasks.matching(place) {
        let day: Option<Date> = placed_day(t, place)
        if let d = day {
            text("{t.title} {d}")
        } else {
            text("{t.title} none")
        }
    }
}


view sidebar(b: Board) {
    window place: Text = "is:inbox"
    std.sidebar(places(Task, in: b), place)
}
"#;

    struct Tasks {
        rt: Runtime,
        app: App,
        board: ResourceRef,
        now: std::rc::Rc<std::cell::RefCell<String>>,
    }

    impl Tasks {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            let now = std::rc::Rc::new(std::cell::RefCell::new(NOW.to_owned()));
            let clock = std::rc::Rc::clone(&now);
            rt.set_clock(move || clock.borrow().clone());
            app.load(&rt);
            let board = rt.create("Board").unwrap();
            let t = Self {
                rt,
                app,
                board,
                now,
            };
            t.run("seed", vec![Value::resource(&t.board)]);
            t
        }

        /// Finishes the task titled `title` at `when`: the change that finishes it is made then,
        /// which is the day the logbook reads it at.
        fn finish(&self, title: &str, when: &str) {
            *self.now.borrow_mut() = when.to_owned();
            let task = self.item(title);
            self.run("finish", vec![Value::resource(&task)]);
        }

        /// Gives the task titled `title` to `who`.
        fn assign(&self, title: &str, who: &Principal) {
            let task = self.item(title);
            self.run(
                "assign",
                vec![Value::resource(&task), Value::Principal(who.clone())],
            );
        }

        fn item(&self, title: &str) -> ResourceRef {
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

        fn run(&self, action: &str, args: Vec<Value>) {
            let id = self.app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap();
        }

        fn view(&self, name: &str, extra: Vec<Value>) -> tilolang_runtime::ViewInstance {
            let mut args = vec![Value::resource(&self.board)];
            args.extend(extra);
            self.rt
                .open_view(&me(), &self.app.view(name), args)
                .unwrap()
        }

        fn titles(&self, q: &str) -> Vec<String> {
            let view = self.view("titles", vec![Value::text(q)]);
            of_kind(view.tree(), "ui.Text")
                .iter()
                .map(|t| text_prop(t, "value"))
                .collect()
        }

        fn places_of(&self) -> Vec<(String, Option<i64>)> {
            let view = self.view("sidebar", vec![]);
            nav_items(view.tree())
                .into_iter()
                .map(|item| {
                    let count = match item.props.get("count") {
                        Some(Value::Int(n)) => Some(*n),
                        _ => None,
                    };
                    (text_prop(item, "key"), count)
                })
                .collect()
        }

        /// Each item of the place, and the day the place's role reads it at: the day it is due
        /// on, the day it was finished on, or none.
        fn days(&self, place: &str) -> Vec<String> {
            let view = self.view("days", vec![Value::text(place)]);
            of_kind(view.tree(), "ui.Text")
                .iter()
                .map(|t| text_prop(t, "value"))
                .collect()
        }

        fn field(&self, ty: &str, name: &str) -> String {
            self.app
                .meta()
                .types
                .iter()
                .find(|t| t.name == ty)
                .unwrap()
                .fields
                .iter()
                .find(|f| f.name == name)
                .unwrap()
                .id
                .clone()
        }

        fn order_of(&self, q: &str) -> Option<tilolang_runtime::query::Order> {
            let ty = self
                .app
                .meta()
                .types
                .iter()
                .find(|t| t.name == "Task")
                .unwrap();
            let clock = Clock::at(NOW).unwrap();
            let parsed = find(q).unwrap();
            resolve(&parsed, ty, &self.app.meta().enums, &clock, None)
                .unwrap()
                .order
        }
    }

    fn nav_items(node: &UiNode) -> Vec<&UiNode> {
        of_kind(node, "ui.NavItem")
    }

    #[test]
    fn the_person_an_item_belongs_to_has_a_place_of_their_own() {
        let t = Tasks::new();
        let viewer = me();
        let ben = Principal::new("ben");
        // On their own, nothing is anyone's but theirs: no place of their own to offer.
        assert!(
            !t.places_of().iter().any(|(key, _)| key == "is:mine"),
            "a workspace of one has no place for what is theirs"
        );
        t.rt.set_members(
            vec![
                Member::person("me", "owner"),
                Member::person("ben", "editor"),
            ],
            None,
        );
        let places = t.places_of();
        assert_eq!(
            places[0],
            ("is:mine".to_owned(), Some(0)),
            "in a shared workspace the place leads, and counts what waits in it: {places:?}"
        );
        t.assign("later", &viewer);
        t.assign("today", &ben);
        assert_eq!(
            t.titles("is:mine"),
            ["later"],
            "what the viewer holds, open"
        );
        assert_eq!(
            t.places_of()[0],
            ("is:mine".to_owned(), Some(1)),
            "the place counts what the viewer has left"
        );
        assert_eq!(
            t.days("is:mine"),
            ["later 2026-10-30"],
            "an agenda's place stands each item at the day it is due"
        );
        t.finish("later", "2026-10-30T09:00:00Z");
        assert_eq!(
            t.titles("is:mine"),
            Vec::<String>::new(),
            "finished work leaves the place"
        );
        assert_eq!(t.places_of()[0].1, Some(0), "and the place counts nothing");
        // A workspace of one again: nothing is anyone's but theirs.
        t.rt.set_members(vec![Member::person("me", "owner")], None);
        let places = t.places_of();
        assert!(
            !places.iter().any(|(key, _)| key == "is:mine"),
            "the place goes when the workspace has no one else in it: {places:?}"
        );
    }

    #[test]
    fn a_place_holds_what_its_role_says_and_nothing_else() {
        let t = Tasks::new();
        // The inbox: open, filed under nothing, for no day.
        assert_eq!(t.titles("is:inbox"), ["undated"]);
        // What is due: open and for a day up to today, the earliest first.
        assert_eq!(t.titles("is:today"), ["overdue", "today"]);
        // What is due later: open and for a day after today.
        assert_eq!(t.titles("is:upcoming"), ["filed", "later"]);
        // What is done, and only what is done.
        assert_eq!(t.titles("is:done"), ["done"]);
    }

    #[test]
    fn a_place_comes_in_the_order_its_role_says() {
        let t = Tasks::new();
        let due = t.field("Task", "due");
        let today = t
            .order_of("is:today")
            .expect("the agenda is ordered by its day");
        assert_eq!(today.field, due);
        assert!(!today.descending, "what is due soonest first");
        let upcoming = t
            .order_of("is:upcoming")
            .expect("what is due later is ordered");
        assert_eq!(upcoming.field, due);
        assert!(!upcoming.descending);
        let done = t.order_of("is:done").expect("the logbook is ordered");
        assert_eq!(
            done.field, "$updated",
            "the last change is when it finished"
        );
        assert!(done.descending, "what was finished last first");
        assert_eq!(
            t.order_of("is:inbox"),
            None,
            "an inbox has no order of its own"
        );
        assert_eq!(
            t.order_of("is:today project:none"),
            None,
            "a filter beside the place keeps the type's order"
        );
    }

    #[test]
    fn a_place_reads_the_day_each_of_its_items_stands_at() {
        let t = Tasks::new();
        assert_eq!(
            t.days("is:today"),
            ["overdue 2026-10-20", "today 2026-10-27"],
            "what is due stands at the day it is due on"
        );
        assert_eq!(
            t.days("is:upcoming"),
            ["filed 2026-10-28", "later 2026-10-30"]
        );
        assert_eq!(t.days("is:done"), ["done 2026-10-27"]);
        // Finished on two other days: newest first, each at the day it was finished on.
        t.finish("today", "2026-10-20T09:00:00Z");
        t.finish("later", "2026-10-26T09:00:00Z");
        assert_eq!(
            t.days("is:done"),
            ["done 2026-10-27", "later 2026-10-26", "today 2026-10-20"],
            "what was finished last stands first, at the day it was finished on"
        );
        assert_eq!(
            t.days("is:inbox"),
            ["undated none"],
            "an inbox has no days to stand under"
        );
    }

    #[test]
    fn only_the_places_whose_role_counts_show_a_count() {
        let t = Tasks::new();
        let places = t.places_of();
        let keys: Vec<&str> = places.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(
            &keys[..4],
            ["is:inbox", "is:today", "is:upcoming", "is:done"],
            "the type's filters make the first places, in the order it declares them: {places:?}"
        );
        assert!(
            keys.get(4).is_some_and(|key| key.starts_with("project:")),
            "then a place per project: {places:?}"
        );
        // What waits in the inbox, and what is due: the two a person counts.
        assert_eq!(places[0].1, Some(1), "the inbox's one unsorted task");
        assert_eq!(places[1].1, Some(2), "today's, the overdue one included");
        // The two that count nothing show no count.
        assert_eq!(places[2].1, None, "what is due later counts nothing");
        assert_eq!(places[3].1, None, "what is done counts nothing");
        assert_eq!(places[4].1, None, "a project's tasks count nothing");
        assert!(
            places[4].0.starts_with("project:"),
            "a project's place is keyed by the project: {places:?}"
        );
        // The sidebar draws those counts and no others.
        let view =
            t.rt.open_view(
                &me(),
                &t.app.view("sidebar"),
                vec![Value::resource(&t.board)],
            )
            .unwrap();
        let nav = |key: &str| -> &UiNode {
            of_kind(view.tree(), "ui.NavItem")
                .into_iter()
                .find(|node| node.props.get("key") == Some(&Value::text(key)))
                .unwrap_or_else(|| panic!("no place {key} in\n{}", dump(view.tree())))
        };
        assert_eq!(
            nav("is:inbox").props.get("count"),
            Some(&Value::Int(1)),
            "the inbox shows what waits to be sorted"
        );
        assert_eq!(
            nav("is:today").props.get("count"),
            Some(&Value::Int(2)),
            "today shows what is due, the overdue one included"
        );
        assert_eq!(nav("is:upcoming").props.get("count"), None);
        assert_eq!(nav("is:done").props.get("count"), None);
    }
}
