//! What a window's controls do, and where they stand: how a test finds a control without the
//! words the person's language gives it and without where the window's composition drew it.
//!
//! A control that runs an app action says which one — the document kind and the operation, in the
//! app's own ids — and one that edits a document's field says which field. The window's
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
        let editing = |node: &tilo_app_window::UiNode| {
            node.binding
                .as_ref()
                .and_then(|binding| binding.field())
                .is_some_and(|(_, edited)| edited == field)
        };
        let view = self.hosted().view().expect("the window is connected");
        // A window whose page is another's after it opens the page in the level after its own;
        // one whose page is its own (a tree beside what it shows) draws it in its content.
        if let Some(detail) = view.detail_tree() {
            let key = find(&detail, &editing)
                .unwrap_or_else(|| panic!("the window edits no {field:?}"))
                .key
                .clone();
            return format!("{}{}", tilo_app_window::shell::DETAIL, key);
        }
        let root = view.root_tree().expect("the window's view");
        let mut page = Vec::new();
        content_nodes(&root, &mut page);
        // A window with no shell of its own draws its page in the tree itself.
        let page = if page.is_empty() { vec![&root] } else { page };
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
    pub(super) fn field_value(&mut self, field: &str) -> Option<tilo_app_window::Value> {
        let id = self.field_key(field);
        self.hosted().view()?.bound_value(&id)
    }

    /// The centre of the control that does `identity`: the action the app publishes, on the kind
    /// of record it acts on. Where it is the window's business — its bar or an empty state's call
    /// to action while it lists nothing, its columns' header once it does — and so is what the
    /// person's language calls it.
    pub(super) fn control(&mut self, identity: &(String, String)) -> (f64, f64) {
        let (kind, action) = identity;
        // The addresses the instance publishes the action under on documents of the kind: what an
        // agent reads, and what the control says it does.
        let published: Vec<String> = self
            .instance()
            .published()
            .iter()
            .filter(|descriptor| {
                descriptor.resource_kind == *kind && descriptor.address.action == *action
            })
            .map(|descriptor| descriptor.address.encode())
            .collect();
        let control = self
            .controls()
            .into_iter()
            .find(|control| {
                control
                    .doing
                    .as_ref()
                    .is_some_and(|doing| published.contains(doing))
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

    /// The kind the app publishes its documents of `ty` (`Notebook`) under, as the app names it.
    pub(super) fn kind(&self, ty: &str) -> String {
        format!("tl.{}.{ty}", self.app)
    }

    /// The app's action `action`, as it publishes it (`new_note`), on documents of `ty`: the document
    /// kind and the action, which [`Self::control`] finds the control of by the address the
    /// instance publishes for it.
    pub(super) fn doing(&self, ty: &str, action: &str) -> (String, String) {
        let bundle = package(package_of(&self.app));
        assert!(
            bundle
                .meta
                .actions
                .iter()
                .any(|declared| declared.name == action),
            "{} declares no action {action}",
            self.app
        );
        (self.kind(ty), action.to_owned())
    }

    /// The address the window's instance publishes for the app's action `action` on documents of
    /// `ty`, as the broker was handed it: what an agent reading the workspace's actions finds.
    pub(super) fn identity_doing(&self, ty: &str, action: &str) -> String {
        self.published_doing(&self.kind(ty), action)
    }

    /// The address the window's instance publishes for the action `action` on a resource of the
    /// kind `kind`.
    pub(super) fn published_doing(&self, kind: &str, action: &str) -> String {
        let published = self.instance().published();
        published
            .iter()
            .find(|descriptor| {
                descriptor.resource_kind == kind && descriptor.address.action == action
            })
            .unwrap_or_else(|| {
                panic!(
                    "{} publishes no {action} on a {kind}: {:?}",
                    self.app,
                    published
                        .iter()
                        .map(|descriptor| (&descriptor.resource_kind, descriptor.address.encode()))
                        .collect::<Vec<_>>()
                )
            })
            .address
            .encode()
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
    pub(super) name: String,
    /// Where it stands: its left and top edge, its width and its height.
    at: (f64, f64, f64, f64),
}

impl super::Session {
    /// Invoke what `identity` says a window's control does, at the action API the broker delivers
    /// to: the kind of resource the action acts on and the id the app declares it by, which is what
    /// the window publishes of the control ([`super::Session::controls`]).
    pub(super) fn invoke_doing(
        &mut self,
        identity: &str,
        arguments: &serde_json::Value,
    ) -> ui::tilo_actions::InvocationUpdate {
        self.invoke_as(
            &ui::tilo_actions::MemberId::person("ada").into(),
            identity,
            arguments,
        )
    }

    /// [`Self::invoke_doing`] as `actor` runs it: what another person's press or an agent's call
    /// writes in the workspace, which the window's own person has not seen.
    pub(super) fn invoke_as(
        &mut self,
        actor: &ui::tilo_actions::Actor,
        identity: &str,
        arguments: &serde_json::Value,
    ) -> ui::tilo_actions::InvocationUpdate {
        let doing = ui::tilo_actions::ActionAddress::parse(identity)
            .unwrap_or_else(|| panic!("{identity} names a resource and an action"));
        let arguments = format!("{}{arguments}", ui::tilo_actions::ARGUMENTS_JSON_PREFIX);
        self.deliver(actor, doing.resource, doing.action, Some(arguments), None)
            .unwrap_or_else(|| panic!("the action API answered nothing of {identity}"))
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

/// A control of a window is invoked by what it does, at the API a tool or another app calls: the
/// identity the window publishes is the app's own action, so what a person presses and what a
/// caller names are one.
#[test]
fn a_new_note_is_invoked_at_the_api_by_what_its_control_does() {
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
        "the API ran what the control does: {answered:?}"
    );
    assert_eq!(
        session.titles(),
        ["Untitled".to_owned()],
        "the note New note makes is in the notebook"
    );
}

/// An empty window's state offers the primary action and its bar keeps its own: the two are the
/// one control, so a caller that names what it does finds one — a button, where a press reaches
/// it, at the button's box and never the state's.
#[test]
fn an_empty_windows_state_and_bar_offer_the_primary_action_as_one_control() {
    let _settled = ui::motion::testing::settled();
    let mut session = super::Session::live_notes();
    let new_note = session.identity_doing("Notebook", "new_note");
    let shown: Vec<ShownControl> = session
        .controls()
        .into_iter()
        .filter(|control| control.doing.as_deref() == Some(new_note.as_str()))
        .collect();
    assert_eq!(
        shown.len(),
        1,
        "one control does new_note, wherever the window shows it: {shown:?}"
    );
    assert_eq!(shown[0].name, "New note");
    let (x, y) = (
        shown[0].at.0 + shown[0].at.2 / 2.0,
        shown[0].at.1 + shown[0].at.3 / 2.0,
    );
    session.window.click_at(x, y);
    session.settle();
    assert_eq!(
        session.titles(),
        ["Untitled".to_owned()],
        "the press reaches the action where the control stands"
    );
}

/// A control that takes an argument is invoked by what it does, with that argument: the Tasks
/// window's New task, run at the API as another app's call or an agent's reaches it, makes the
/// task it names.
#[test]
fn a_new_task_is_invoked_at_the_api_by_what_its_control_does_and_its_argument() {
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
        "the API ran what the control does: {answered:?}"
    );
    assert!(
        super::task_in_workspace(&session, "Water the plants", false),
        "the task named by the argument is in the document"
    );
}

/// A row's controls name the row they belong to: the second task's Complete, invoked at the API by
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
            "the API made {title:?}: {answered:?}"
        );
    }
    // The second row's menu, opened where the row stands.
    session.window.show_context_menu("Call the plumber");
    session.settle();
    let complete = session.identity_named("Complete");
    let answered = session.invoke_doing(&complete, &serde_json::json!({}));
    assert!(
        matches!(
            answered,
            ui::tilo_actions::InvocationUpdate::Succeeded { .. }
        ),
        "the API ran what the row's Complete does: {answered:?}"
    );
    assert!(
        super::task_in_workspace(&session, "Call the plumber", true),
        "the second task is done: {:?}",
        session.titles()
    );
    assert!(
        super::task_in_workspace(&session, "Water the plants", false),
        "the task nobody named is still to do"
    );
}

/// Today's own command acts on another app's documents, which Today's window publishes no resource
/// of: the identity its control publishes resolves at Today's api all the same, and the cleanup
/// runs.
#[test]
fn todays_cleanup_runs_at_the_api_by_what_its_control_does() {
    let _settled = ui::motion::testing::settled();
    let mut session = super::Session::today(super::Window::live);
    let cleanup = session.identity_named("Morning cleanup");
    // The two places its command acts on, as the window's own cards name them.
    let list = card_document(&mut session, "Today");
    let notebook = card_document(&mut session, "Pinned");
    let answered = session.invoke_doing(
        &cleanup,
        &serde_json::json!({ "list": list, "nb": notebook }),
    );
    assert!(
        matches!(
            answered,
            ui::tilo_actions::InvocationUpdate::Succeeded { .. }
        ),
        "the API ran what the control does: {answered:?}"
    );
}

/// Today's date, as the window's own clock reads it: a task due by then stands in the day's list.
fn today() -> String {
    ui::clock::today().to_string()
}

/// The document the card `title` on a window shows, as an argument names a resource: the place of
/// another app's the card stands for, by the id and type the window publishes it with.
fn card_document(session: &mut super::Session, title: &str) -> serde_json::Value {
    fn walk(node: &tilo_app_window::UiNode, title: &str, found: &mut Option<serde_json::Value>) {
        if found.is_none()
            && node.kind == tilo_app_window::adapter::kind("Card")
            && node.props.get("title") == Some(&tilo_app_window::Value::text(title))
            && let Some(tilo_app_window::Value::Ref(record)) = node.props.get("item")
        {
            // The catalogue's reference to it, as any caller of the app's api gives a document.
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

/// A box bound only to a field runs no action of its own and still says what it edits, as the
/// field's own entry does: the one document, by the id the app publishes it under, and the field,
/// in a box a view places itself and in a list row's leading column. A caller that names it so
/// marks the field, where a press and a screen reader's activation do.
#[test]
fn a_field_bound_checkbox_says_which_field_it_edits_and_marks_it_by_that_identity() {
    let _settled = ui::motion::testing::settled();
    let mut session = super::Session::open(super::FIELDBOX, "ada", super::Window::live);
    assert_eq!(
        session.field_value("done"),
        Some(tilo_app_window::Value::Bool(false)),
        "the page starts with the box unmarked"
    );
    let shown = session.controls();
    let editing = |id: &str| {
        shown
            .iter()
            .filter_map(|control| ui::tilo_actions::ActionAddress::parse(control.doing.as_ref()?))
            .find(|address| address.action == id)
            .unwrap_or_else(|| panic!("the window shows no control that edits {id}: {shown:?}"))
    };
    let done = editing(&session.field_id("done"));
    assert_eq!(
        session.identity_named("Done"),
        done.encode(),
        "the box is listed, by what it edits"
    );
    let titled = editing(&session.field_id("title"));
    assert_eq!(
        done.resource, titled.resource,
        "the box names the document the field's own entry names, the same way"
    );
    let published = format!("tl:{}:", session.app);
    // Named as an action on the document is: by the id the app publishes the document under.
    let named =
        |address: &ui::tilo_actions::ActionAddress| address.resource.starts_with(&published);
    assert!(named(&done), "{done:?}");
    assert!(
        session
            .window
            .app()
            .press_window_control(&done.encode(), ui::window_targets::TargetAction::Activate),
        "the window presses the control the identity names"
    );
    session.settle();
    assert_eq!(
        session.field_value("done"),
        Some(tilo_app_window::Value::Bool(true)),
        "the press marked the field the box edits"
    );
    let answered = session.invoke_doing(&done.encode(), &serde_json::json!({}));
    assert!(
        matches!(
            answered,
            ui::tilo_actions::InvocationUpdate::Succeeded { .. }
        ),
        "the API ran the edit the box names: {answered:?}"
    );
    assert_eq!(
        session.field_value("done"),
        Some(tilo_app_window::Value::Bool(false)),
        "and its press marked the field the other way"
    );

    // A task's row holds a box bound to a field of its own, with no action either.
    let add = session.identity_doing("Page", "add");
    let answered = session.invoke_doing(&add, &serde_json::json!({ "title": "Water the plants" }));
    assert!(
        matches!(
            answered,
            ui::tilo_actions::InvocationUpdate::Succeeded { .. }
        ),
        "the API added a task: {answered:?}"
    );
    let boxed = session
        .controls()
        .into_iter()
        .find(|control| control.name == "Mark Water the plants done")
        .expect("the row shows its box");
    let boxed = boxed
        .doing
        .expect("the row's box says which field it edits");
    let address = ui::tilo_actions::ActionAddress::parse(&boxed)
        .unwrap_or_else(|| panic!("{boxed} names a document and a field"));
    assert_eq!(address.action, session.field_id("checked"));
    assert!(named(&address), "{address:?}");
    assert!(
        session
            .window
            .app()
            .press_window_control(&boxed, ui::window_targets::TargetAction::Activate),
        "the window presses the row's box by what it edits"
    );
    session.settle();
    assert_eq!(
        session.field_value("checked"),
        Some(tilo_app_window::Value::Bool(true)),
        "the press marked the task's field"
    );
}

impl crate::change_marks::Driven for super::Session {
    fn harness(&mut self) -> &mut ui::headless::FrameHarness {
        self.window.harness()
    }

    fn settle(&mut self) {
        super::Session::settle(self);
    }
}

impl super::Session {
    /// Invoke `action` of the published `resource` at the API the broker delivers to, as
    /// `invoker` (the person at the window when `None`), within `ambient`: the task the invoker
    /// carries out, recorded with every change it makes. The broker names the invoker it admitted
    /// in the delivery's context. Answers what the operation answered.
    pub(crate) fn invoke(
        &mut self,
        invoker: Option<ui::tilo_actions::ActionInvoker>,
        ambient: Option<&ui::tilo_actions::ActionContext>,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ui::tilo_actions::ActionOutput, String> {
        let acting = invoker.as_ref().map_or_else(
            || ui::tilo_actions::Acting::person(self.person.clone()),
            |invoker: &ui::tilo_actions::ActionInvoker| invoker.acting.clone(),
        );
        let actor = acting.by.clone();
        let context = match invoker {
            Some(invoker) => Some(ambient.cloned().unwrap_or_default().with_invoker(invoker)),
            None => ambient.cloned(),
        };
        let arguments = format!(
            "{}{}",
            ui::tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::Value::Object(arguments.clone())
        );
        let answered = self.deliver(
            &actor,
            resource.to_owned(),
            action.to_owned(),
            Some(arguments),
            context,
        );
        match answered {
            Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) => {
                self.show_write(&acting, resource, action, &output);
                Ok(output)
            }
            Some(ui::tilo_actions::InvocationUpdate::Failed { error, .. }) => Err(error.message),
            Some(other) => Err(format!("{other:?}")),
            None => Err(format!("the API answered nothing of `{action}`")),
        }
    }

    /// Show the characters `action`'s answer says it wrote where they landed, as the broker's
    /// step for an agent at work does: the write's colour on the new characters and the writer's
    /// caret at their end, under way.
    fn show_write(
        &mut self,
        acting: &ui::tilo_actions::Acting,
        resource: &str,
        action: &str,
        output: &ui::tilo_actions::ActionOutput,
    ) {
        let Some(mut wrote) = output.wrote() else {
            return;
        };
        // A window names an item's text by the resource it publishes it under
        // (`tl:app:document/item`); the anchor names the item's own id.
        if let ui::tilo_actions::InteractionAnchor::Text { ranges, .. } = &mut wrote {
            for range in ranges {
                if !range.block.is_empty() && resource.ends_with(&range.block) {
                    range.block.clone_from(&resource.to_owned());
                }
            }
        }
        let super::Window::Harness(window) = &mut self.window else {
            return;
        };
        let resources = window.resources();
        let descriptor = ui::tilo_actions::api::survey(&resources)
            .into_iter()
            .find(|descriptor| {
                descriptor.address.resource == resource && descriptor.address.action == action
            });
        let event = ui::tilo_actions::SemanticInteraction {
            version: ui::tilo_actions::INTERACTION_VERSION,
            invocation: ui::tilo_actions::InvocationId::new(),
            sequence: 1,
            actor: acting.clone(),
            phase: ui::tilo_actions::InteractionPhase::Applying,
            action: ui::tilo_actions::InteractionAction {
                id: action.to_owned(),
                label: descriptor.as_ref().map_or_else(
                    || action.to_owned(),
                    |descriptor| descriptor.action_label.clone(),
                ),
                app: None,
                step: None,
                creates: None,
                did: None,
            },
            resource: ui::tilo_actions::ActionResource {
                id: resource.to_owned(),
                kind: descriptor
                    .as_ref()
                    .map_or_else(String::new, |descriptor| descriptor.resource_kind.clone()),
                label: descriptor.map_or_else(String::new, |descriptor| descriptor.resource_label),
            },
            anchor: wrote,
            group: None,
        };
        let _ =
            window.update_presence(|presence| presence.apply_interaction(event).unwrap_or(false));
    }

    /// Deliver `action` of `resource` at the API the broker delivers to, run by `actor` within
    /// `context`, and let the host answer it: what the API last said of it.
    pub(super) fn deliver(
        &mut self,
        actor: &ui::tilo_actions::Actor,
        resource: String,
        action: String,
        arguments: Option<String>,
        context: Option<ui::tilo_actions::ActionContext>,
    ) -> Option<ui::tilo_actions::InvocationUpdate> {
        let id = self.invoked;
        self.invoked += 1;
        self.inbox()
            .borrow_mut()
            .push_back(CollabEvent::Invocation {
                id,
                invocation: ui::tilo_actions::InvocationId::new(),
                resource,
                action,
                argument: arguments,
                actor: actor.clone(),
                context: context.map(Box::new),
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
    }
}

/// What the person may do in the app, as the session hands it with the launch and says again when
/// it changes: a viewer's window offers no change as if they could make it, and the instance
/// refuses one before it reaches the workspace.
mod access {
    use collaboration::RouteSpec;
    use tilolang_process::hosted::Spec;
    use ui::permissions::policy::{MemberAccess, Role, WorkspacePolicy};

    use super::super::{Session, TASKS, Window, find, task_in_workspace};

    /// Ada's access in `workspace`, whose owner is Bob, as `role`.
    fn ada_as(workspace: &str, role: Role) -> MemberAccess {
        let mut policy = WorkspacePolicy::new(workspace, "bob");
        policy.members.insert("ada".to_owned(), role);
        MemberAccess::person(policy, "ada")
    }

    /// Ada at a Tasks window of Bob's workspace, as `role`.
    fn tasks_as(role: Role) -> (Session, String) {
        let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
        let workspace = route.cluster_id.to_string();
        let spec = Spec::new(TASKS, workspace.clone(), 1, "ada")
            .in_language("en-US")
            .access(ada_as(&workspace, role));
        let mut session = Session::brokered_as(&route, spec, Window::in_harness);
        session.settle();
        (session, workspace)
    }

    /// The window bar's New task button, as the window draws it.
    fn new_task_button(session: &mut Session) -> tilo_app_window::UiNode {
        let view = session.hosted().view().expect("the window is connected");
        let tree = view.root_tree().expect("the window has a tree");
        find(&tree, &|node| {
            node.kind == "ui.Button"
                && node.props.get("label") == Some(&tilo_app_window::Value::text("New task"))
        })
        .expect("the bar holds New task")
        .clone()
    }

    /// Ada views Bob's workspace: New task stands disabled and says why, and invoked anyway it makes
    /// nothing. Bob makes her an editor while her window is open: New task is offered at once, and
    /// it makes the task.
    #[test]
    fn a_viewer_is_offered_no_change_and_one_made_an_editor_is_while_the_window_is_open() {
        let _settled = ui::motion::testing::settled();
        let (mut session, workspace) = tasks_as(Role::Viewer);
        let button = new_task_button(&mut session);
        assert_eq!(
            button.props.get("enabled"),
            Some(&tilo_app_window::Value::Int(0)),
            "a viewer's New task is disabled: {button:?}"
        );
        assert_eq!(
            button.props.get("description"),
            Some(&tilo_app_window::Value::text(
                "That change is not allowed here."
            )),
            "and it says why"
        );
        let new_task = session.identity_doing("Tasks", "new_task");
        let answered = session.invoke_doing(
            &new_task,
            &serde_json::json!({ "title": "Water the plants" }),
        );
        assert!(
            !matches!(
                answered,
                ui::tilo_actions::InvocationUpdate::Succeeded { .. }
            ),
            "the instance refuses a viewer's change: {answered:?}"
        );
        assert!(
            !task_in_workspace(&session, "Water the plants", false),
            "and nothing is written"
        );

        // Bob makes Ada an editor; the session says so on the host's api.
        assert!(
            session
                .host
                .set_access(TASKS, Some(&ada_as(&workspace, Role::Editor)))
        );
        session.settle();
        let button = new_task_button(&mut session);
        assert_ne!(
            button.props.get("enabled"),
            Some(&tilo_app_window::Value::Int(0)),
            "an editor's New task is offered in the window already open: {button:?}"
        );
        let answered = session.invoke_doing(
            &new_task,
            &serde_json::json!({ "title": "Water the plants" }),
        );
        assert!(
            matches!(
                answered,
                ui::tilo_actions::InvocationUpdate::Succeeded { .. }
            ),
            "an editor's change runs: {answered:?}"
        );
        assert!(task_in_workspace(&session, "Water the plants", false));
        assert!(
            !session
                .host
                .set_access(TASKS, Some(&ada_as(&workspace, Role::Editor))),
            "the same access said again is nothing to do"
        );
    }
}
