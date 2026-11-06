//! The state a scenario stages a window at (`window place = Task.done`): a scenario says which
//! place its window stands on, so a capture lays out a place that is not the declaration's
//! default, and the person's own choice still wins over what the scenario staged.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{AmStore, Principal, ResourceRef, Runtime, UiNode, Value, ViewInstance};

    const TASKS: &str = include_str!("../../../../apps/tl/tasks/app.tl");
    const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");

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
        /// The app's root resource, as installing it makes one.
        fn root(&self, ty: &str) -> ResourceRef {
            self.rt.create_root(ty).unwrap()
        }

        fn call(&self, action: &str, args: Vec<Value>) -> tilolang_runtime::InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&self.me, (id.as_str(), args))
                .unwrap()
        }

        fn task(&self, root: &ResourceRef, title: &str) {
            self.call("new_task", vec![Value::resource(root), Value::text(title)]);
        }

        /// Run the app's scenario `name`, as the host stages it before it lays a window out.
        fn stage(&self, name: &str) {
            let action = self
                .app
                .meta()
                .scenarios
                .iter()
                .find(|scenario| scenario.name == name)
                .unwrap_or_else(|| panic!("no scenario {name}"))
                .action
                .clone();
            self.rt
                .invoke_action(&self.me, (action.as_str(), vec![]))
                .unwrap();
        }

        /// The window of `view` on `resource`, every row of its lists rendered.
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

    /// The title a window's bar shows for the place it stands on.
    fn title(tree: &UiNode) -> String {
        let shell = of_kind(tree, "ui.NavShell")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("a window has a bar:\n{}", dump(tree)));
        shell
            .props
            .get("overview_title")
            .and_then(Value::as_text)
            .unwrap_or_else(|| panic!("the bar titles the window:\n{}", dump(tree)))
            .to_owned()
    }

    /// The placeholder of the window's entry: what it says is typed where.
    fn entry_placeholder(tree: &UiNode) -> String {
        let entry = of_kind(tree, "ui.TextInput")
            .into_iter()
            .find(|field| field.props.get("entry") == Some(&Value::Int(1)))
            .unwrap_or_else(|| panic!("the list ends in its entry:\n{}", dump(tree)))
            .clone();
        entry
            .props
            .get("placeholder")
            .and_then(Value::as_text)
            .unwrap_or_default()
            .to_owned()
    }

    /// A window whose place its scenario stages stands on that place: the bar names it, and the
    /// entry at the list's end says what typing there does, where the declaration's default
    /// would have made both say Inbox.
    #[test]
    fn a_scenario_stages_the_place_its_window_stands_on() {
        let w = tasks();
        let root = w.root("Tasks");
        w.task(&root, "milk");
        let plain = w.window("home", &root);
        assert_eq!(title(plain.tree()), "Inbox", "the declaration's default");
        assert_eq!(entry_placeholder(plain.tree()), "New task");

        w.stage("today");
        let today = w.window("home", &root);
        assert_eq!(
            title(today.tree()),
            "Today",
            "the place the scenario staged:\n{}",
            dump(today.tree())
        );
        assert_eq!(
            entry_placeholder(today.tree()),
            "New task for today",
            "what typing in the window does follows the place it stands on"
        );

        w.stage("what got done this week");
        let done = w.window("home", &root);
        assert_eq!(
            title(done.tree()),
            "Done",
            "the other place the app stages:\n{}",
            dump(done.tree())
        );
        assert!(
            w.rt.take_view_writes().is_empty(),
            "what a scenario stages is no one's change to keep"
        );
    }

    /// A place a person picked is theirs: it is what their window shows, whatever a scenario
    /// staged the declaration at, and it is kept as their change.
    #[test]
    fn a_place_a_person_picked_wins_over_what_a_scenario_staged() {
        const PLACES: &str = r#"
app so.tilo.test.places "Places"
type Note {
    title: Text (title)
}
type Notes {
    items: Collection<Note>
}
enum Side {
    left
    right
}
root Notes
default view all for Notes
view all(n: Notes) {
    window side: Side = Side.left
    let heading = if side == Side.left { "left" } else { "right" }
    ui.NavShell(content_kind: .document, overview_title: heading) {
        overview: {
            if side == Side.left {
                button("Go right", on: set(side, Side.right))
            } else {
                button("Go left", on: set(side, Side.left))
            }
        }
        content: {
            ui.EmptyState("Nothing here")
        }
    }
}
scenario "on the right" {
    window side = Side.right
}
"#;
        let w = install(PLACES);
        let root = w.root("Notes");
        w.stage("on the right");
        let mut window = w.window("all", &root);
        assert_eq!(title(window.tree()), "right", "the staged side");
        let go_left = of_kind(window.tree(), "ui.Button")
            .into_iter()
            .find(|node| node.props.get("label") == Some(&Value::text("Go left")))
            .unwrap_or_else(|| panic!("{}", dump(window.tree())))
            .clone();
        window.press(&go_left.key).unwrap();
        window.poll_patches().unwrap();
        assert_eq!(title(window.tree()), "left", "the side the person picked");
        // What they picked is theirs to keep, and the window shows it from then on, though the
        // scenario still stages the other side.
        let writes = w.rt.take_view_writes();
        assert_eq!(writes.len(), 1, "the person's pick: {writes:?}");
        w.stage("on the right");
        let reopened = w.window("all", &root);
        assert_eq!(
            title(reopened.tree()),
            "left",
            "the person's choice stands over the scenario's"
        );
    }
}
