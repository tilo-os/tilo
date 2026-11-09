//! The state a run stages a window at (`layout-check --state place=Task.done`): a run says which
//! place its windows stand on, so a capture lays out a place that is not the declaration's
//! default, and the person's own choice still wins over what the run staged.

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

        /// Run the app's action `name`.
        fn invoke(&self, action: &str, args: Vec<Value>) -> tilolang_runtime::InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&self.me, (id.as_str(), args))
                .unwrap()
        }

        fn task(&self, root: &ResourceRef, title: &str) {
            self.invoke(
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
            );
        }

        /// Stage the app's state `state` at `text`, as a run does before it opens a window.
        fn stage(&self, state: &str, text: &str) {
            self.rt
                .stage_view_state(&self.app.meta().app, state, text)
                .unwrap_or_else(|e| panic!("--state {state}={text}: {e}"));
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

    /// The title a window's bar shows for the place it stands on: the content column's, or in a
    /// window whose overview is what a person chooses from, the overview's.
    fn title(tree: &UiNode) -> String {
        let shell = of_kind(tree, "ui.NavShell")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("a window has a bar:\n{}", dump(tree)));
        ["title", "overview_title"]
            .iter()
            .find_map(|prop| shell.props.get(*prop).and_then(Value::as_text))
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

    /// A window whose place the run stages stands on that place: the bar names it, and the entry
    /// at the list's end says what typing there does, where the declaration's default would have
    /// made both say Today.
    #[test]
    fn a_run_stages_the_place_its_window_stands_on() {
        let w = tasks();
        let root = w.root("Tasks");
        w.task(&root, "milk");
        let plain = w.window("home", &root);
        assert_eq!(title(plain.tree()), "Today", "the declaration's default");
        assert_eq!(entry_placeholder(plain.tree()), "New task for today");

        w.stage("place", "Task.inbox");
        let inbox = w.window("home", &root);
        assert_eq!(
            title(inbox.tree()),
            "Inbox",
            "the place the run staged:\n{}",
            dump(inbox.tree())
        );
        assert_eq!(
            entry_placeholder(inbox.tree()),
            "New task",
            "what typing in the window does follows the place it stands on"
        );

        w.stage("place", "Task.done");
        let done = w.window("home", &root);
        assert_eq!(
            title(done.tree()),
            "Done",
            "the other place a run stages:\n{}",
            dump(done.tree())
        );
        assert!(
            w.rt.take_view_writes().is_empty(),
            "what a run stages is no one's change to keep"
        );
    }

    /// What a run stages is the value the app's own code names: a filter of the type the state
    /// queries, a case of the enum it holds, or a literal, and nothing else.
    #[test]
    fn staging_names_a_value_the_app_declares() {
        let w = tasks();
        let error = |state: &str, text: &str| {
            w.rt.stage_view_state(&w.app.meta().app, state, text)
                .unwrap_err()
        };
        assert!(
            error("place", "Task.nonsense").contains("declares no filter `nonsense`"),
            "{}",
            error("place", "Task.nonsense")
        );
        assert!(
            error("place", "Proj.done").contains("holds the filters of `Task`"),
            "{}",
            error("place", "Proj.done")
        );
        assert!(
            error("nowhere", "Task.today").contains(&format!(
                "no view of `{}` declares the state",
                w.app.meta().app
            )),
            "{}",
            error("nowhere", "Task.today")
        );
    }

    /// A place a person picked is theirs: it is what their window shows, whatever a run staged
    /// the declaration at, and it is kept as their change.
    #[test]
    fn a_place_a_person_picked_wins_over_what_a_run_staged() {
        const PLACES: &str = r#"
app so.tilo.test.places "Places"
type Note {
    title: Text
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
    state side: Side = Side.left
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
"#;
        let w = install(PLACES);
        let root = w.root("Notes");
        w.stage("side", "Side.right");
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
        // run still stages the other side.
        let writes = w.rt.take_view_writes();
        assert_eq!(writes.len(), 1, "the person's pick: {writes:?}");
        w.stage("side", "Side.right");
        let reopened = w.window("all", &root);
        assert_eq!(
            title(reopened.tree()),
            "left",
            "the person's choice stands over the run's"
        );
    }
}
