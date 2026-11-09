//! Choosing more than one row of a list: the window's bar offers what the rows offer, once
//! over all of them, and running one changes them all as one step a single undo takes back.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{AmStore, Principal, ResourceRef, Runtime, UiNode, Value, ViewInstance};

    const TASKS: &str = super::common::TASKS_SOURCE;
    /// What a task's page says about it: tasks are made from discussions of them.
    const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");

    /// An app installed in a workspace of its own: its actions runnable, its views openable.
    struct Installed {
        rt: Runtime,
        app: App,
        me: Principal,
        /// Held for the run: the platform trusts the module for as long as it stands.
        _auth: TestAuthority,
    }

    fn tasks() -> Installed {
        let store = AmStore::new();
        let auth = TestAuthority::new();
        let rt = Runtime::new(store, auth.clone()).unwrap();
        let discussion = App::compile(DISCUSSION, &[]);
        auth.trust(&discussion.load(&rt));
        let app = App::compile(TASKS, &[discussion.meta().clone()]);
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
            // `new_task` takes the fields a task can be started with before its title, and
            // this makes one with nothing but a title.
            let mut args = vec![Value::resource(root)];
            args.resize(6, Value::none());
            args.push(Value::text(title));
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

    fn of_kind<'a>(tree: &'a UiNode, kind: &str) -> Vec<&'a UiNode> {
        fn walk<'a>(node: &'a UiNode, kind: &str, out: &mut Vec<&'a UiNode>) {
            if node.kind == kind {
                out.push(node);
            }
            for child in &node.children {
                walk(child, kind, out);
            }
        }
        let mut out = Vec::new();
        walk(tree, kind, &mut out);
        out
    }

    /// The children of the slot `name` of `node`, in order.
    fn slot<'a>(node: &'a UiNode, name: &str) -> Vec<&'a UiNode> {
        node.children
            .iter()
            .filter(|child| {
                child.kind == "slot" && child.props.get("name") == Some(&Value::text(name))
            })
            .flat_map(|slot| slot.children.iter())
            .collect()
    }

    /// The window's bar: the shell that spans its columns.
    fn bar(tree: &UiNode) -> &UiNode {
        let shells = of_kind(tree, "ui.NavShell");
        assert_eq!(shells.len(), 1, "a window has one bar over its columns");
        shells[0]
    }

    /// The buttons the window's bar offers, by the words on them.
    fn offered(tree: &UiNode) -> Vec<String> {
        slot(bar(tree), "actions")
            .iter()
            .filter(|node| node.kind == "ui.Button")
            .map(|node| text_of(node, "label"))
            .collect()
    }

    /// The button the window's bar offers with the label `label`, as the person presses it.
    fn button(tree: &UiNode, label: &str) -> String {
        slot(bar(tree), "actions")
            .iter()
            .find(|node| node.kind == "ui.Button" && text_of(node, "label") == label)
            .unwrap_or_else(|| {
                panic!(
                    "the bar offers no {label}: {:?}",
                    slot(bar(tree), "actions")
                )
            })
            .key
            .clone()
    }

    /// Pick the place `key` in the window's sidebar, as a person choosing a place does.
    fn pick(window: &mut ViewInstance, key: &str) {
        let item = of_kind(window.tree(), "ui.NavItem")
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
            let lists = of_kind(window.tree(), "ui.VirtualList");
            assert_eq!(lists.len(), 1, "the window shows one list");
            lists[0].key.clone()
        };
        let items: Vec<Value> = items.iter().map(|item| Value::resource(item)).collect();
        window
            .fire_with(&list, "selected", Some(&Value::List(items)))
            .unwrap();
        window.poll_patches().unwrap();
    }

    /// Six tasks chosen at once are one command of the bar and one change: each of the six
    /// stands in the same change, which says it changed six, and the bar says what is chosen.
    #[test]
    fn six_chosen_tasks_are_changed_as_one_step() {
        let w = tasks();
        let root = w.root("Tasks");
        let six: Vec<ResourceRef> = (1..=6)
            .map(|n| w.task(&root, &format!("Task {n}")))
            .collect();
        let mut window = w.window("home", &root);
        // The window opens on today, and these tasks are undated: the person picks the inbox,
        // where what waits to be sorted stands.
        pick(&mut window, "is:inbox");
        let chosen: Vec<&ResourceRef> = six.iter().collect();
        choose(&mut window, &chosen);
        let tree = window.tree();
        let badge = slot(bar(tree), "actions")
            .into_iter()
            .find(|node| node.kind == "ui.Badge")
            .unwrap_or_else(|| panic!("the bar says what is chosen"));
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
            "the window's own subject gives its commands to the choice: {commands:?}"
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
            of_kind(tree, "ui.Badge").len(),
            0,
            "the choice is put away once it has acted"
        );
        assert!(
            offered(tree).contains(&"New task".to_owned()),
            "the window's own commands are back: {:?}",
            offered(tree)
        );
    }

    /// A command one of the chosen tasks cannot take is no command over them: choosing a task
    /// already done leaves "Today" out of the bar, and "Delete", which every one of them takes,
    /// still in it.
    #[test]
    fn an_action_one_chosen_item_cannot_take_is_not_offered() {
        let w = tasks();
        let root = w.root("Tasks");
        let six: Vec<ResourceRef> = (1..=6)
            .map(|n| w.task(&root, &format!("Task {n}")))
            .collect();
        w.call("check", vec![Value::resource(&six[3])]);
        let mut window = w.window("home", &root);
        pick(&mut window, "is:inbox");
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
