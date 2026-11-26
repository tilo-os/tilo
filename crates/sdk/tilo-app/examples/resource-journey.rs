//! A checklist whose controls and semantic callers use the same actions.
//!
//! Run with `cargo run -p tilo-app --example resource-journey`.
//! Prove its platform journey without a display with
//! `cargo test --locked -p tilo-app --example resource-journey`.

use tilo_app::i18n::EmbeddedCatalog;
use tilo_app::prelude::*;

static CATALOG: EmbeddedCatalog =
    EmbeddedCatalog::new("en-US", &[("en-US", include_str!("resource-journey.ftl"))]);

#[derive(Model, Debug, Clone, PartialEq)]
/// One task, with the platform-provided author of its creation.
pub struct Task {
    pub title: String,
    pub done: bool,
    pub author: tilo_resource::MemberId,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
/// The collaborative draft and stable, ordered task collection.
pub struct Checklist {
    pub draft: String,
    pub tasks: Collection<Task>,
}

#[resource(kind = "example.resource-journey")]
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
        let author = cx.author().ok_or("Sign in before adding a task")?.clone();
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
        // Every level still invokes ADD through the same host API.
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
