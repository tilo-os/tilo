//! Choosing more than one row of a list: the window's bar offers what the rows offer, once
//! over all of them, and running one changes them all as one step a single undo takes back.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{Actor, AmStore, ResourceRef, Runtime, UiNode, Value, ViewInstance};

    const TASKS: &str = crate::common::TASKS_SOURCE;

    /// An app installed in a workspace of its own: its actions runnable, its views openable.
    struct Installed {
        rt: Runtime,
        app: App,
        me: Actor,
        /// Held for the run: the platform trusts the module for as long as it stands.
        _auth: TestAuthority,
    }

    fn tasks() -> Installed {
        let store = AmStore::new();
        let auth = TestAuthority::new();
        let rt = Runtime::new(store, auth.clone()).unwrap();
        let imports = Uses::load(&rt, "tasks", |loaded| auth.trust(loaded));
        let app = App::compile(TASKS, &imports.metas());
        auth.trust(&app.load(&rt));
        Installed {
            rt,
            app,
            me: me(),
            _auth: auth,
        }
    }

    impl Installed {
        fn root(&self, ty: &str) -> ResourceRef {
            self.rt.create_root(ty).unwrap()
        }

        fn call(&self, action: &str, args: Vec<Value>) -> tilolang_runtime::InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&self.me, (id.as_str(), args))
                .unwrap()
        }

        fn task(&self, root: &ResourceRef, title: &str) -> ResourceRef {
            // What the task is made of by name, so the fields a task can be started with come
            // and go without this caller: `new_task(tasks: root, title: title)`.
            let args = self.app.args(
                &self.rt,
                "new_task",
                &[
                    ("tasks", Value::resource(root)),
                    ("title", Value::text(title)),
                ],
            );
            returned_ref(self.call("new_task", args))
        }

        fn window(&self, view: &str, resource: &ResourceRef) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(
                    &self.me,
                    &self.app.view(view),
                    vec![Value::resource(resource)],
                )
                .unwrap_or_else(|e| panic!("{view}: {e}"));
            window.set_lazy_rows(false).unwrap();
            window
        }

        /// The versions the resource's own changes made, oldest first.
        fn versions(&self, at: &ResourceRef) -> Vec<tilolang_runtime::store::Version> {
            self.rt.versions(at).unwrap()
        }

        /// The task's last change as `(change, count, action)`.
        fn last(&self, at: &ResourceRef) -> (String, u32, String) {
            let versions = self.versions(at);
            let last = versions.last().expect("a task has a version");
            (last.change.clone(), last.count, last.action.clone())
        }
    }

    /// The text of a node's property.
    fn text_of(node: &UiNode, prop: &str) -> String {
        node.props
            .get(prop)
            .and_then(Value::as_text)
            .unwrap_or_else(|| panic!("{} has no text {prop}: {node:?}", node.kind))
            .to_owned()
    }

    /// The commands of the window's bar: its own and those over its overview, in order.
    fn commands(tree: &UiNode) -> Vec<&UiNode> {
        let mut commands = part(bar(tree), "actions");
        commands.extend(part(bar(tree), "overview_actions"));
        commands
    }

    /// The children of the part `name` of `node`, in order.
    fn part<'a>(node: &'a UiNode, name: &str) -> Vec<&'a UiNode> {
        node.children
            .iter()
            .filter(|child| {
                child.kind == "part" && child.props.get("name") == Some(&Value::text(name))
            })
            .flat_map(|part| part.children.iter())
            .collect()
    }

    /// The window's bar: the shell that spans its columns.
    fn bar(tree: &UiNode) -> &UiNode {
        let shells = tree.of_kind("ui.NavShell");
        assert_eq!(shells.len(), 1, "a window has one bar over its columns");
        shells[0]
    }

    /// The buttons the window's bar offers, by the words on them.
    fn offered(tree: &UiNode) -> Vec<String> {
        commands(tree)
            .iter()
            .filter(|node| node.kind == "ui.Button")
            .map(|node| text_of(node, "label"))
            .collect()
    }

    /// The button the window's bar offers with the label `label`, as the person presses it.
    fn button(tree: &UiNode, label: &str) -> String {
        commands(tree)
            .iter()
            .find(|node| node.kind == "ui.Button" && text_of(node, "label") == label)
            .unwrap_or_else(|| panic!("the bar offers no {label}: {:?}", commands(tree)))
            .key
            .clone()
    }

    /// Pick the place `key` in the window's sidebar, as a person choosing a place does.
    fn pick(window: &mut ViewInstance, key: &str) {
        let item = window
            .tree()
            .of_kind("ui.NavItem")
            .into_iter()
            .find(|node| node.props.get("key") == Some(&Value::text(key)))
            .unwrap_or_else(|| panic!("the window offers no place {key}"))
            .key
            .clone();
        window.fire(&item, "select").unwrap();
        window.poll_patches().unwrap();
    }

    /// Choose `items` in the window's list, as a person choosing rows reports them.
    fn choose(window: &mut ViewInstance, items: &[&ResourceRef]) {
        let list = {
            let lists = window.tree().of_kind("ui.VirtualList");
            assert_eq!(lists.len(), 1, "the window shows one list");
            lists[0].key.clone()
        };
        let items: Vec<Value> = items.iter().map(|item| Value::resource(item)).collect();
        window
            .fire_with(&list, "selected", Some(&Value::List(items)))
            .unwrap();
        window.poll_patches().unwrap();
    }

    /// The list's key, the one list the window shows.
    fn list_of(window: &ViewInstance) -> String {
        let lists = window.tree().of_kind("ui.VirtualList");
        assert_eq!(lists.len(), 1, "the window shows one list");
        lists[0].key.clone()
    }

    /// The rows the window's list shows chosen.
    fn chosen_rows(window: &ViewInstance) -> Value {
        let lists = window.tree().of_kind("ui.VirtualList");
        lists[0]
            .props
            .get("chosen")
            .cloned()
            .unwrap_or_else(|| Value::List(Vec::new()))
    }

    tilo_test_env::scenario! {
        fn multi_select_1;

        phase the_open_tasks_row_stands_chosen_while_no_rows_are;
        // While the person holds no choice of rows, the list shows chosen the row of the task the
        // window has open, so it says what stands beside it; a choice they make is what it shows,
        // and once they put it away the open task's row is chosen again.
        {
            let w = tasks();
            let root = w.root("Tasks");
            let three: Vec<ResourceRef> = (1..=3)
                .map(|n| w.task(&root, &format!("Task {n}")))
                .collect();
            let mut window = w.window("home", &root);
            pick(&mut window, "inbox");
            let list = list_of(&window);
            window
                .fire_with(&list, "opened", Some(&Value::resource(&three[1])))
                .unwrap();
            window.poll_patches().unwrap();
            assert_eq!(
                chosen_rows(&window),
                Value::List(vec![Value::resource(&three[1])]),
                "the open task's row stands chosen"
            );
            choose(&mut window, &[&three[0], &three[2]]);
            assert_eq!(
                chosen_rows(&window),
                Value::List(vec![Value::resource(&three[0]), Value::resource(&three[2])]),
                "the rows the person chose are the ones chosen"
            );
            choose(&mut window, &[]);
            assert_eq!(
                chosen_rows(&window),
                Value::List(vec![Value::resource(&three[1])]),
                "the choice put away, the open task's row stands chosen again"
            );
        }

        phase six_chosen_tasks_are_changed_as_one_step;
        // Six tasks chosen at once are one command of the bar and one change: each of the six
        // stands in the same change, which says it changed six, and the bar says what is chosen.
        {
            let w = tasks();
            let root = w.root("Tasks");
            let six: Vec<ResourceRef> = (1..=6)
                .map(|n| w.task(&root, &format!("Task {n}")))
                .collect();
            let mut window = w.window("home", &root);
            // The window opens on today, and these tasks are undated: the person picks the inbox,
            // where what waits to be sorted stands.
            pick(&mut window, "inbox");
            let chosen: Vec<&ResourceRef> = six.iter().collect();
            choose(&mut window, &chosen);
            let tree = window.tree();
            let badge = commands(tree)
                .into_iter()
                .find(|node| node.kind == "ui.Badge")
                .unwrap_or_else(|| panic!("the bar says what is chosen:\n{}", dump(tree)));
            assert_eq!(text_of(badge, "text"), "6 selected");
            let commands = offered(tree);
            assert!(
                commands.contains(&"Today".to_owned()),
                "what every chosen task can take is offered: {commands:?}"
            );
            assert!(
                commands.contains(&"Delete".to_owned()),
                "a destructive command is offered as itself: {commands:?}"
            );
            assert!(
                !commands.contains(&"New task".to_owned()),
                "the window's own target gives its commands to the choice: {commands:?}"
            );
            // Run it: one command over the six.
            let today = button(tree, "Today");
            window.fire(&today, "press").unwrap();
            window.poll_patches().unwrap();
            let due = w.app.action("due_today");
            let mut changes = Vec::new();
            for task in &six {
                let (change, count, action) = w.last(task);
                assert_eq!(action, due, "each task changed by the one action");
                assert_eq!(count, 6, "the change says it was made over six items");
                changes.push((task.id_string(), change));
            }
            let first = changes[0].1.clone();
            assert!(
                changes.iter().all(|(_, change)| *change == first),
                "the six are one change: {changes:?}"
            );
            // The choice is put away once it has acted: the bar is the window's own again.
            let tree = window.tree();
            assert_eq!(
                tree.of_kind("ui.Badge").len(),
                0,
                "the choice is put away once it has acted"
            );
            assert!(
                offered(tree).contains(&"New task".to_owned()),
                "the window's own commands are back: {:?}",
                offered(tree)
            );
        }

        phase an_action_one_chosen_item_cannot_take_is_not_offered;
        // A command one of the chosen tasks cannot take is no command over them: choosing a task
        // already done leaves "Today" out of the bar, and "Delete", which every one of them takes,
        // still in it.
        {
            let w = tasks();
            let root = w.root("Tasks");
            let six: Vec<ResourceRef> = (1..=6)
                .map(|n| w.task(&root, &format!("Task {n}")))
                .collect();
            w.call("check", vec![Value::resource(&six[3])]);
            let mut window = w.window("home", &root);
            pick(&mut window, "inbox");
            let chosen: Vec<&ResourceRef> = six.iter().collect();
            choose(&mut window, &chosen);
            let offered = offered(window.tree());
            assert!(
                !offered.contains(&"Today".to_owned()),
                "a task done already cannot be made due today: {offered:?}"
            );
            assert!(
                offered.contains(&"Delete".to_owned()),
                "what every chosen task takes is still offered: {offered:?}"
            );
        }
    }
}
