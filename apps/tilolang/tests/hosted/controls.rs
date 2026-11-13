//! What a window's controls do, and where they stand: how a test finds a control without the
//! words the person's language gives it and without where the window's composition drew it.
//!
//! A control that runs an app action says which one — the record kind and the operation, in the
//! app's own ids — and one that edits a record's field says which field. The window's
//! composition moves the control between its bar, an empty state and a columns' header row and
//! the person's language renames it; neither changes what it does.

use super::packages::{package, package_of};
use super::{Window, content_nodes, find};
use collaboration::{CollabEvent, CollabRequest};

impl super::Session {
    /// The centre of the open note's body.
    pub(super) fn body(&mut self) -> (f64, f64) {
        self.field("body")
    }

    /// The key in the composed tree of the field that edits `field` of what the window shows: the
    /// title of the note it has open, its body, the title of the task.
    ///
    /// A field is known by what it edits, never by the words it shows when it is empty: those are
    /// the app's, in the reader's language.
    pub(super) fn field_key(&mut self, field: &str) -> String {
        let field = self.field_id(field);
        let editing = |node: &tilo_ui_host::UiNode| {
            node.binding
                .as_ref()
                .and_then(|binding| binding.field())
                .is_some_and(|(_, edited)| edited == field)
        };
        let view = self.hosted().view().expect("the window is connected");
        // A window whose page is another's after it opens the page in the lane after its own;
        // one whose page is its own (a tree beside what it shows) draws it in its content.
        if let Some(detail) = view.detail_tree() {
            let key = find(&detail, &editing)
                .unwrap_or_else(|| panic!("the window edits no {field:?}"))
                .key
                .clone();
            return format!("{}{}", tilo_ui_host::shell::DETAIL, key);
        }
        let root = view.root_tree().expect("the window's view");
        let mut page = Vec::new();
        content_nodes(&root, &mut page);
        page.iter()
            .find_map(|node| find(node, &editing))
            .unwrap_or_else(|| panic!("the window edits no {field:?}"))
            .key
            .clone()
    }

    /// The centre of the field that edits `field` (`title`, `body`) of what the window shows.
    pub(super) fn field(&mut self, field: &str) -> (f64, f64) {
        let editing = self.field_id(field);
        let doing = self
            .controls()
            .into_iter()
            .find(|control| {
                control
                    .doing
                    .as_deref()
                    .and_then(ui::tilo_actions::ActionAddress::parse)
                    .is_some_and(|doing| doing.action == editing)
            })
            .unwrap_or_else(|| panic!("no field edits {field}: {:?}", self.controls()));
        (doing.at.0 + doing.at.2 / 2.0, doing.at.1 + doing.at.3 / 2.0)
    }

    /// What the field that edits `field` of what the window shows holds.
    pub(super) fn field_value(&mut self, field: &str) -> Option<tilo_ui_host::Value> {
        let id = self.field_key(field);
        self.hosted().view()?.bound_value(&id)
    }

    /// The centre of the control that does `identity`: the action the app publishes, on the kind
    /// of record it acts on. Where it is the window's business — its bar while it lists nothing,
    /// its columns' header once it does — and so is what the person's language calls it.
    pub(super) fn control(&mut self, identity: &(String, String)) -> (f64, f64) {
        let (kind, action) = identity;
        let control = self
            .controls()
            .into_iter()
            .find(|control| {
                control
                    .doing
                    .as_deref()
                    .and_then(ui::tilo_actions::ActionAddress::parse)
                    .is_some_and(|doing| doing.resource == *kind && doing.action == *action)
            })
            .unwrap_or_else(|| {
                panic!(
                    "no control does {action} on {kind}: {:?}",
                    self.controls()
                        .into_iter()
                        .map(|control| (control.doing, control.name))
                        .collect::<Vec<_>>()
                )
            });
        (
            control.at.0 + control.at.2 / 2.0,
            control.at.1 + control.at.3 / 2.0,
        )
    }

    /// The kind the app publishes its records of `ty` (`Notebook`) under, as the app names it.
    pub(super) fn kind(&self, ty: &str) -> String {
        format!("tl.{}.{ty}", self.app)
    }

    /// The identity of the app's action `action`, as it is declared (`new_note`), on records of
    /// `ty`: the record kind and the id the app declares the action by.
    pub(super) fn doing(&self, ty: &str, action: &str) -> (String, String) {
        let bundle = package(package_of(&self.app));
        let Some(declared) = bundle
            .meta
            .actions
            .iter()
            .find(|declared| declared.name == action)
        else {
            panic!("{} declares no action {action}", self.app);
        };
        (self.kind(ty), declared.id.clone())
    }

    /// The id the app gives the field `field`, as it is declared (`title`): what the control that
    /// edits it is known by, in the type that declares it.
    pub(super) fn field_id(&self, field: &str) -> String {
        let bundle = package(package_of(&self.app));
        let Some(declared) = bundle
            .meta
            .types
            .iter()
            .flat_map(|declared| &declared.fields)
            .find(|declared| declared.name == field)
        else {
            panic!("{} declares no field {field}", self.app);
        };
        declared.id.clone()
    }

    /// Every control the window shows, as its accessibility tree publishes it: what it does,
    /// what it is called, and where it stands.
    pub(super) fn controls(&mut self) -> Vec<ShownControl> {
        match &mut self.window {
            Window::Harness(harness) => harness
                .a11y_nodes()
                .into_iter()
                .filter_map(|(_, node)| {
                    let at = node.bounds()?;
                    Some(ShownControl {
                        doing: node.author_id().map(str::to_owned),
                        name: node.label().unwrap_or_default().to_owned(),
                        at: (at.x0, at.y0, at.x1 - at.x0, at.y1 - at.y0),
                    })
                })
                .collect(),
            Window::Live(app) => app
                .window_controls()
                .into_iter()
                .map(|control| ShownControl {
                    doing: control.identity,
                    name: control.name,
                    at: control.rect,
                })
                .collect(),
        }
    }
}

/// One control of a window as its accessibility tree publishes it.
#[derive(Debug)]
pub(super) struct ShownControl {
    /// What it does: the identity of the app action it runs, or of the field it edits. `None` for
    /// a control that runs none.
    doing: Option<String>,
    /// What the person's language calls it.
    name: String,
    /// Where it stands: its left and top edge, its width and its height.
    at: (f64, f64, f64, f64),
}

impl super::Session {
    /// Invoke what `identity` says a window's control does, at the door the broker delivers to:
    /// the kind of resource the action acts on and the id the app declares it by, which is what
    /// the window publishes of the control ([`super::Session::controls`]).
    pub(super) fn invoke_doing(
        &mut self,
        identity: &str,
        arguments: &serde_json::Value,
    ) -> ui::tilo_actions::InvocationUpdate {
        let doing = ui::tilo_actions::ActionAddress::parse(identity)
            .unwrap_or_else(|| panic!("{identity} names a resource and an action"));
        let id = 1;
        self.inbox()
            .borrow_mut()
            .push_back(CollabEvent::Invocation {
                id,
                invocation: ui::tilo_actions::InvocationId::new(),
                resource: doing.resource,
                action: doing.action,
                argument: Some(format!(
                    "{}{arguments}",
                    ui::tilo_actions::ARGUMENTS_JSON_PREFIX
                )),
                actor: "ada".to_owned(),
                context: None,
            });
        self.settle();
        self.sent
            .borrow()
            .iter()
            .rev()
            .find_map(|request| match request {
                CollabRequest::UpdateInvocation {
                    id: answered,
                    update,
                } if *answered == id => Some(update.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the door answered nothing of {identity}"))
    }

    /// What the window's control called `name` says it does: the identity every caller names it
    /// by, read off the control the person sees, as a tool or an agent reads it.
    pub(super) fn identity_named(&mut self, name: &str) -> String {
        self.controls()
            .into_iter()
            .find(|control| control.name == name)
            .unwrap_or_else(|| {
                panic!(
                    "the window shows no control called {name:?}: {:?}",
                    self.controls()
                        .into_iter()
                        .map(|control| (control.name, control.doing))
                        .collect::<Vec<_>>()
                )
            })
            .doing
            .unwrap_or_else(|| panic!("the control called {name:?} says nothing of what it does"))
    }
}

/// A control of a window is invoked by what it does, at the door a tool or another app calls: the
/// identity the window publishes is the app's own action, so what a person presses and what a
/// caller names are one.
#[test]
fn a_new_note_is_invoked_at_the_door_by_what_its_control_does() {
    let _settled = ui::motion::testing::settled();
    let mut session = super::Session::live_notes();
    let new_note = session.identity_named("New note");
    assert!(
        session.titles().is_empty(),
        "the notebook starts with no note"
    );
    let answered = session.invoke_doing(&new_note, &serde_json::json!({}));
    assert!(
        matches!(
            answered,
            ui::tilo_actions::InvocationUpdate::Succeeded { .. }
        ),
        "the door ran what the control does: {answered:?}"
    );
    assert_eq!(
        session.titles(),
        ["Untitled".to_owned()],
        "the note New note makes is in the notebook"
    );
}

/// A control that takes an argument is invoked by what it does, with that argument: the Tasks
/// window's New task, run at the door as another app's call or an agent's reaches it, makes the
/// task it names.
#[test]
fn a_new_task_is_invoked_at_the_door_by_what_its_control_does_and_its_argument() {
    let _settled = ui::motion::testing::settled();
    let mut session = super::Session::live_tasks();
    let new_task = session.identity_named("New task");
    let answered = session.invoke_doing(
        &new_task,
        &serde_json::json!({ "title": "Water the plants" }),
    );
    assert!(
        matches!(
            answered,
            ui::tilo_actions::InvocationUpdate::Succeeded { .. }
        ),
        "the door ran what the control does: {answered:?}"
    );
    assert!(
        super::task_in_record(&session, "Water the plants", false),
        "the task named by the argument is in the record"
    );
}

/// A row's controls name the row they belong to: the second task's Complete, invoked at the door by
/// what it does, completes that task alone, and the first is left as it was.
#[test]
fn a_tasks_complete_completes_the_task_its_row_names() {
    let _settled = ui::motion::testing::settled();
    let mut session = super::Session::live_tasks();
    let new_task = session.identity_named("New task");
    for title in ["Water the plants", "Call the plumber"] {
        let answered = session.invoke_doing(
            &new_task,
            &serde_json::json!({ "title": title, "due": today() }),
        );
        assert!(
            matches!(
                answered,
                ui::tilo_actions::InvocationUpdate::Succeeded { .. }
            ),
            "the door made {title:?}: {answered:?}"
        );
    }
    // The second row's menu, opened where the row stands.
    let (row, _) = super::described(session.window.app(), "Call the plumber").expect("the row");
    session
        .window
        .app()
        .dispatch_a11y_action(row, &ui::a11y::Action::ShowContextMenu);
    session.settle();
    let complete = session.identity_named("Complete");
    let answered = session.invoke_doing(&complete, &serde_json::json!({}));
    assert!(
        matches!(
            answered,
            ui::tilo_actions::InvocationUpdate::Succeeded { .. }
        ),
        "the door ran what the row's Complete does: {answered:?}"
    );
    assert!(
        super::task_in_record(&session, "Call the plumber", true),
        "the second task is done: {:?}",
        session.titles()
    );
    assert!(
        super::task_in_record(&session, "Water the plants", false),
        "the task nobody named is still to do"
    );
}

/// Today's own command acts on another app's records, which Today's window publishes no resource
/// of: the identity its control publishes resolves at Today's door all the same, and the cleanup
/// runs.
#[test]
fn todays_cleanup_runs_at_the_door_by_what_its_control_does() {
    let _settled = ui::motion::testing::settled();
    let mut session = super::Session::today(super::Window::live);
    let cleanup = session.identity_named("Morning cleanup");
    // The two places its command acts on, as the window's own cards name them.
    let list = card_record(&mut session, "Today");
    let notebook = card_record(&mut session, "Pinned");
    let answered = session.invoke_doing(
        &cleanup,
        &serde_json::json!({ "list": list, "nb": notebook }),
    );
    assert!(
        matches!(
            answered,
            ui::tilo_actions::InvocationUpdate::Succeeded { .. }
        ),
        "the door ran what the control does: {answered:?}"
    );
}

/// Today's date, as the window's own clock reads it: a task due by then stands in the day's list.
fn today() -> String {
    ui::clock::today().format("%Y-%m-%d").to_string()
}

/// The record the card `title` on a window shows, as an argument names a resource: the place of
/// another app's the card stands for, by the id and type the window publishes it with.
fn card_record(session: &mut super::Session, title: &str) -> serde_json::Value {
    fn walk(node: &tilo_ui_host::UiNode, title: &str, found: &mut Option<serde_json::Value>) {
        if found.is_none()
            && node.kind == tilo_ui_host::adapter::kind("Card")
            && node.props.get("title") == Some(&tilo_ui_host::Value::text(title))
            && let Some(tilo_ui_host::Value::Ref(record)) = node.props.get("item")
        {
            // The catalogue's reference to it, as any caller of the app's door gives a record.
            let (app, ty) = record
                .ty
                .split_once('#')
                .unwrap_or((record.ty.as_str(), ""));
            *found = Some(serde_json::json!({
                "app": app,
                "kind": format!("tl.{app}.{ty}"),
                "id": record.id,
                "label": title,
            }));
        }
        for child in &node.children {
            walk(child, title, found);
        }
    }
    let view = session.hosted().view().expect("the window is connected");
    let root = view.root_tree().expect("the window's view");
    let mut found = None;
    walk(&root, title, &mut found);
    found.unwrap_or_else(|| panic!("the window shows no card called {title:?}"))
}
