//! The window over a room record, driven the way the framework drives a surface, with the door
//! played by the test: what the window hands over is built, applied to the record and answered.

use serde_json::Value as Json;
use tilolang_tilo::action::register_actions;
use tilolang_tilo::runtime::MemorySink;
use ui::automerge::AutoCommit;
use ui::tilo_actions::{
    ActionContext, ActionInvoker, ActionRegistry, EncodedAction, apply_local_with_context,
};
use ui::widget::controls::ActionControls as _;

use super::*;

const HOST: &str = "tl:host";

struct Room {
    doc: AutoCommit,
    window: Window,
    sink: MemorySink,
}

/// The delegate of an agent's task, acting for the person at the window.
fn task() -> ActionInvoker {
    ActionInvoker::actor("agent-task:7")
        .with_kind(ui::tilo_actions::ActorKind::Agent)
        .acting_for("ada")
}

impl Room {
    /// The window of `app` over an empty room.
    fn new(app: &str) -> Self {
        let meta = crate::app::package_named(app).expect("a built-in package");
        let sink = MemorySink::new();
        let window =
            Window::new(meta.app.clone(), "ada".to_owned(), RunMode::Live).with_sink(sink.clone());
        let mut room = Self {
            doc: AutoCommit::new(),
            window,
            sink,
        };
        room.settle();
        room
    }

    /// The window of `app` with its scenario `scenario` staged, as a capture stages it.
    fn staged(app: &str, scenario: &str) -> Self {
        let mut room = Self::new(app);
        room.window.stage(Some(scenario));
        room.settle();
        room
    }

    /// The window of `app` with its first scenario staged.
    fn seeded(app: &str) -> Self {
        let scenario = crate::app::package_named(app)
            .and_then(|meta| meta.scenarios.first())
            .map(|scenario| scenario.name.clone())
            .expect("the package declares a scenario");
        Self::staged(app, &scenario)
    }

    /// Fill the room with the first scenario of another package too.
    fn seed(&mut self, app: &str) {
        let host = self.window.current_host().expect("the host started");
        let meta = crate::app::package_named(app).expect("a built-in package");
        let scenario = meta
            .scenarios
            .first()
            .expect("the package declares a scenario");
        host.runtime()
            .invoke_action(
                &Principal::new("ada"),
                (scenario.action.as_str(), Vec::new()),
            )
            .unwrap();
        self.window.commit_planned();
        self.settle();
    }

    fn frame(&mut self) {
        let root = ui::automerge::ROOT;
        self.window.sync(&self.doc, &root);
        self.window.update(&self.doc, &root);
    }

    fn resources(&self) -> Vec<ui::tilo_actions::Resource> {
        crate::resource::published(&self.window.current_host().expect("the host started"))
    }

    /// Hand every pending invocation to the door and apply what it builds.
    fn door(&mut self) {
        while let Some((invocation, origin)) = self.window.take_attributed_control_invocation() {
            let invoker = origin.map_or_else(|| ActionInvoker::actor("ada".to_owned()), |_| task());
            let resources = self.resources();
            let result = (|| -> Result<(), String> {
                let address = ui::tilo_actions::door::published(
                    &resources,
                    &invocation.address.resource,
                    &invocation.address.action,
                )
                .ok_or_else(|| format!("{:?} is not published", invocation.address))?;
                let encoded: EncodedAction = {
                    let _admitted = ui::invocation_scope::InvokerScope::enter(invoker.clone());
                    ui::tilo_actions::door::encode_json(&resources, &address, &invocation.arguments)
                        .map_err(|error| error.to_string())?
                };
                let action = ActionRegistry::with(register_actions)
                    .decode(&encoded)
                    .map_err(|error| error.to_string())?;
                let context = ActionContext::for_resource("room", "tl.room", "room")
                    .with_invoker(invoker.clone());
                apply_local_with_context(
                    &mut self.doc,
                    action.as_ref(),
                    encoded,
                    Some(invoker.actor.as_bytes()),
                    context,
                )
                .map_err(|error| error.to_string())?;
                self.doc.commit();
                Ok(())
            })();
            self.window.answered(&invocation.address, result.err());
        }
    }

    /// Frames and doors until the window has nothing more to hand over.
    fn settle(&mut self) {
        for _ in 0..4 {
            self.frame();
            self.door();
        }
        self.frame();
    }

    fn surveyed(&self) -> Vec<ui::tilo_actions::door::ActionDescriptor> {
        ui::tilo_actions::door::survey(&self.resources())
    }

    fn agent_invokes(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Json,
    ) -> Result<ui::tilo_actions::ActionOutput, String> {
        self.invokes_as(Some(task()), resource, action, arguments)
    }

    /// Invoke an effect of the door as `who` (the person at the window when `None`).
    fn invokes_as(
        &mut self,
        who: Option<ActionInvoker>,
        resource: &str,
        action: &str,
        arguments: &Json,
    ) -> Result<ui::tilo_actions::ActionOutput, String> {
        use ui::framework::{Effected, HostDelivery as _};
        let descriptor = self
            .surveyed()
            .into_iter()
            .find(|d| d.address.resource == resource && d.address.action == action)
            .ok_or_else(|| format!("{resource}/{action} is not published"))?;
        let encoded = ui::tilo_actions::door::encode_json(
            &self.resources(),
            &descriptor.address,
            &arguments.as_object().cloned().unwrap_or_default(),
        )
        .map_err(|error| error.to_string())?;
        let started = {
            let _admitted = who.map(ui::invocation_scope::InvokerScope::enter);
            self.window.start_encoded(
                descriptor.kind,
                &encoded.type_id,
                &encoded.payload,
                &mut self.doc,
                &ui::automerge::ROOT,
            )?
        }
        .ok_or_else(|| "nothing answered the operation".to_owned())?;
        self.settle();
        match started {
            Effected::Done(output) => Ok(output),
            Effected::Pending => Err("the operation is still running".to_owned()),
        }
    }

    /// Apply a published verb as the door does for `who`.
    fn applies_as(
        &mut self,
        who: Option<ActionInvoker>,
        resource: &str,
        action: &str,
        arguments: &Json,
    ) -> Result<(), String> {
        let resources = self.resources();
        let address = ui::tilo_actions::door::published(&resources, resource, action)
            .ok_or_else(|| format!("{resource}/{action} is not published"))?;
        let invoker = who
            .clone()
            .unwrap_or_else(|| ActionInvoker::actor("ada".to_owned()));
        let encoded = {
            let _admitted = who.map(ui::invocation_scope::InvokerScope::enter);
            ui::tilo_actions::door::encode_json(
                &resources,
                &address,
                &arguments.as_object().cloned().unwrap_or_default(),
            )
            .map_err(|error| error.to_string())?
        };
        let action = ActionRegistry::with(register_actions)
            .decode(&encoded)
            .map_err(|error| error.to_string())?;
        let context =
            ActionContext::for_resource(resource, "tl", resource).with_invoker(invoker.clone());
        apply_local_with_context(
            &mut self.doc,
            action.as_ref(),
            encoded,
            Some(invoker.actor.as_bytes()),
            context,
        )
        .map_err(|error| error.to_string())?;
        self.doc.commit();
        self.settle();
        Ok(())
    }

    fn shell(&self) -> &NavShell {
        self.window.shell_ref().expect("the window shows the shell")
    }

    fn list(&mut self) -> &mut ListPane {
        let shell = self.window.shell().expect("the window shows the shell");
        if shell.overview_mut::<ListPane>().is_some() {
            return shell.overview_mut::<ListPane>().unwrap();
        }
        shell.content_mut::<ListPane>().expect("a list column")
    }

    /// The keys and titles of the rows the list shows.
    fn rows(&mut self) -> Vec<(String, String)> {
        self.list()
            .rows
            .iter()
            .map(|row| {
                let title = row
                    .row
                    .as_ref()
                    .and_then(|node| node.props.get("title"))
                    .map(crate::reconcile::display)
                    .unwrap_or_else(|| row.label.clone());
                (row.key.clone(), title)
            })
            .collect()
    }

    fn row_key(&mut self, title: &str) -> String {
        self.rows()
            .into_iter()
            .find(|(_, shown)| shown == title)
            .map(|(key, _)| key)
            .unwrap_or_else(|| panic!("no row {title}"))
    }

    /// Run what a shell control asks and let the window settle.
    fn ask(&mut self, request: Request) {
        self.window.request(request);
        self.settle();
    }

    /// Press the header action labelled `label` of the root window.
    fn header_action(&mut self, label: &str) {
        let host = self.window.current_host().unwrap();
        let target = self.window.root.as_ref().unwrap().target.clone();
        let action = describe::offered(&host, &target)
            .iter()
            .find(|action| action.label == label)
            .cloned()
            .unwrap_or_else(|| panic!("the header offers no {label}"));
        self.ask(Request::Invoke {
            action,
            title: String::new(),
        });
    }

    fn items_of(&self, app: &str, field: &str) -> usize {
        let host = self.window.current_host().unwrap();
        let root = host.root_of(app).unwrap();
        match host.runtime().read(&root, field).unwrap() {
            Value::List(items) => items.len(),
            other => panic!("not a list: {other:?}"),
        }
    }
}

/// The first node of the tree with `kind` whose prop `name` is `value`.
fn node_with<'a>(node: &'a UiNode, kind: &str, name: &str, value: &str) -> Option<&'a UiNode> {
    if node.kind == kind
        && node
            .props
            .get(name)
            .map(crate::reconcile::display)
            .as_deref()
            == Some(value)
    {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| node_with(child, kind, name, value))
}

fn first_input(node: &UiNode) -> Option<&UiNode> {
    if node.kind == "input" {
        return Some(node);
    }
    node.children.iter().find_map(first_input)
}

// ----- the shell ---------------------------------------------------------------------------

#[test]
fn a_notebook_is_the_native_list_and_detail_shell_with_the_header_the_platform_fills() {
    let mut room = Room::seeded("notes");
    assert!(
        room.shell().has_overview(),
        "the notes list is the overview column"
    );
    assert!(room.shell().has_sidebar(), "the filter is the sidebar");
    let rows = room.rows();
    assert!(rows.len() >= 3, "{rows:?}");
    assert_eq!(
        room.window
            .headers
            .get("overview")
            .map(|header| header.split('\u{1f}').next().unwrap_or_default().to_owned())
            .as_deref(),
        Some("All notes")
    );
    // Nothing is open yet: the content says so in one sentence.
    assert!(room.window.detail.is_none());
    assert!(room.window.headers.contains_key("content-empty"));
}

// USER REQUIREMENT (2026-10-20): `open(r)` "the host minimally implements it as
// push-navigation to the resource's default view with back". It now shows the resource's
// default view in the next column, which a narrow window pushes with Back.
#[test]
fn activating_a_row_shows_the_item_in_the_next_column_and_back_returns_to_the_list() {
    let mut room = Room::seeded("notes");
    let (key, title) = room.rows().remove(0);
    room.ask(Request::Activate(key.clone()));
    let detail = room
        .window
        .detail_tree()
        .expect("the note is shown")
        .clone();
    assert!(
        first_input(&detail).is_some(),
        "the note's editor: {detail:#?}"
    );
    assert_eq!(
        room.list().selected.as_deref(),
        Some(key.as_str()),
        "the row is selected"
    );
    let header = room
        .window
        .headers
        .get("content")
        .cloned()
        .unwrap_or_default();
    assert!(
        header.starts_with(&title),
        "the content's title is the note's: {header}"
    );
    // Editing the title changes the row.
    let input = first_input(&detail).unwrap().key.clone();
    room.window
        .act(true, UiEvent::Edit(input, "Plans for the week".to_owned()));
    room.settle();
    assert!(
        room.rows()
            .iter()
            .any(|(_, shown)| shown == "Plans for the week"),
        "{:?}",
        room.rows()
    );
    room.ask(Request::Back);
    assert!(room.window.detail.is_none() || room.shell().front_pane() != NavPane::Content);
}

#[test]
fn the_primary_action_is_the_header_and_empty_states_action_and_creates_through_the_door() {
    let mut room = Room::new("notes");
    assert_eq!(room.items_of("so.tilo.notes", "notes"), 0);
    // An empty notebook offers its primary action and says one sentence.
    let empty = room.list().empty.clone();
    assert_eq!(empty.sentence, "No notes yet");
    assert_eq!(
        empty.action.as_ref().map(|action| action.label.as_str()),
        Some("New note")
    );
    room.header_action("New note");
    assert_eq!(
        room.items_of("so.tilo.notes", "notes"),
        1,
        "the action inserted a note"
    );
    assert_eq!(room.rows().len(), 1);
    assert!(room.window.detail.is_some(), "and `open` showed it");
}

#[test]
fn a_filter_case_from_the_sidebar_narrows_the_list_and_titles_it() {
    let mut room = Room::seeded("notes");
    let all = room.rows().len();
    let key = room
        .window
        .root
        .as_ref()
        .map(|lane| frame_of(&lane.tree).unwrap().key.clone())
        .unwrap();
    room.ask(Request::Fire {
        key,
        event: "filter.pinned".to_owned(),
    });
    let pinned = room.rows().len();
    assert!(pinned < all, "{pinned} of {all}");
    let header = room
        .window
        .headers
        .get("overview")
        .cloned()
        .unwrap_or_default();
    assert!(header.starts_with("Pinned"), "{header}");
}

#[test]
fn a_destructive_row_action_runs_through_the_door_and_offers_undo() {
    let mut room = Room::seeded("notes");
    let before = room.rows().len();
    let key = room.rows()[0].0.clone();
    let row = room
        .list()
        .rows
        .iter()
        .find(|row| row.key == key)
        .cloned()
        .unwrap();
    let delete = row
        .actions
        .iter()
        .find(|action| action.destructive)
        .cloned()
        .expect("a destructive action on the row");
    assert!(
        row.actions.iter().any(|action| !action.destructive),
        "the row's menu has every action"
    );
    room.ask(Request::Invoke {
        action: delete,
        title: row.label.clone(),
    });
    assert_eq!(room.rows().len(), before - 1);
    assert!(room.shell().toast_text().is_some(), "the shell offers undo");
}

#[test]
fn a_scenario_stages_exactly_its_data_however_many_frames_run() {
    let mut room = Room::seeded("notes");
    let staged = room.items_of("so.tilo.notes", "notes");
    for _ in 0..6 {
        room.settle();
    }
    assert_eq!(room.items_of("so.tilo.notes", "notes"), staged);
    assert_eq!(room.rows().len(), staged);
    let records = tilolang_tilo::action::resource_ids(&room.doc, "so.tilo.notes");
    assert_eq!(records.len(), 1, "one notebook");
}

#[test]
fn every_package_opens_into_one_frame_of_the_native_shell() {
    for built in crate::app::compiled() {
        let meta = &built.compiled.meta;
        if meta
            .roots
            .first()
            .is_none_or(|root| meta.default_view(root).is_none())
        {
            continue;
        }
        let mut room = Room::seeded(built.dir);
        let tree = room.window.root_tree().expect("the root view runs").clone();
        assert!(
            frame_of(&tree).is_some(),
            "{} has a frame: {tree:#?}",
            built.dir
        );
        assert!(
            room.window.shape.is_some(),
            "{} composed the shell",
            built.dir
        );
        room.settle();
    }
}

#[test]
fn the_published_tree_has_the_host_and_every_record() {
    let room = Room::seeded("notes");
    let resources = room.resources();
    let kinds: Vec<&str> = resources.iter().map(|r| r.kind.as_str()).collect();
    assert!(kinds.contains(&"tl.host"), "{kinds:?}");
    assert!(kinds.contains(&"tl.so.tilo.notes.Notebook"), "{kinds:?}");
}

// ----- the door ----------------------------------------------------------------------------

#[test]
fn an_agent_finds_the_hosts_operations_in_the_survey_and_reads_the_catalogue() {
    let mut room = Room::seeded("letters");
    let host_actions: Vec<String> = room
        .surveyed()
        .into_iter()
        .filter(|d| d.address.resource == HOST)
        .map(|d| d.address.action)
        .collect();
    for wanted in ["run-block", "run-flow", "catalogue"] {
        assert!(host_actions.iter().any(|a| a == wanted), "{host_actions:?}");
    }
    let resources = room.resources();
    let violations = ui::tilo_actions::validate_surface(
        &resources,
        &ActionRegistry::with(register_actions),
        &ui::widget::Widget::resource_operation_handlers(&room.window),
    );
    assert!(violations.is_empty(), "{violations:#?}");
    let output = room.agent_invokes(HOST, "catalogue", &Json::Null).unwrap();
    let text = output
        .value
        .expect("the catalogue is the answer")
        .to_string();
    assert!(text.contains("so.tilo.letters"), "{text}");
}

#[test]
fn a_view_block_a_value_block_and_a_bad_block_answer_an_agent() {
    let mut room = Room::seeded("chat");
    let run = |room: &mut Room, source: &str| {
        room.agent_invokes(HOST, "run-block", &serde_json::json!({ "source": source }))
    };
    let view = run(&mut room, "column { text(\"Hello from an agent\") }")
        .unwrap()
        .value
        .unwrap();
    assert_eq!(view["kind"], "view", "{view}");
    let data = run(&mut room, "1 + 2").unwrap().value.unwrap();
    assert_eq!(data, serde_json::json!({"kind": "data", "value": 3}));
    let refused = run(&mut room, "nothing.here(").unwrap_err();
    assert!(refused.contains("does not compile"), "{refused}");
}

#[test]
fn the_verbs_of_items_are_published_on_the_door_and_an_agent_calls_them() {
    let mut room = Room::seeded("notes");
    let survey = room.surveyed();
    let pin = survey
        .iter()
        .find(|d| d.resource_kind == "tl.so.tilo.notes.Note" && d.action_label == "Toggle pin")
        .expect("a note publishes its own verbs");
    let (resource, action) = (pin.address.resource.clone(), pin.address.action.clone());
    assert!(
        resource.contains('/'),
        "an item is addressed by its path: {resource}"
    );
    let pinned_before = room.surveyed().len();
    room.applies_as(Some(task()), &resource, &action, &Json::Null)
        .unwrap();
    assert_eq!(
        room.surveyed().len(),
        pinned_before,
        "the same verbs after the change"
    );
}

#[test]
fn search_entries_find_notes_and_open_the_window_at_the_note() {
    let mut room = Room::seeded("notes");
    let host = room.window.current_host().unwrap();
    let entries = crate::resource::search_entries(&host);
    assert!(!entries.is_empty());
    let first = &entries[0];
    assert_eq!(first.group.as_deref(), Some("Notes"), "{first:?}");
    let anchor = first.anchor.clone().expect("a hit names its resource");
    room.window.open_requests().borrow_mut().push(anchor);
    room.settle();
    assert!(room.window.detail.is_some(), "the hit showed the note");
}
