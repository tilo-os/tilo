//! Filters (search.md, "What a type declares"): a type's filters are values the compiler knows
//! (`Task.today`), what `filters(T)` lists and what a query takes; a list's places are the ones
//! that are on or off, each holding the items its predicate says. A filter is its predicate and
//! nothing else — no membership, grouping, order or count of its own. What a window shows beside
//! them (a place's badge, the projects its root holds) is the app's own.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::members::Member;
    use tilolang_runtime::{
        AmStore, Principal, ResourceRef, Runtime, SimpleAuthority, UiNode, Value,
    };

    const NOW: &str = "2026-10-27T10:00:00Z";

    const SOURCE: &str = r#"app so.tilo.tasks "Tasks"

type Task {
    title: Text
    done: Bool = false
    due: Option<Date> = .none
    project: Option<Project> = .none
    /// Who is doing it.
    assignee: Option<Principal> = .none

    search {
        filter "Inbox" = project == .none and due == .none and not done (icon: .inbox)
        filter "Today" = due <= today and not done (icon: .star)
        filter "Upcoming" = due > today and not done (icon: .calendar)
        filter "Done" = done (icon: .circle_check)
        filter "Assigned to me" = assignee == workspace.me and not done (icon: .user)
        filter project
    }
}

type Project {
    name: Text
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

view sidebar(b: Board) {
    state place: Filter<Task> = Task.inbox
    // The places whose badge says how many wait there, by key.
    let counted: List<Text> = ["inbox", "today", "done", "assigned_to_me"]
    ui.NavList(selected: place) {
        for f in filters(Task).filter(f -> f.toggle) {
            let held = if counted.any(k -> k == f.key) { b.tasks.matching(f).len() } else { 0 }
            ui.NavItem(
                f,
                f.label,
                icon: f.icon,
                count: if held > 0 { .some(held) } else { .none },
            ) {
                on select: set(place, f)
            }
        }
        // A place of a value: the project a task is filed under.
        for p in b.projects {
            ui.NavItem(Task.project(p), p.name, icon: .folder) {
                on select: set(place, Task.project(p))
            }
        }
    }
    for t in b.tasks.matching(place) {
        text(t.title)
    }
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

        /// Finishes the task titled `title` at `when`.
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

        fn titles(&self, q: &str) -> Vec<String> {
            self.titles_by(&me(), q)
        }

        /// The titles `q` holds, as `who` sees them.
        fn titles_by(&self, who: &Principal, q: &str) -> Vec<String> {
            let view = self
                .rt
                .open_view(
                    who,
                    &self.app.view("titles"),
                    vec![Value::resource(&self.board), Value::text(q)],
                )
                .unwrap();
            titles_of(view.tree())
        }

        fn places_of(&self) -> Vec<(String, Option<i64>)> {
            self.places_of_by(&me())
        }

        /// The project named `name`.
        fn project(&self, name: &str) -> ResourceRef {
            let Value::List(items) = self.rt.read(&self.board, "projects").unwrap() else {
                panic!("the board holds its projects in a collection");
            };
            items
                .into_iter()
                .find_map(|item| match item {
                    Value::Ref(r)
                        if self.rt.read(&r.target, "name").unwrap().as_text() == Some(name) =>
                    {
                        Some(ResourceRef::clone(&r.target))
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no project named {name}"))
        }

        /// The sidebar opened with `who` looking.
        fn sidebar_by(&self, who: &Principal) -> tilolang_runtime::ViewInstance {
            self.rt
                .open_view(
                    who,
                    &self.app.view("sidebar"),
                    vec![Value::resource(&self.board)],
                )
                .unwrap()
        }

        /// The places the sidebar offers `who`.
        fn places_of_by(&self, who: &Principal) -> Vec<(String, Option<i64>)> {
            let view = self.sidebar_by(who);
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
    }

    fn nav_items(node: &UiNode) -> Vec<&UiNode> {
        of_kind(node, "ui.NavItem")
    }

    /// The titles a view shows as text, in order.
    fn titles_of(node: &UiNode) -> Vec<String> {
        of_kind(node, "ui.Text")
            .iter()
            .map(|t| text_prop(t, "value"))
            .collect()
    }

    #[test]
    fn the_places_are_the_types_own_filters_and_nothing_the_root_holds() {
        let t = Tasks::new();
        let places = t.places_of();
        let keys: Vec<&str> = places.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(
            &keys[..5],
            [
                "is:inbox",
                "is:today",
                "is:upcoming",
                "is:done",
                "is:assigned_to_me",
            ],
            "the type's filters that are on or off, in the order it declares them, and nothing \
             else: a filter naming a reference is a value to pick in search, not a place: {places:?}"
        );
        // The projects the root holds are a list the app draws of its own, and the key its item
        // carries is the filter value it hands the list: the query that picks the project's
        // tasks, which no app writes as text.
        let project = format!("project:{}", t.project("Work").id_string());
        assert_eq!(keys[5..], [project], "{places:?}");
    }

    #[test]
    fn a_filter_naming_the_person_looking_reads_differently_for_each_of_them() {
        let t = Tasks::new();
        t.rt.set_members(
            vec![
                Member::person("me", "owner"),
                Member::person("ben", "editor"),
            ],
            None,
        );
        let ben = Principal::new("ben");
        t.assign("later", &me());
        t.assign("today", &ben);
        assert_eq!(
            t.titles("is:assigned_to_me"),
            ["later"],
            "the filter asks which task's assignee is the person looking"
        );
        assert_eq!(
            t.titles_by(&ben, "is:assigned_to_me"),
            ["today"],
            "and reads differently for anyone else looking: {ben:?}"
        );
        let places = t.places_of();
        assert_eq!(
            places[4],
            ("is:assigned_to_me".to_owned(), Some(1)),
            "the place counts what the person looking holds open: {places:?}"
        );
        assert_eq!(
            t.places_of_by(&ben)[4],
            ("is:assigned_to_me".to_owned(), Some(1)),
            "and counts each person's own: {places:?}"
        );
        t.finish("later", "2026-10-30T09:00:00Z");
        assert_eq!(
            t.titles("is:assigned_to_me"),
            Vec::<String>::new(),
            "finished work leaves the place"
        );
        assert_eq!(t.places_of()[4].1, None, "and a count of zero says nothing");
    }

    #[test]
    fn a_place_holds_what_its_filter_says_and_nothing_else() {
        let t = Tasks::new();
        // The inbox: open, filed under nothing, for no day.
        assert_eq!(t.titles("is:inbox"), ["undated"]);
        // What is due: open and for a day up to today, in the order the collection holds them.
        assert_eq!(
            t.titles("is:today"),
            ["overdue", "today"],
            "a place keeps the collection's order; sorting is the app's"
        );
        // What is due later: open and for a day after today.
        assert_eq!(t.titles("is:upcoming"), ["later", "filed"]);
        // What is done, and only what is done.
        assert_eq!(t.titles("is:done"), ["done"]);
    }

    #[test]
    fn only_the_places_the_sidebar_counts_show_a_count() {
        let t = Tasks::new();
        let places = t.places_of();
        let keys: Vec<&str> = places.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(
            &keys[..4],
            ["is:inbox", "is:today", "is:upcoming", "is:done"],
            "the type's filters make the places, in the order it declares them: {places:?}"
        );
        // What waits in the inbox, and what is due: the two a person counts.
        assert_eq!(places[0].1, Some(1), "the inbox's one unsorted task");
        assert_eq!(places[1].1, Some(2), "today's, the overdue one included");
        // A place the sidebar does not count shows none.
        assert_eq!(places[2].1, None, "what is due later counts nothing");
        // A place holds what its filter says, so a counted place says it: the app's own
        // `all_done` reads that zero, not a place's idea of what it holds.
        assert_eq!(places[3].1, Some(1), "what is done is counted as it is");
        assert_eq!(
            places[4],
            ("is:assigned_to_me".to_owned(), None),
            "nothing is assigned to the person looking, so its badge says nothing: {places:?}"
        );
        // The sidebar draws the counts of the places it counts, and no others.
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
        assert_eq!(
            nav("is:done").props.get("count"),
            Some(&Value::Int(1)),
            "a place the sidebar counts shows how many it holds"
        );
    }

    #[test]
    fn a_place_of_a_value_picks_what_it_names_and_the_list_answers_with_it() {
        let t = Tasks::new();
        let project = t.project("Work");
        let key = format!("project:{}", project.id_string());
        let mut view = t.sidebar_by(&me());
        assert_eq!(
            titles_of(view.tree()),
            ["undated"],
            "the inbox it starts in"
        );
        let item = nav_items(view.tree())
            .into_iter()
            .find(|node| node.props.get("key") == Some(&Value::text(&key)))
            .unwrap_or_else(|| panic!("no place {key} in\n{}", dump(view.tree())))
            .key
            .clone();
        view.fire(&item, "select").unwrap();
        view.poll_patches().unwrap();
        // `Task.project(p)` asks the project's tasks, and the view state holds the value: the
        // list's chosen place is its query, which the platform writes from the value.
        assert_eq!(
            titles_of(view.tree()),
            ["filed"],
            "the task filed under the project"
        );
        assert_eq!(
            text_prop(of_kind(view.tree(), "ui.NavList")[0], "selected"),
            key,
            "the window is on the project's place"
        );
        // The same value names the same place: choosing a toggle and coming back reads the same
        // filter, and the project is chosen again by the equality of what they ask.
        let toggle = nav_items(view.tree())
            .into_iter()
            .find(|node| node.props.get("key") == Some(&Value::text("is:done")))
            .expect("the Done place")
            .key
            .clone();
        view.fire(&toggle, "select").unwrap();
        view.poll_patches().unwrap();
        assert_eq!(titles_of(view.tree()), ["done"]);
    }
}
