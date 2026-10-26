//! A checklist whose controls and semantic callers use the same actions.
//!
//! Run with `cargo run -p tilo-app --example resource-journey`.
//! Prove its platform journey without a display with
//! `cargo test --locked -p tilo-app --example resource-journey`.

use tilo_app::i18n::EmbeddedCatalog;
use tilo_app::prelude::*;

static CATALOG: EmbeddedCatalog =
    EmbeddedCatalog::new("en-US", &[("en-US", include_str!("resource-journey.ftl"))]);

#[derive(Model, Default, Debug, Clone, PartialEq)]
/// One task, with the platform-provided author of its creation.
pub struct Task {
    pub title: String,
    pub done: bool,
    pub author: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
/// The collaborative draft and stable, ordered task collection.
pub struct Checklist {
    pub draft: String,
    pub tasks: Collection<Task>,
}

#[resource(kind = "example.resource-journey", label = "Checklist")]
impl Checklist {
    /// Add the drafted task.
    ///
    /// # Errors
    /// Refuses an empty title or an invocation without an author.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx) -> Result<(), &'static str> {
        let title = self.draft.trim().to_owned();
        if title.is_empty() {
            return Err("Give the task a title before adding it");
        }
        let author = cx
            .author()
            .ok_or("Sign in before adding a task")?
            .to_owned();
        self.tasks.push(
            cx.id(),
            Task {
                title,
                done: false,
                author,
            },
        );
        self.draft.clear();
        Ok(())
    }

    /// Remove a task after confirmation.
    ///
    /// # Errors
    /// Refuses a task that no longer exists.
    #[action(destructive)]
    pub fn remove(&mut self, task: &Id) -> Result<(), &'static str> {
        self.tasks
            .remove(task)
            .map(drop)
            .ok_or("This task no longer exists")
    }
}

/// A custom component keeps the shared button's focus, semantics and binding.
/// Override a decorator hook only when the component needs different behavior.
struct AddTask {
    button: Button,
}

impl tilo_app::widget::Decorator for AddTask {
    type Inner = Button;

    fn inner(&self) -> &Button {
        &self.button
    }

    fn inner_mut(&mut self) -> &mut Button {
        &mut self.button
    }
}

/// A view over the checklist; persistence and authority belong to the host.
pub struct ChecklistApp;

impl ResourceApp for ChecklistApp {
    type Resource = Checklist;
    const ID: &'static str = "so.tilo.example.resource-journey";
    const TITLE: &'static str = "Checklist";

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn view(cx: &mut ViewCx<Checklist>) -> Element {
        let draft = cx
            .input(Checklist::paths().draft)
            .with_id("draft")
            .with_placeholder("journey-draft");
        // Progressive disclosure: start with the shorthand, then use the
        // shared control's presentation API, then wrap it in a component.
        // Every level still invokes ADD through the same host door.
        let add = cx.button("journey-add", Checklist::ADD.invoke());
        let actions = cx.actions();
        let icon = actions.bind_invocation(
            Button::new("add-icon", "")
                .with_icon(tilo_app::icons::Icon::Plus)
                .with_accessible_name("journey-add")
                .tooltip("journey-add"),
            Checklist::ADD.invoke(),
        );
        let label = actions.bind_invocation(
            Button::new("add-label", "journey-add-label").with_icon(tilo_app::icons::Icon::Plus),
            Checklist::ADD.invoke(),
        );
        let component = AddTask {
            button: actions.bind_invocation(
                Button::new("add-component", "journey-add"),
                Checklist::ADD.invoke(),
            ),
        };
        let rows = cx.each(
            |list| &list.tasks,
            |row| {
                let Some(id) = row.item().cloned() else {
                    return Box::new(Column::new()) as Element;
                };
                Box::new(tilo_app::row![
                    row.text(|task| task.title.clone()),
                    row.checkbox("journey-done", row.paths().done),
                    row.button("journey-remove", Checklist::REMOVE.with(id)),
                ]) as Element
            },
        );
        Box::new(tilo_app::column![
            draft,
            tilo_app::row![add, icon, label, component],
            rows
        ])
    }
}

fn main() {
    tilo_app::launch::run_or_exit(&tilo_app::framework::launch_spec::<ChecklistApp>());
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tilo_actions::{ActionDescriptor, DoorError};
    use tilo_app::Role;

    fn task_action(app: &mut TestApp<ChecklistApp>, action: &str) -> ActionDescriptor {
        app.actions()
            .expect("discover current actions")
            .into_iter()
            .find(|entry| entry.address.action == action && entry.resource_label == "Pack lunch")
            .expect("the task publishes its own action")
    }

    #[test]
    fn a_semantic_caller_discovers_edits_confirms_undoes_and_reopens() {
        let mut app = TestApp::<ChecklistApp>::for_person("ada").expect("open");
        assert!(app.state().tasks.is_empty(), "the live app starts empty");
        let versions = app.versions();
        assert!(
            matches!(app.call("add", json!({})), Err(DoorError::Failed(message))
            if message.contains("Give the task a title"))
        );
        assert_eq!(
            app.versions(),
            versions,
            "a refused action writes no history"
        );

        app.harness()
            .perform_named(
                "draft",
                &tilo_app::a11y::Action::SetValue("Pack lunch".into()),
            )
            .expect("the bound input edits the draft");
        app.call("add", json!({})).expect("add");
        assert_eq!(
            app.state().tasks.values().next().expect("task").author,
            "ada"
        );

        let id = app.state().tasks.ids().next().cloned().expect("task id");
        let done = Checklist::paths().tasks.item(&id).done;
        let wrong: serde_json::Value = serde_json::from_str(
            &serde_json::to_string(&done.set(true))
                .expect("an edit")
                .replace("true", "\"yes\""),
        )
        .expect("an edit");
        let before = app.state();
        assert!(app.call("field-edit", json!({ "edit": wrong })).is_err());
        assert_eq!(app.state(), before);
        app.call("field-edit", json!({ "edit": done.set(true) }))
            .expect("complete the task");
        assert!(app.state().tasks.values().all(|task| task.done));
        let remove = task_action(&mut app, "remove");
        // Undo takes a removal back, so nobody is asked first.
        assert_eq!(
            remove.contract.advertised_confirmation(),
            tilo_actions::ConfirmationPolicy::None
        );
        app.call_on(&remove.address.resource, &remove.address.action, json!({}))
            .expect("an undoable removal applies at once");
        assert!(app.state().tasks.is_empty());
        assert!(
            matches!(
                app.call_confirmed_on(&remove.address.resource, &remove.address.action, json!({})),
                Err(DoorError::Unknown(_))
            ),
            "a deleted task's address is stale"
        );
        assert!(app.undo(), "removal is one undo step");
        assert_eq!(task_action(&mut app, "remove").address, remove.address);
        app.call_on(&remove.address.resource, &remove.address.action, json!({}))
            .expect("removing again applies at once");
        assert!(app.undo(), "and Undo takes it back again");
        let reopened = app.reopen().expect("reopen");
        assert_eq!(reopened.state().tasks.len(), 1);
        assert!(
            reopened
                .state()
                .tasks
                .values()
                .all(|task| task.done && task.author == "ada")
        );
    }

    #[test]
    fn progressively_customized_controls_keep_semantics_focus_and_permissions() {
        use tilo_app::a11y::{Action, Role as AccessibleRole};
        use tilo_app::headless::SemanticExpectation;

        let mut app = TestApp::<ChecklistApp>::new().expect("open");
        let add_address = app
            .actions()
            .expect("discover the production action catalogue")
            .into_iter()
            .find(|entry| entry.address.action == "add")
            .expect("the checklist publishes Add")
            .address;
        let add_binding = tilo_app::widget::ControlBindingKind::Action(add_address);
        let bindings =
            tilo_app::widget::controls::control_action_bindings(app.harness().root_widget());
        let controls = [
            ("add", "Add task"),
            ("add-icon", "Add task"),
            ("add-label", "Add this task"),
            ("add-component", "Add task"),
        ];
        for (id, name) in controls {
            assert_eq!(
                bindings
                    .iter()
                    .filter(|binding| binding.control_id == id)
                    .map(|binding| binding.kind.clone())
                    .collect::<Vec<_>>(),
                vec![add_binding.clone()],
                "every presentation binds exactly the discovered Add address"
            );
            app.harness()
                .expect(
                    &SemanticExpectation::named(id)
                        .role(AccessibleRole::Button)
                        .name(name)
                        .enabled(true),
                )
                .expect("every presentation announces its action with the correct name");
            app.call(
                "field-edit",
                json!({ "edit": Checklist::paths().draft.replace(id) }),
            )
            .expect("draft");
            app.harness()
                .perform_named(id, &Action::Focus)
                .expect("focus the shared control");
            app.harness()
                .expect(&SemanticExpectation::named(id).focused(true))
                .expect("customization preserves focus");
            let before = app.state().tasks.len();
            app.harness().key("Enter");
            let state = app.state();
            assert_eq!(state.tasks.len(), before.saturating_add(1));
            assert_eq!(
                state.tasks.values().filter(|task| task.title == id).count(),
                1
            );
            assert!(state.draft.is_empty());
        }
        assert_eq!(app.state().tasks.len(), controls.len());
        assert!(app.undo(), "the last button activation is an undo step");
        assert_eq!(app.state().tasks.len(), controls.len().saturating_sub(1));
        app.call(
            "field-edit",
            json!({ "edit": Checklist::paths().draft.replace("Viewer cannot add") }),
        )
        .expect("draft");
        let before = app.state();
        app.set_role(Role::Viewer);
        for (id, name) in controls {
            app.harness()
                .expect(
                    &SemanticExpectation::named(id)
                        .role(AccessibleRole::Button)
                        .name(name)
                        .enabled(false),
                )
                .expect("customized controls inherit the live role change");
        }
        assert!(matches!(
            app.call("add", json!({})),
            Err(DoorError::Unknown(_))
        ));
        assert_eq!(app.state(), before);
        let versions = app.versions();
        // Revocation between focus and Enter must stop every presentation.
        for (id, _) in controls {
            app.set_role(Role::Owner);
            app.harness()
                .perform_named(id, &Action::Focus)
                .expect("focus while authorized");
            app.set_role(Role::Viewer);
            app.harness().key("Enter");
            assert_eq!(app.state(), before);
            assert_eq!(app.versions(), versions);
        }
    }

    #[test]
    fn confirmation_does_not_override_revoked_permissions_or_argument_validation() {
        let mut app = TestApp::<ChecklistApp>::new().expect("open");
        app.call(
            "field-edit",
            json!({ "edit": Checklist::paths().draft.replace("Pack lunch") }),
        )
        .expect("draft");
        app.call("add", json!({})).expect("add");
        let remove = task_action(&mut app, "remove");
        let before = app.state();
        let versions = app.versions();
        assert!(matches!(
            app.call_confirmed_on(
                &remove.address.resource,
                &remove.address.action,
                json!({"unexpected": true})
            ),
            Err(DoorError::InvalidArguments(_))
        ));
        assert!(matches!(
            app.call_confirmed("remove", json!([])),
            Err(DoorError::InvalidArguments(_))
        ));
        app.set_role(Role::Viewer);
        assert!(
            !app.actions()
                .expect("discover current actions")
                .iter()
                .any(|entry| entry.address == remove.address)
        );
        assert!(
            matches!(
                app.call_confirmed_on(&remove.address.resource, &remove.address.action, json!({})),
                Err(DoorError::Unknown(_))
            ),
            "saved descriptors grant no authority"
        );
        assert_eq!(app.state(), before);
        assert_eq!(app.versions(), versions);
        app.set_role(Role::Owner);
        let id = app.state().tasks.ids().next().cloned().expect("task id");
        app.call_confirmed("remove", json!({"task": id}))
            .expect("the root action also admits a confirmed, typed removal");
        assert!(app.state().tasks.is_empty());
    }
}
