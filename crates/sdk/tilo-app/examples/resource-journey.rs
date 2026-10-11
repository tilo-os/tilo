//! A checklist whose controls and semantic callers use the same actions.
//!
//! Run with `cargo run -p tilo-app --example resource-journey`.
//! Prove its platform journey without a display with
//! `cargo test --locked -p tilo-app --example resource-journey`.

use tilo_app::i18n::EmbeddedCatalog;
use tilo_app::prelude::*;

static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[("en-US", include_str!("resource-journey.ftl"))],
    "journey-draft",
);

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
    /// Set the next task's title.
    #[action]
    pub fn draft(&mut self, title: String) {
        self.draft = title;
    }

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

    /// Mark a task completed or open.
    ///
    /// # Errors
    /// Refuses a task that no longer exists.
    #[action]
    pub fn set_done(&mut self, task: &Id, done: bool) -> Result<(), &'static str> {
        self.tasks
            .get_mut(task)
            .ok_or("This task no longer exists")?
            .done = done;
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
            .input(|list| list.draft.clone(), Checklist::DRAFT)
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
                let done_id = id.clone();
                Box::new(tilo_app::row![
                    row.text(|task| task.title.clone()),
                    row.checkbox(
                        "journey-done",
                        |task| task.done,
                        Checklist::SET_DONE.map(move |done| (done_id.clone(), done))
                    ),
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
    fn agent_execution_merges_with_person_edits_and_survives_undo_and_reconnect() {
        use tilo_app::framework::{Participant, resource_kind};
        use tilo_app::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};

        let mut app = TestApp::<ChecklistApp>::for_person("ada").expect("open");
        app.call("draft", json!({"title": "Pack lunch"}))
            .expect("draft");
        app.call("add", json!({})).expect("add");
        let snapshot = app.read_document(|doc| doc.clone().save());
        let permissions = PermissionContext::from_capabilities(
            Actor::agent("scribe"),
            vec![CapOverride::grant(
                Subject::agent("scribe"),
                Scope::App(ChecklistApp::ID.into()),
                Verb::Write,
            )],
        );
        // This is an in-memory host fixture, not sign-in or a persistent grant.
        let mut agent = Participant::from_snapshot(
            resource_kind::<ChecklistApp>(),
            &snapshot,
            "scribe",
            permissions.clone(),
        )
        .expect("join the existing record");
        let complete = agent
            .operations()
            .into_iter()
            .find(|operation| {
                operation.address.action == "set-done" && operation.resource_label == "Pack lunch"
            })
            .expect("discover the task action");
        let changes = agent
            .invoke(
                &complete.address,
                json!({"done": true}).as_object().expect("arguments"),
            )
            .expect("execute through the agent door")
            .expect("an edit produces changes");
        assert!(!changes.is_empty());

        // Ada edits while the agent is disconnected. Neither overwrites the other.
        app.call("draft", json!({"title": "Make tea"}))
            .expect("local draft");
        app.call("add", json!({})).expect("local add");
        let mut peer = agent.doc().clone();
        app.merge_peer(&mut peer).expect("deliver the agent edit");
        let lunch = app
            .state()
            .tasks
            .ids()
            .find(|id| {
                app.state()
                    .tasks
                    .get(id)
                    .is_some_and(|task| task.title == "Pack lunch")
            })
            .cloned()
            .expect("stable lunch identity");
        assert_eq!(app.state().tasks.len(), 2);
        assert!(app.state().tasks.get(&lunch).expect("lunch").done);
        assert!(
            app.versions().iter().any(|version| {
                version
                    .context
                    .as_ref()
                    .and_then(|context| context.invoker.as_ref())
                    .is_some_and(|invoker| invoker.actor == "scribe")
            }),
            "the delivered action retains agent attribution in production history"
        );

        app.call("undo", json!({})).expect("undo Ada's last step");
        assert_eq!(app.state().tasks.len(), 1, "undo retains the peer's edit");
        assert!(
            app.state()
                .tasks
                .get(&lunch)
                .expect("lunch after undo")
                .done
        );
        app.call("redo", json!({})).expect("redo Ada's step");
        assert_eq!(app.state().tasks.len(), 2);
        assert!(
            app.harness()
                .inspection_json()
                .expect("inspect the live view")
                .contains("Pack lunch"),
            "the live view follows the merge"
        );

        // Reconnect from the agent's saved replica, then receive Ada's newer history.
        let saved = agent.save();
        let mut resumed = Participant::from_snapshot(
            resource_kind::<ChecklistApp>(),
            &saved,
            "scribe",
            permissions,
        )
        .expect("reconnect");
        let latest = app.read_document(|doc| doc.clone().save());
        assert!(resumed.merge_snapshot(&latest));
        assert_eq!(
            resumed.state::<Checklist>().expect("agent state"),
            app.state()
        );
        resumed.set_permissions(PermissionContext::from_capabilities(
            Actor::agent("scribe"),
            vec![],
        ));
        let before = resumed.heads();
        assert!(
            resumed
                .invoke(
                    &complete.address,
                    json!({"done": false}).as_object().expect("arguments"),
                )
                .is_err(),
            "a remembered address does not survive revoked authority"
        );
        assert_eq!(resumed.heads(), before, "refusal changes no history");

        let expected = app.state();
        let versions = app.versions();
        let mut reopened = app.reopen().expect("reopen the canonical record");
        assert_eq!(reopened.state(), expected);
        assert_eq!(reopened.versions(), versions);
        assert!(
            reopened
                .harness()
                .inspection_json()
                .expect("inspect the restored view")
                .contains("Pack lunch")
        );
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

        // Use the shared input control, then the same semantic door as a script.
        app.harness()
            .perform_named(
                "draft",
                &tilo_app::a11y::Action::SetValue("Pack lunch".into()),
            )
            .expect("the bound input edits the draft");
        app.call("add", json!({})).expect("add");
        let done = task_action(&mut app, "set-done");
        assert_eq!(done.params.len(), 1, "the item identity is already bound");
        assert_eq!(done.params.first().expect("done parameter").name, "done");
        assert_eq!(
            app.state().tasks.values().next().expect("task").author,
            "ada"
        );

        let before = app.state();
        assert!(matches!(
            app.call_on(
                &done.address.resource,
                &done.address.action,
                json!({"done": "yes"})
            ),
            Err(DoorError::InvalidArguments(_))
        ));
        assert_eq!(app.state(), before);
        app.call_on(
            &done.address.resource,
            &done.address.action,
            json!({"done": true}),
        )
        .expect("complete the discovered task");
        assert!(app.state().tasks.values().all(|task| task.done));
        let remove = task_action(&mut app, "remove");
        assert_eq!(
            remove.contract.advertised_confirmation(),
            tilo_actions::ConfirmationPolicy::Confirm
        );
        assert_eq!(
            app.call_on(&remove.address.resource, &remove.address.action, json!({})),
            Err(DoorError::ConfirmationRequired)
        );
        assert_eq!(app.state().tasks.len(), 1);
        app.call_confirmed_on(&remove.address.resource, &remove.address.action, json!({}))
            .expect("confirm only this removal");
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
        assert_eq!(
            app.call_on(&remove.address.resource, &remove.address.action, json!({})),
            Err(DoorError::ConfirmationRequired),
            "confirmation is not remembered"
        );
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
            app.call("draft", json!({"title": id})).expect("draft");
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
        app.call("draft", json!({"title": "Viewer cannot add"}))
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
        app.call("draft", json!({"title": "Pack lunch"}))
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
