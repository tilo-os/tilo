//! The surfaces the host owns for a window's actions: one bar over the window's columns, the
//! actions a type declares placed there once, each item's actions in the item's own menu, and
//! for the page of an item them behind the "…" beside its title. The bar also holds the switch
//! between the views a type declares, which the window remembers.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{AmStore, Principal, ResourceRef, Runtime, UiNode, Value, ViewInstance};

    const TASKS: &str = include_str!("../../../../apps/tl/tasks/app.tl");
    const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");
    const BOARD: &str = include_str!("../../../../apps/tl/board/app.tl");

    /// An app installed in a workspace of its own: its actions runnable, its views openable.
    struct Installed {
        rt: Runtime,
        app: App,
        me: Principal,
        /// Held for the run: the platform trusts the module for as long as it stands.
        _auth: TestAuthority,
    }

    fn install(source: &str) -> Installed {
        let store = AmStore::new();
        let auth = TestAuthority::new();
        let rt = Runtime::new(store, auth.clone()).unwrap();
        let discussion = App::compile(DISCUSSION, &[]);
        auth.trust(&discussion.load(&rt));
        let app = App::compile(source, &[discussion.meta().clone()]);
        // Installed packages are code the platform trusts, as it does every installed app.
        auth.trust(&app.load(&rt));
        Installed {
            rt,
            app,
            me: me(),
            _auth: auth,
        }
    }

    fn tasks() -> Installed {
        install(TASKS)
    }

    impl Installed {
        /// The app's root resource, as installing it makes one: the window every other
        /// resource of the app stands beside.
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
            returned_ref(self.call(
                "new_task",
                vec![
                    Value::resource(root),
                    Value::none(),
                    Value::none(),
                    Value::none(),
                    Value::none(),
                    Value::none(),
                    Value::text(title),
                ],
            ))
        }

        fn project(&self, root: &ResourceRef, name: &str) -> ResourceRef {
            let project = returned_ref(self.call("new_project", vec![Value::resource(root)]));
            self.call("rename", vec![Value::resource(&project), Value::text(name)]);
            project
        }

        /// The window of `view` on `resource`, every row of its lists rendered: these checks
        /// read the whole tree, as a host that draws every row does.
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
    }

    /// The nodes of a tree of the given kind whose property `prop` is the text `value`.
    fn nodes_with<'a>(tree: &'a UiNode, kind: &str, prop: &str, value: &str) -> Vec<&'a UiNode> {
        of_kind(tree, kind)
            .into_iter()
            .filter(|node| node.props.get(prop) == Some(&Value::text(value)))
            .collect()
    }

    /// The field that edits the title of what the window shows: the one it marks as the title,
    /// whatever words it puts in its placeholder. A page edits one title.
    fn title_field(tree: &UiNode) -> &UiNode {
        let titled: Vec<&UiNode> = of_kind(tree, "ui.TextInput")
            .into_iter()
            .filter(|field| field.props.get("title") == Some(&Value::Int(1)))
            .collect();
        assert_eq!(titled.len(), 1, "the page edits its title:\n{}", dump(tree));
        titled[0]
    }

    /// The text of a node's property.
    fn text_of(node: &UiNode, prop: &str) -> String {
        node.props
            .get(prop)
            .and_then(Value::as_text)
            .unwrap_or_else(|| panic!("{} has no text {prop}: {node:?}", node.kind))
            .to_owned()
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

    /// `node` and the child holding `key` at every step down to it.
    fn path_to<'a>(tree: &'a UiNode, key: &str) -> Option<Vec<&'a UiNode>> {
        if tree.key == key {
            return Some(vec![tree]);
        }
        for child in &tree.children {
            if let Some(mut path) = path_to(child, key) {
                path.insert(0, tree);
                return Some(path);
            }
        }
        None
    }

    /// The window's bar: the shell that spans its columns, which every window has one of.
    fn one_bar(tree: &UiNode) {
        let shells = of_kind(tree, "ui.NavShell");
        assert_eq!(
            shells.len(),
            1,
            "a window has one bar over its columns:\n{}",
            dump(tree)
        );
    }

    /// The Tasks window places the type's primary action in its bar, once, and reads nothing
    /// of the actions on what it shows into chrome of its own: each task's actions are in the
    /// menu the host gives the row that stands for it.
    #[test]
    fn the_tasks_window_has_one_bar_with_its_primary_action_once() {
        let w = tasks();
        let root = w.root("Tasks");
        w.task(&root, "milk");
        let window = w.window("home", &root);
        let tree = window.tree();
        one_bar(tree);
        let new_task = nodes_with(tree, "ui.Button", "label", "New task");
        assert_eq!(
            new_task.len(),
            1,
            "one New task, in the bar:\n{}",
            dump(tree)
        );
        assert_eq!(
            new_task[0].props.get("role"),
            Some(&Value::text("primary")),
            "it is the window's primary"
        );
        // The bar the host filled is the one over the window's columns, not a row of the
        // view's: the shell holds the button, in the slot its overview's actions go in.
        let shell = of_kind(tree, "ui.NavShell")[0];
        assert!(
            slot(shell, "overview_actions")
                .iter()
                .any(|node| node.key == new_task[0].key),
            "the primary stands in the window's bar:\n{}",
            dump(tree)
        );
        // The entry at the list's end declares the primary's key: the key and the bar's button
        // lead the person there to type the item, and the button names it plainly.
        let entry = of_kind(tree, "ui.TextInput")
            .into_iter()
            .find(|field| field.props.get("entry") == Some(&Value::Int(1)))
            .unwrap_or_else(|| panic!("the list ends in its entry:\n{}", dump(tree)));
        assert_eq!(
            entry.props.get("key"),
            Some(&Value::text("Ctrl+n")),
            "the entry where the primary's key leads:\n{}",
            dump(tree)
        );
        // The row the view drew says what a task is; the host put what can be done with it on
        // the row itself.
        let row = &of_kind(tree, "ui.ListItem")[0];
        let menu = slot(row, "menu");
        assert!(
            !menu.is_empty() && menu.iter().all(|item| item.kind == "ui.MenuItem"),
            "the row's menu holds one row per action on it:\n{}",
            dump(tree)
        );
        assert!(
            slot(row, "swipe")
                .iter()
                .all(|node| node.kind == "ui.SwipeAction"),
            "the row swipes its actions too:\n{}",
            dump(tree)
        );
    }

    /// Whether any node of the tree carries the property `name`.
    fn any_prop(node: &UiNode, name: &str) -> bool {
        node.props.contains_key(name) || node.children.iter().any(|child| any_prop(child, name))
    }

    /// A row shows the actions its view places, in the order it placed them, and keeps what it
    /// left over behind its "More": the board's placement is not on every row, and every
    /// command a task takes stays in the row's menu.
    #[test]
    fn a_tasks_row_offers_the_actions_its_view_places_first() {
        let w = tasks();
        let root = w.root("Tasks");
        w.task(&root, "milk");
        let window = w.window("home", &root);
        let tree = window.tree();
        let row = of_kind(tree, "ui.ListItem")[0];
        let menu = slot(row, "menu");
        let labels: Vec<String> = menu.iter().map(|item| text_of(item, "label")).collect();
        assert_eq!(
            labels.get(..4).unwrap_or_default(),
            ["Complete", "Pick date…", "Assign…", "Delete"],
            "the row opens with the actions its view placed, whole menu {labels:?}:\n{}",
            dump(tree)
        );
        assert!(
            menu[4..]
                .iter()
                .all(|item| item.props.get("group") == Some(&Value::text("More"))),
            "what the view left over stands behind its \"More\", whole menu {labels:?}:\n{}",
            dump(tree)
        );
        assert!(
            labels
                .get(4..)
                .is_some_and(|held| held.contains(&"Duplicate".to_owned())),
            "every other action a task takes stays in the row's menu, whole menu \
             {labels:?}:\n{}",
            dump(tree)
        );
        // The swipe a row offers is one it placed: Delete, the destructive action it names.
        let swipe = slot(row, "swipe");
        assert_eq!(
            swipe.first().map(|node| text_of(node, "label")).as_deref(),
            Some("Delete"),
            "the row swipes an action it placed:\n{}",
            dump(tree)
        );
        // The host read the placement while it built the surfaces and took it off the tree: the
        // row is sent with the properties its widget declares and no other.
        assert!(
            !any_prop(tree, "actions"),
            "a row is sent no placement:\n{}",
            dump(tree)
        );
    }

    /// The bar shows the window's actions its view placed, in its order, and keeps the rest of
    /// what the window's own place offers behind a "More" the bar holds itself.
    #[test]
    fn the_tasks_bar_places_the_actions_its_view_names() {
        let w = tasks();
        let root = w.root("Tasks");
        w.task(&root, "milk");
        let window = w.window("home", &root);
        let tree = window.tree();
        let shell = of_kind(tree, "ui.NavShell")[0];
        let bar = slot(shell, "overview_actions");
        let buttons: Vec<String> = bar
            .iter()
            .filter(|node| node.kind == "ui.Button")
            .map(|node| text_of(node, "label"))
            .collect();
        assert_eq!(
            buttons,
            ["New task", "New task due today", "Delete finished tasks"],
            "the bar holds the actions the view placed, in its order:\n{}",
            dump(tree)
        );
        let more = bar
            .iter()
            .find(|node| node.kind == "ui.MenuButton")
            .unwrap_or_else(|| panic!("the bar keeps the rest behind More:\n{}", dump(tree)));
        let behind = more
            .children
            .iter()
            .filter(|child| child.kind == "ui.MenuItem")
            .any(|child| text_of(child, "label") == "New project");
        assert!(
            behind,
            "the rest of what Tasks offers stands behind More:\n{}",
            dump(tree)
        );
        assert!(
            !any_prop(tree, "actions"),
            "a shell is sent no placement:\n{}",
            dump(tree)
        );
    }

    /// An action that destroys what it names is drawn as the destructive button the action
    /// declares, wherever the bar places it: its role is the one its declaration gives it, not
    /// one the bar picks.
    #[test]
    fn a_destructive_action_in_the_bar_is_a_destructive_button() {
        let w = tasks();
        let root = w.root("Tasks");
        w.task(&root, "milk");
        let window = w.window("home", &root);
        let tree = window.tree();
        let delete_finished = nodes_with(tree, "ui.Button", "label", "Delete finished tasks");
        assert_eq!(
            delete_finished.len(),
            1,
            "the bar holds the action once:\n{}",
            dump(tree)
        );
        assert_eq!(
            delete_finished[0].props.get("role"),
            Some(&Value::text("destructive")),
            "a destructive action's button carries its role:\n{}",
            dump(tree)
        );
        // The primary action keeps the primary role, and nothing else claims to be destructive.
        let new_task = nodes_with(tree, "ui.Button", "label", "New task");
        assert_eq!(
            new_task[0].props.get("role"),
            Some(&Value::text("primary")),
            "the primary is the primary button:\n{}",
            dump(tree)
        );
        let destructive = of_kind(tree, "ui.Button")
            .into_iter()
            .filter(|node| node.props.get("role") == Some(&Value::text("destructive")))
            .map(|node| text_of(node, "label"))
            .collect::<Vec<_>>();
        assert_eq!(
            destructive,
            ["Delete finished tasks"],
            "only the actions declared destructive are drawn so:\n{}",
            dump(tree)
        );
    }

    /// A command's "…" is the host's, and says what it means: it opens a form, because running
    /// it still asks for an argument. Finished tasks' delete is answered by the window it stands
    /// in — its only parameter is the resource the window shows — so it reads plainly, while the
    /// actions whose forms ask for a day, a project or a person keep their "…". No app writes
    /// the dots.
    #[test]
    fn the_dots_follow_the_arguments_the_offer_leaves_open() {
        let w = tasks();
        let root = w.root("Tasks");
        w.task(&root, "milk");
        let window = w.window("home", &root);
        let tree = window.tree();
        assert_eq!(
            nodes_with(tree, "ui.Button", "label", "Delete finished tasks").len(),
            1,
            "the window answers what finished tasks' delete asks for:\n{}",
            dump(tree)
        );
        let row = of_kind(tree, "ui.ListItem")[0];
        let labels: Vec<String> = slot(row, "menu")
            .iter()
            .map(|item| text_of(item, "label"))
            .collect();
        for asks in ["Pick date…", "Project…", "Assign…"] {
            assert!(
                labels.iter().any(|label| label == asks),
                "a form opens for {asks}, so it says so: {labels:?}\n{}",
                dump(tree)
            );
        }
        assert!(
            labels.iter().any(|label| label == "Complete"),
            "an action whose argument has a default runs at once: {labels:?}\n{}",
            dump(tree)
        );
    }

    /// A task's page edits its title, and what can be done with the task stands behind the "…"
    /// beside that title: the page has no bar of its own, and no button of the task's actions.
    #[test]
    fn a_tasks_page_keeps_its_actions_behind_the_dots_beside_its_title() {
        let w = tasks();
        let root = w.root("Tasks");
        let task = w.task(&root, "milk");
        let window = w.window("task_page", &task);
        let tree = window.tree();
        one_bar(tree);
        let title = title_field(tree);
        let path = path_to(tree, &title.key).expect("the title stands in the tree");
        let parent = path[path.len() - 2];
        let at = parent
            .children
            .iter()
            .position(|child| child.key == title.key)
            .expect("the title is its parent's child");
        let more = parent
            .children
            .iter()
            .skip(at + 1)
            .find(|child| child.kind == "ui.MenuButton" && child.key.ends_with("~more"))
            .unwrap_or_else(|| panic!("a \"…\" follows the title:\n{}", dump(tree)));
        let commands: Vec<&UiNode> = more
            .children
            .iter()
            .filter(|child| child.kind == "ui.MenuItem")
            .collect();
        assert!(
            !commands.is_empty() && commands.iter().all(|c| c.events.contains_key("choose")),
            "the \"…\" holds the actions on the task:\n{}",
            dump(tree)
        );
        assert!(
            of_kind(tree, "ui.ViewTabs").is_empty(),
            "the task has one view to show it through:\n{}",
            dump(tree)
        );
        assert!(
            of_kind(tree, "ui.Button")
                .iter()
                .all(|button| button.props.get("role") != Some(&Value::text("primary"))),
            "no primary button of the task's actions:\n{}",
            dump(tree)
        );
    }

    /// A page whose item offers nothing shows no "…": the door would open on nothing, taking a
    /// line of the page for a control with nothing behind it.
    #[test]
    fn a_page_whose_item_offers_nothing_shows_no_dots() {
        const QUIET: &str = r#"
app so.tilo.test.quiet "Quiet"
type Quiet {
    title: Text
    notes: Text
}
type Quiets {
    items: Collection<Quiet>
}
root Quiets
default view all for Quiets
default view quiet_page for Quiet
primary action new(q: Quiets) -> Quiet (icon: .plus) {
    q.items.insert(Quiet(title: "Nothing to do"))
}
view all(q: Quiets) {
    std.page(q) {
        ui.Text("Quiets")
    }
}
view quiet_page(q: Quiet) {
    std.document(q) {
        ui.TextInput(bind: q.title, placeholder: "Title")
    }
}
"#;
        let w = install(QUIET);
        let root = w.root("Quiets");
        let item = returned_ref(w.call("new", vec![Value::resource(&root)]));
        let window = w.window("quiet_page", &item);
        let tree = window.tree();
        assert!(
            of_kind(tree, "ui.MenuButton").is_empty(),
            "an item with nothing to do shows no \"…\":\n{}",
            dump(tree)
        );
        assert_eq!(
            of_kind(tree, "ui.TextInput").len(),
            1,
            "the page edits its title:\n{}",
            dump(tree)
        );
    }

    /// A page whose item offers commands keeps its "…" at the trailing edge of the line its
    /// title is on, whatever the view left the title in: the door stands beside the title, not
    /// on a line of its own under it.
    #[test]
    fn a_pages_dots_stand_beside_its_title() {
        let w = install(BOARD);
        let root = w.root("Board");
        let column = returned_ref(w.call("new_column", vec![Value::resource(&root)]));
        let card = returned_ref(w.call(
            "new_card",
            vec![
                Value::resource(&column),
                Value::text("Merge the offline queue"),
            ],
        ));
        assert_eq!(
            w.rt.read(&card, "title").unwrap(),
            Value::text("Merge the offline queue"),
            "the card is named what the call gave it"
        );
        let window = w.window("card_page", &card);
        let tree = window.tree();
        let title = title_field(tree);
        let path = path_to(tree, &title.key).expect("the title stands in the tree");
        let line = path[path.len() - 2];
        assert_eq!(
            line.kind,
            "row",
            "the title stands on a line:\n{}",
            dump(tree)
        );
        let more = line
            .children
            .iter()
            .find(|child| child.kind == "ui.MenuButton" && child.key.ends_with("~more"))
            .unwrap_or_else(|| panic!("a \"…\" stands beside the title:\n{}", dump(tree)));
        assert!(
            more.children
                .iter()
                .any(|child| child.kind == "ui.MenuItem"),
            "the \"…\" holds the commands on the card:\n{}",
            dump(tree)
        );
        assert_eq!(
            line.children.last().map(|child| child.key.clone()),
            Some(more.key.clone()),
            "the \"…\" stands at the line's trailing edge:\n{}",
            dump(tree)
        );
        assert!(
            title.props.contains_key("fill"),
            "the title spans the line it is on:\n{}",
            dump(tree)
        );
    }

    /// A project's window offers the views its type declares as labelled tabs in its bar, the
    /// one the window opens on chosen; choosing another shows the project through it, and the
    /// window keeps it.
    #[test]
    fn a_projects_bar_switches_between_the_views_its_type_declares() {
        let w = tasks();
        let root = w.root("Tasks");
        let project = w.project(&root, "Move flat");
        w.call(
            "add_to",
            vec![Value::resource(&project), Value::text("Pack the flat")],
        );
        let mut window = w.window("project_list", &project);
        let tabs = {
            let tree = window.tree();
            let tabs = of_kind(tree, "ui.ViewTabs");
            assert_eq!(tabs.len(), 1, "the bar switches views:\n{}", dump(tree));
            tabs[0].clone()
        };
        let labels: Vec<String> = tabs
            .children
            .iter()
            .filter(|child| child.kind == "ui.Segment")
            .map(|segment| text_of(segment, "label"))
            .collect();
        assert_eq!(
            labels,
            ["List", "Board"],
            "each view says what it adds to the type's name"
        );
        assert_eq!(
            tabs.props.get("selected"),
            Some(&Value::Opt(Some(Box::new(Value::Int(0))))),
            "the window opens on the list"
        );
        // Choosing the board shows the project through it: a lane per section, and no row.
        window
            .fire_with(&tabs.key, "change", Some(&Value::Int(1)))
            .unwrap();
        window.poll_patches().unwrap();
        let tree = window.tree();
        assert_eq!(
            of_kind(tree, "ui.Board").len(),
            1,
            "the board is shown:\n{}",
            dump(tree)
        );
        assert!(
            of_kind(tree, "ui.ListItem").is_empty(),
            "the list is gone:\n{}",
            dump(tree)
        );
        let tabs = &of_kind(tree, "ui.ViewTabs")[0];
        assert_eq!(
            tabs.props.get("selected"),
            Some(&Value::Opt(Some(Box::new(Value::Int(1))))),
            "the bar shows which view the window is in"
        );
        // The same window again, as the shell opens it when the window comes back in front.
        let reopened = w.window("project_list", &project);
        assert_eq!(
            of_kind(reopened.tree(), "ui.Board").len(),
            1,
            "the window keeps the view it was left in:\n{}",
            dump(reopened.tree())
        );
    }

    /// No app's views read the actions on what they show back to build chrome of their own: the
    /// host places them where every window keeps them, and a view that read them would draw a
    /// second set beside the bar's.
    #[test]
    fn no_app_view_reads_the_actions_of_what_it_shows() {
        fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).expect("apps/tl reads") {
                let path = entry.expect("apps/tl reads").path();
                if path.is_dir() {
                    sources(&path, out);
                } else if path.extension().is_some_and(|e| e == "tl") {
                    out.push(path);
                }
            }
        }
        // A call of the builtin `actions(r)`: the word alone, not the end of a longer name
        // (`bar_lists_actions`), a component's (`ui.Reactions`) or a comment's.
        fn call_at(line: &str) -> Option<usize> {
            line.match_indices("actions(")
                .map(|(at, _)| at)
                .find(|&at| {
                    let before = line.get(..at).and_then(|before| before.chars().next_back());
                    !before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
                })
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl");
        let mut files = Vec::new();
        sources(&root, &mut files);
        assert!(files.len() >= 8, "every app in apps/tl is read");
        let mut offenders = Vec::new();
        for file in &files {
            let source = std::fs::read_to_string(file).expect("an app's source reads");
            for (number, line) in source.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                if call_at(line).is_some() {
                    offenders.push(format!(
                        "{}:{}: {}",
                        file.strip_prefix(&root).unwrap().display(),
                        number + 1,
                        line.trim()
                    ));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "views draw no chrome of their own from the actions on what they show; the host \
             places them:\n{}",
            offenders.join("\n")
        );
    }
}
