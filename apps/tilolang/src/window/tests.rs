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
        crate::app::builtin::choose(app);
        let meta = crate::app::builtin::package_named(app).expect("a built-in package");
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
        let scenario = crate::app::builtin::package_named(app)
            .and_then(|meta| meta.scenarios.first())
            .map(|scenario| scenario.name.clone())
            .expect("the package declares a scenario");
        Self::staged(app, &scenario)
    }

    /// Fill the room with the first scenario of another package too.
    fn seed(&mut self, app: &str) {
        let host = self.window.current_host().expect("the host started");
        let meta = crate::app::builtin::package_named(app).expect("a built-in package");
        let scenario = meta
            .scenarios
            .first()
            .expect("the package declares a scenario");
        // A scenario fills its package's root, which a room makes the first time it is wanted.
        host.root_of(&meta.app).expect("the package has a root");
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

    fn resources() -> Vec<ui::tilo_actions::Resource> {
        crate::resource::published()
    }

    /// Hand every pending invocation to the door and apply what it builds.
    fn door(&mut self) {
        while let Some((invocation, origin)) = self.window.take_attributed_control_invocation() {
            let invoker = origin.map_or_else(|| ActionInvoker::actor("ada".to_owned()), |_| task());
            let resources = Self::resources();
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

    fn surveyed() -> Vec<ui::tilo_actions::door::ActionDescriptor> {
        ui::tilo_actions::door::survey(&Self::resources())
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
        let descriptor = Self::surveyed()
            .into_iter()
            .find(|d| d.address.resource == resource && d.address.action == action)
            .ok_or_else(|| format!("{resource}/{action} is not published"))?;
        let encoded = ui::tilo_actions::door::encode_json(
            &Self::resources(),
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
        let resources = Self::resources();
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

    /// The rows of the list, every one shown, as a list tall enough for all of them shows them.
    fn shown(&mut self) -> Vec<RowData> {
        let list = self.list();
        list.show_rows(0..list.len());
        list.shown()
    }

    /// The keys and titles of the rows the list shows.
    fn rows(&mut self) -> Vec<(String, String)> {
        self.shown()
            .iter()
            .map(|row| {
                let title = row
                    .row
                    .as_ref()
                    .and_then(|node| node.props.get("title"))
                    .map_or_else(|| row.label.clone(), crate::reconcile::display);
                (row.key.clone(), title)
            })
            .collect()
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

    /// Whether the note published as `resource` is pinned.
    fn pinned(&self, resource: &str) -> bool {
        let host = self.window.current_host().unwrap();
        let path = resource.strip_prefix("tl:so.tilo.lang.notes:").unwrap();
        let probe = tilolang_tilo::runtime::ResourceRef::parse_id(path, String::new()).unwrap();
        let ty = tilolang_tilo::runtime::store::Store::type_of(host.store(), &probe).unwrap();
        let note = tilolang_tilo::runtime::ResourceRef::parse_id(path, ty).unwrap();
        host.runtime().read(&note, "pinned").unwrap() == Value::Bool(true)
    }

    /// The states of the invocations the room record holds.
    fn invocation_states(&self) -> Vec<String> {
        let host = self.window.current_host().unwrap();
        tilolang_tilo::action::resource_ids(&self.doc, tilolang_tilo::HOST_APP)
            .iter()
            .filter_map(|doc| host.invocation_ref(doc))
            .filter_map(|invocation| host.runtime().read_invocation(&invocation).ok())
            .map(|record| record.state)
            .collect()
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
    // The scenario showed its note; with that closed, the content says so in one sentence.
    assert!(room.window.detail.is_some());
    room.ask(Request::Back);
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
    let detail = room.window.detail_tree().expect("the note is shown");
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
        header.starts_with('\u{1f}'),
        "the note shows {title} as its own title, which the header does not repeat: {header}"
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
    assert_eq!(room.items_of("so.tilo.lang.notes", "notes"), 0);
    // An empty notebook offers its primary action and says one sentence.
    let empty = room.list().empty.clone();
    assert_eq!(empty.sentence, "No notes yet");
    assert_eq!(
        empty.action.as_ref().map(|action| action.label.as_str()),
        Some("New note")
    );
    room.header_action("New note");
    assert_eq!(
        room.items_of("so.tilo.lang.notes", "notes"),
        1,
        "the action inserted a note"
    );
    assert_eq!(room.rows().len(), 1);
    let made = room.shown()[0].item.clone();
    assert_eq!(
        room.window.detail_target(),
        Some(&made),
        "and the note it made opened beside the list"
    );
}

/// The published id and action id of the notebook's "New note".
fn new_note_on_the_door() -> (String, String) {
    let descriptor = Room::surveyed()
        .into_iter()
        .find(|d| {
            d.resource_kind == "tl.so.tilo.lang.notes.Notebook" && d.action_label == "New note"
        })
        .expect("the notebook publishes New note");
    (descriptor.address.resource, descriptor.address.action)
}

#[test]
fn what_an_agent_or_a_script_creates_through_the_door_moves_nobodys_view() {
    let mut room = Room::new("notes");
    let (notebook, new_note) = new_note_on_the_door();
    // An agent's task makes a note: the window stays where the person left it.
    room.applies_as(Some(task()), &notebook, &new_note, &Json::Null)
        .unwrap();
    assert_eq!(room.items_of("so.tilo.lang.notes", "notes"), 1);
    assert_eq!(
        room.window.detail_target(),
        None,
        "the agent's note did not open"
    );
    // So does a script on the control socket, which acts as the person but not at the window.
    let script = ActionInvoker::actor("ada".to_owned());
    room.applies_as(Some(script), &notebook, &new_note, &Json::Null)
        .unwrap();
    assert_eq!(room.items_of("so.tilo.lang.notes", "notes"), 2);
    assert_eq!(
        room.window.detail_target(),
        None,
        "the script's note did not open"
    );
    // The person pressing New note in the window opens what it made, whatever was made before.
    room.header_action("New note");
    assert_eq!(room.items_of("so.tilo.lang.notes", "notes"), 3);
    let opened = room
        .window
        .detail_target()
        .cloned()
        .expect("the person's note opened");
    let theirs = room
        .shown()
        .into_iter()
        .map(|row| row.item)
        .filter(|item| *item == opened)
        .count();
    assert_eq!(theirs, 1, "it is a note of the list");
    // And an agent's note after that leaves the person's open.
    room.applies_as(Some(task()), &notebook, &new_note, &Json::Null)
        .unwrap();
    assert_eq!(room.window.detail_target(), Some(&opened));
}

#[test]
fn a_new_conversation_shows_the_groups_avatar_until_someone_writes() {
    let mut room = Room::new("chat");
    room.header_action("New chat");
    let rows = room.shown();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].avatar,
        Some(list::RowAvatar::Glyph(
            "New chat".to_owned(),
            ui::icons::Icon::Users
        )),
        "a conversation always has an avatar: before a message, its group's"
    );
    let seeded = Room::seeded("chat").shown();
    assert!(
        seeded
            .iter()
            .filter(|row| row
                .row
                .as_ref()
                .and_then(|node| node.props.get("secondary"))
                != Some(&Value::Text("No messages yet".to_owned())))
            .all(|row| matches!(row.avatar, Some(list::RowAvatar::Of(..)))),
        "after one, its last author's"
    );
}

#[test]
fn a_filter_case_from_the_sidebar_narrows_the_list_and_titles_it() {
    let mut room = Room::seeded("notes");
    let all = room.rows().len();
    let key = room
        .window
        .root
        .as_ref()
        .and_then(|lane| lane.list.clone())
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
    let row = room.shown().into_iter().find(|row| row.key == key).unwrap();
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
        title: row.label,
    });
    assert_eq!(room.rows().len(), before - 1);
    assert!(room.shell().toast_text().is_some(), "the shell offers undo");
}

#[test]
fn a_scenario_stages_exactly_its_data_however_many_frames_run() {
    let mut room = Room::seeded("notes");
    let staged = room.items_of("so.tilo.lang.notes", "notes");
    for _ in 0..6 {
        room.settle();
    }
    assert_eq!(room.items_of("so.tilo.lang.notes", "notes"), staged);
    assert_eq!(room.rows().len(), staged);
    let records = tilolang_tilo::action::resource_ids(&room.doc, "so.tilo.lang.notes");
    assert_eq!(records.len(), 1, "one notebook");
}

#[test]
fn every_package_opens_into_one_frame_of_the_native_shell() {
    for built in crate::app::builtin::compiled() {
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
    // The live window publishes its room while it lives.
    let _room = Room::seeded("notes");
    let resources = Room::resources();
    let kinds: Vec<&str> = resources.iter().map(|r| r.kind.as_str()).collect();
    assert!(kinds.contains(&"tl.host"), "{kinds:?}");
    assert!(
        kinds.contains(&"tl.so.tilo.lang.notes.Notebook"),
        "{kinds:?}"
    );
}

// ----- the door ----------------------------------------------------------------------------

#[test]
fn an_agent_finds_the_hosts_operations_in_the_survey_and_reads_the_catalogue() {
    let mut room = Room::seeded("letters");
    let host_actions: Vec<String> = Room::surveyed()
        .into_iter()
        .filter(|d| d.address.resource == HOST)
        .map(|d| d.address.action)
        .collect();
    for wanted in ["run-block", "run-flow", "catalogue"] {
        assert!(host_actions.iter().any(|a| a == wanted), "{host_actions:?}");
    }
    let resources = Room::resources();
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
    assert!(text.contains("so.tilo.lang.letters"), "{text}");
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
    let survey = Room::surveyed();
    let pin = survey
        .iter()
        .find(|d| d.resource_kind == "tl.so.tilo.lang.notes.Note" && d.action_label == "Pin")
        .expect("a note publishes its own verbs");
    let (resource, action) = (pin.address.resource.clone(), pin.address.action.clone());
    assert!(
        resource.contains('/'),
        "an item is addressed by its path: {resource}"
    );
    let before = room.pinned(&resource);
    room.applies_as(Some(task()), &resource, &action, &Json::Null)
        .unwrap();
    assert_eq!(room.pinned(&resource), !before, "the agent pinned the note");
    // What the agent passes wins over the default, which would flip the note back.
    let pinned = serde_json::json!({ "pinned": !before });
    room.applies_as(Some(task()), &resource, &action, &pinned)
        .unwrap();
    assert_eq!(
        room.pinned(&resource),
        !before,
        "an explicit `pinned:` is the outcome"
    );

    // A draft's send reaches outside: asked of an agent, it is recorded as a suggestion.
    let mut desk = Room::seeded("letters");
    let send = Room::surveyed()
        .into_iter()
        .find(|d| d.resource_kind == "tl.so.tilo.lang.letters.Draft" && d.action_label == "Send")
        .expect("a draft publishes its send");
    desk.applies_as(
        Some(task()),
        &send.address.resource,
        &send.address.action,
        &Json::Null,
    )
    .unwrap();
    assert!(
        desk.invocation_states()
            .iter()
            .any(|state| state == "suggested"),
        "{:?}",
        desk.invocation_states()
    );
    assert!(desk.sink.delivered().is_empty(), "nothing was sent");
}

/// The first draft on the desk of a letters room.
fn first_draft(room: &Room) -> tilolang_tilo::runtime::ResourceRef {
    let host = room.window.current_host().expect("the host started");
    let desk = host.root_of("so.tilo.lang.letters").unwrap();
    let Value::List(drafts) = host.runtime().read(&desk, "drafts").unwrap() else {
        panic!("the desk lists its drafts");
    };
    let Some(Value::Ref(draft)) = drafts.first() else {
        panic!("a draft is a reference");
    };
    ResourceRef::clone(&draft.target)
}

#[test]
fn a_send_block_from_an_agent_becomes_a_suggested_invocation_a_person_decides() {
    let mut room = Room::new("chat");
    room.seed("letters");
    let host = room.window.current_host().unwrap();
    let source = format!(
        "letters.send(@letters.Draft:{})",
        first_draft(&room).id_string()
    );
    let output = room
        .agent_invokes(HOST, "run-block", &serde_json::json!({ "source": source }))
        .unwrap()
        .value
        .unwrap();
    assert_eq!(output["kind"], "invocation", "{output}");
    assert_eq!(
        output["state"], "suggested",
        "an agent's send asks first: {output}"
    );
    // The invocation is a resource of the room record, so every replica shows it.
    let id = output["invocation"].as_str().unwrap();
    let invocation = host.invocation_ref(id).unwrap();
    let in_record = tilolang_tilo::action::resource_ids(&room.doc, tilolang_tilo::HOST_APP);
    assert!(
        in_record.iter().any(|doc| doc == &invocation.doc),
        "{in_record:?}"
    );
    // A person declines it, and nothing is sent.
    host.runtime()
        .resolve_invocation(
            &invocation,
            &Principal::new("ada"),
            tilolang_tilo::runtime::Resolution::Decline,
        )
        .unwrap();
    assert_eq!(
        host.runtime().read_invocation(&invocation).unwrap().state,
        "declined"
    );
    assert!(room.sink.delivered().is_empty());
}

fn person_decides(room: &mut Room, decision: &str, target: &str) -> Result<Json, String> {
    room.invokes_as(
        None,
        HOST,
        "resolve",
        &serde_json::json!({"target": target, "decision": decision}),
    )
    .map(|output| output.value.unwrap_or_default())
}

#[test]
fn approving_a_pressed_send_through_the_ask_bar_delivers_the_letter_once() {
    let mut room = Room::seeded("letters");
    let key = room.rows()[0].0.clone();
    room.ask(Request::Activate(key));
    let draft = room
        .window
        .detail_target()
        .cloned()
        .expect("the letter opened");
    let host = room.window.current_host().unwrap();
    let to = crate::reconcile::display(&host.runtime().read(&draft, "to").unwrap());
    let send = node_with(
        &room.window.detail_tree().unwrap(),
        "button",
        "label",
        "Send",
    )
    .expect("the letter's Send button")
    .key
    .clone();
    room.window.act(true, UiEvent::Press(send));
    room.settle();
    assert!(room.window.ask.is_some(), "a send asks the person first");
    assert!(
        room.sink.delivered().is_empty(),
        "nothing leaves before they say so"
    );
    room.window.answers.borrow_mut().push(Answer::Allow);
    room.settle();
    let delivered = room.sink.delivered();
    assert_eq!(delivered.len(), 1, "{delivered:?}");
    assert_eq!(delivered[0].0, "post");
    assert!(delivered[0].1.to_string().contains(&to), "{delivered:?}");
    room.settle();
    room.settle();
    assert_eq!(room.sink.delivered().len(), 1, "settled once, not again");
    // The record says it was delivered, so no device queues it again.
    assert_eq!(
        tilolang_tilo::runtime::store::Store::outbox_pending(host.store()).len(),
        0
    );
}

#[test]
fn a_person_approves_an_agents_suggested_send_through_the_door_and_it_leaves_once() {
    let mut room = Room::new("chat");
    room.seed("letters");
    let host = room.window.current_host().unwrap();
    let draft = first_draft(&room);
    let to = crate::reconcile::display(&host.runtime().read(&draft, "to").unwrap());
    let source = format!("letters.send(@letters.Draft:{})", draft.id_string());
    let suggested = room
        .agent_invokes(HOST, "run-block", &serde_json::json!({ "source": source }))
        .unwrap()
        .value
        .unwrap();
    assert_eq!(suggested["state"], "suggested", "{suggested}");
    let id = suggested["invocation"].as_str().unwrap().to_owned();
    // The runtime knows the author as the task's delegate.
    let record = host
        .runtime()
        .read_invocation(&host.invocation_ref(&id).unwrap())
        .unwrap();
    assert_eq!(record.author, Principal::new("agent-task:7"));
    // An agent cannot approve what it suggested.
    let refused = room.invokes_as(
        Some(task()),
        HOST,
        "resolve",
        &serde_json::json!({"target": id, "decision": "approve"}),
    );
    assert!(
        refused.unwrap_err().contains("only a person"),
        "agents never decide"
    );
    assert!(room.sink.delivered().is_empty());
    let decided = person_decides(&mut room, "approve", &id).unwrap();
    assert_eq!(decided["state"], "done", "{decided}");
    let delivered = room.sink.delivered();
    assert_eq!(delivered.len(), 1, "{delivered:?}");
    assert!(delivered[0].1.to_string().contains(&to), "{delivered:?}");
    // Approving again changes nothing: the suggestion is decided.
    assert!(person_decides(&mut room, "approve", &id).is_err());
    room.settle();
    assert_eq!(room.sink.delivered().len(), 1);
}

/// A board room with the todo list and the notebook it points at, each with its scenario, and
/// how many of the todos are done.
fn board_room() -> (Room, usize) {
    let mut room = Room::new("board");
    room.seed("todo");
    room.seed("notes");
    let host = room.window.current_host().unwrap();
    let list = host.root_of("so.tilo.lang.todo").unwrap();
    let Value::List(items) = host.runtime().read(&list, "items").unwrap() else {
        panic!("the list holds items");
    };
    let done = items
        .iter()
        .filter(|item| match item {
            Value::Ref(item) => {
                host.runtime().read(&item.target, "done").ok() == Some(Value::Bool(true))
            }
            _ => false,
        })
        .count();
    assert!(done > 0, "the saturday list has done items");
    (room, done)
}

fn cleanup_args(room: &Room) -> Json {
    let host = room.window.current_host().unwrap();
    let list = host.root_of("so.tilo.lang.todo").unwrap().id_string();
    let notebook = host.root_of("so.tilo.lang.notes").unwrap().id_string();
    serde_json::json!({
        "flow": "so.tilo.lang.board.morning_cleanup",
        "args": serde_json::json!({"list": list, "nb": notebook}).to_string(),
    })
}

#[test]
fn an_agent_runs_a_flow_of_a_trusted_package_and_reads_what_it_returned() {
    let (mut room, done) = board_room();
    let args = cleanup_args(&room);
    let output = room
        .agent_invokes(HOST, "run-flow", &args)
        .unwrap()
        .value
        .unwrap();
    assert_eq!(output, serde_json::json!({"state": "done", "result": done}));
}

#[test]
fn a_flow_an_agent_may_not_run_alone_waits_for_a_person_and_completes_once() {
    let (mut room, done) = board_room();
    let host = room.window.current_host().unwrap();
    // The person narrowed this agent: changes to the todo app ask first.
    host.set_agent_answer(
        "so.tilo.lang.todo",
        tilolang_tilo::runtime::meta::EffectClass::Undoable,
        Decision::Ask,
    );
    let todos = room.items_of("so.tilo.lang.todo", "items");
    let notes = room.items_of("so.tilo.lang.notes", "notes");
    let args = cleanup_args(&room);
    let waiting = room
        .agent_invokes(HOST, "run-flow", &args)
        .unwrap()
        .value
        .unwrap();
    assert_eq!(waiting["state"], "suspended", "{waiting}");
    assert!(
        room.window.ask.is_some(),
        "the window asks the person, as for a press"
    );
    assert_eq!(
        room.items_of("so.tilo.lang.todo", "items"),
        todos,
        "nothing went while it waits"
    );
    let run = waiting["run"].as_str().unwrap().to_owned();
    // Each removal asks: the flow waits again until the person approved every one.
    let mut approvals = 0;
    let finished = loop {
        let answer = person_decides(&mut room, "approve", &run).unwrap();
        approvals += 1;
        if answer["state"] != "suspended" || approvals > done {
            break answer;
        }
    };
    assert_eq!(finished["state"], "done", "{finished}");
    assert_eq!(approvals, done, "one approval per removal");
    assert_eq!(
        room.items_of("so.tilo.lang.todo", "items"),
        todos - done,
        "the done ones went, once"
    );
    assert_eq!(
        room.items_of("so.tilo.lang.notes", "notes"),
        notes + 1,
        "and one note says so"
    );
    assert!(
        person_decides(&mut room, "approve", &run).is_err(),
        "it completes once"
    );
    room.settle();
    assert_eq!(room.items_of("so.tilo.lang.todo", "items"), todos - done);
    assert_eq!(room.items_of("so.tilo.lang.notes", "notes"), notes + 1);
}

#[test]
fn search_finds_chat_messages_and_a_hit_opens_their_conversation() {
    let mut room = Room::seeded("chat");
    let entries = crate::resource::search_entries();
    let hit = entries
        .iter()
        .find(|entry| entry.title.contains("meet at the station"))
        .unwrap_or_else(|| panic!("a message is found: {entries:#?}"));
    // The persistent index keeps short fields, never a prose body.
    assert!(
        entries
            .iter()
            .flat_map(|entry| &entry.fields)
            .all(|(text, _)| text.chars().count() <= 80)
    );
    let anchor = hit.anchor.clone().expect("a hit names its resource");
    room.window.open_requests().borrow_mut().push(anchor);
    room.settle();
    let thread = room
        .window
        .detail_tree()
        .expect("the hit opened its conversation");
    assert!(
        format!("{thread:?}").contains("Let's meet at the station at ten."),
        "the conversation shows the message: {thread:#?}"
    );
}

#[test]
fn search_entries_find_notes_and_open_the_window_at_the_note() {
    let mut room = Room::seeded("notes");
    let entries = crate::resource::search_entries();
    assert!(!entries.is_empty());
    let first = &entries[0];
    assert_eq!(first.group.as_deref(), Some("Notes"), "{first:?}");
    let anchor = first.anchor.clone().expect("a hit names its resource");
    room.window.open_requests().borrow_mut().push(anchor);
    room.settle();
    assert!(room.window.detail.is_some(), "the hit showed the note");
}

#[test]
fn an_action_the_content_shows_as_a_button_is_not_repeated_in_the_header() {
    let room = Room::seeded("counter");
    let host = room.window.current_host().unwrap();
    let target = room.window.root.as_ref().unwrap().target.clone();
    let offered = describe::offered(&host, &target);
    let labels = |actions: &[RowAction]| -> Vec<String> {
        actions.iter().map(|action| action.label.clone()).collect()
    };
    assert!(
        labels(&offered).contains(&"Increment".to_owned()),
        "{:?}",
        labels(&offered)
    );
    let frame = frame_of(&room.window.root_tree().unwrap()).unwrap().clone();
    let header = outside(&offered, &frame);
    assert!(
        !labels(&header).contains(&"Increment".to_owned())
            && !labels(&header).contains(&"Decrement".to_owned()),
        "the counter's buttons are its content's: {:?}",
        labels(&header)
    );
}

#[test]
fn a_board_scenario_shows_the_apps_it_attaches_filled_by_their_own_scenarios() {
    fn walk<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) {
        if node.kind == "card" {
            out.push(node);
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let room = Room::seeded("board");
    let tree = room.window.root_tree().unwrap();
    let mut cards = Vec::new();
    walk(&tree, &mut cards);
    assert_eq!(cards.len(), 3, "the todo list, the notebook and the chats");
    assert!(
        room.items_of("so.tilo.lang.todo", "items") >= 12,
        "the list holds its scenario"
    );
    assert!(
        room.items_of("so.tilo.lang.notes", "notes") >= 12,
        "and the notebook its"
    );
}

#[test]
fn a_scenario_that_returns_a_resource_shows_it_beside_the_list() {
    let mut room = Room::seeded("notes");
    let shown = room
        .window
        .detail_target()
        .cloned()
        .expect("the scenario's note is shown");
    let host = room.window.current_host().unwrap();
    assert_eq!(describe::title_of(&host, &shown), "Q3 plan");
    let selected = room.list().selected.clone();
    let row = room
        .shown()
        .into_iter()
        .find(|row| row.item == shown)
        .map(|row| row.key);
    assert_eq!(selected, row, "and its row is the selected one");
}

#[test]
fn a_document_that_edits_its_title_shows_it_as_its_own_and_the_header_does_not_repeat_it() {
    let room = Room::seeded("notes");
    assert!(room.window.detail.is_some(), "the scenario's note is shown");
    let header = room
        .window
        .headers
        .get("content")
        .map(|header| header.split('\u{1f}').next().unwrap_or_default().to_owned());
    assert_eq!(
        header.as_deref(),
        Some(""),
        "the note's title is its own, not the header's"
    );
    let host = room.window.current_host().unwrap();
    let tree = room.window.detail_tree().unwrap();
    let mut page = content_page(frame_of(&tree).unwrap());
    assert!(
        mark_field_roles(&host, &mut page),
        "the editor binds the title"
    );
}

#[test]
fn the_list_renders_only_the_rows_it_shows() {
    let mut room = Room::seeded("notes");
    let rendered = |room: &Room| -> Vec<String> {
        let tree = room.window.root_tree().unwrap();
        frame_of(&tree)
            .unwrap()
            .children
            .iter()
            .filter(|row| !row.children.is_empty())
            .map(|row| row.key.clone())
            .collect()
    };
    let len = room.list().len();
    let first = rendered(&room);
    assert!(
        !first.is_empty() && first.len() < len,
        "the list asked for the rows it shows, {} of {len}",
        first.len()
    );
    room.list().show_rows(len - 2..len);
    let shown: Vec<String> = room.list().shown().into_iter().map(|row| row.key).collect();
    assert_eq!(
        rendered(&room),
        shown,
        "the rows scrolled away are forgotten"
    );
    assert_eq!(shown.len(), 2);
}

#[test]
fn a_change_to_an_item_reaches_its_shown_row() {
    let mut room = Room::seeded("notes");
    let (key, _) = room.rows().remove(0);
    room.ask(Request::Activate(key.clone()));
    let detail = room.window.detail_tree().expect("the note is shown");
    let input = first_input(&detail).expect("the note's title").key.clone();
    room.window
        .act(true, UiEvent::Edit(input, "Renamed".to_owned()));
    room.settle();
    let title = room
        .list()
        .find(&key)
        .and_then(|row| row.row)
        .and_then(|row| row.props.get("title").map(crate::reconcile::display));
    assert_eq!(title.as_deref(), Some("Renamed"));
}

/// The identity of every widget under `widget`, itself included, with its name.
fn widget_identities(widget: &dyn ui::widget::Widget, out: &mut Vec<(usize, String)>) {
    out.push((
        std::ptr::from_ref(widget).cast::<()>() as usize,
        widget.name().to_owned(),
    ));
    for child in widget.children() {
        widget_identities(child, out);
    }
}

/// The content column.
fn content_pane(room: &mut Room) -> &mut Pane {
    room.window
        .shell()
        .and_then(|shell| shell.content_mut::<Pane>())
        .expect("the content column")
}

/// The widgets of the content column.
fn content_widgets(room: &mut Room) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    widget_identities(content_pane(room), &mut out);
    out
}

/// The widget under `widget`, itself included, whose test id is `id`.
fn widget_with_id<'a>(
    widget: &'a dyn ui::widget::Widget,
    id: &str,
) -> Option<&'a dyn ui::widget::Widget> {
    if widget.test_id() == Some(id) {
        return Some(widget);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| widget_with_id(child, id))
}

#[test]
fn a_change_on_a_canvas_replaces_the_card_it_changed_and_keeps_every_other_widget() {
    use ui::widget::Widget as _;
    fn cards(node: &UiNode, out: &mut Vec<String>) {
        if node.kind == "card" {
            out.push(node.key.clone());
        }
        for child in &node.children {
            cards(child, out);
        }
    }
    let mut room = Room::seeded("board");
    let mut keys = Vec::new();
    cards(&room.window.root_tree().unwrap(), &mut keys);
    let [todos, notes, chats] = &keys[..] else {
        panic!("the todo list, the notebook and the chats: {keys:?}");
    };
    let widgets_of = |room: &mut Room, key: &str| {
        let mut out = Vec::new();
        widget_identities(
            widget_with_id(content_pane(room), key).expect("the card's content"),
            &mut out,
        );
        out
    };
    let chats_before = widgets_of(&mut room, chats);
    let notes_before = widgets_of(&mut room, notes);
    let todo_before = widgets_of(&mut room, todos);
    let outside_before: Vec<_> = content_widgets(&mut room)
        .into_iter()
        .filter(|widget| !todo_before.contains(widget))
        .collect();
    content_pane(&mut room).take_structure_change();

    // Ticking a todo off changes what the todo card counts and previews, and nothing else.
    let host = room.window.current_host().unwrap();
    let list = host.root_of("so.tilo.lang.todo").unwrap();
    let Value::List(items) = host.runtime().read(&list, "items").unwrap() else {
        panic!("the list holds items");
    };
    let open = items
        .iter()
        .find_map(|item| match item {
            Value::Ref(item)
                if host.runtime().read(&item.target, "done").ok() == Some(Value::Bool(false)) =>
            {
                Some(item.clone())
            }
            _ => None,
        })
        .expect("an open todo");
    let check = describe::offered(&host, &open.target)
        .iter()
        .find(|action| action.label == "Check")
        .cloned()
        .expect("an open todo can be checked");
    room.ask(Request::Invoke {
        action: check,
        title: String::new(),
    });
    let Value::List(items) = host.runtime().read(&list, "items").unwrap() else {
        panic!("the list holds items");
    };
    assert!(
        !items.contains(&Value::Ref(open.clone()))
            || host.runtime().read(&open.target, "done").ok() == Some(Value::Bool(true)),
        "the todo is checked"
    );

    assert_eq!(
        widgets_of(&mut room, chats),
        chats_before,
        "the chats card is kept"
    );
    assert_eq!(
        widgets_of(&mut room, notes),
        notes_before,
        "the notes card is kept"
    );
    let after = content_widgets(&mut room);
    let gone: Vec<_> = outside_before
        .iter()
        .filter(|widget| !after.contains(widget))
        .collect();
    // The card itself is replaced; what holds it is kept.
    assert!(
        gone.iter().all(|(_, name)| name == "Card"),
        "only the todo card is replaced: {gone:?}"
    );
    assert!(widget_with_id(content_pane(&mut room), todos).is_some());
    assert_eq!(
        content_pane(&mut room).take_structure_change(),
        ui::widget::StructureChange::Described,
        "the frame's damage describes the card it replaced"
    );
}

#[test]
fn a_conversation_is_a_transcript_whose_messages_are_built_as_it_shows_them() {
    fn rendered(node: &UiNode) -> usize {
        usize::from(node.kind == "item") + node.children.iter().map(rendered).sum::<usize>()
    }
    let mut room = Room::seeded("chat");
    let messages = |room: &mut Room| {
        thread_in(content_pane(room).content_mut())
            .map(|thread| ui::widget::ListModel::len(thread.model()))
    };
    let first = messages(&mut room).expect("the conversation's transcript");
    let thread_widget = |room: &mut Room| {
        thread_in(content_pane(room).content_mut())
            .map(|thread| std::ptr::from_ref(thread).cast::<()>() as usize)
    };
    let before = thread_widget(&mut room);
    // Enough messages that the transcript shows only the last of them.
    let sent = 80;
    for n in 0..sent {
        let detail = room.window.detail_tree().unwrap();
        let input = first_input(&detail).expect("the composer").key.clone();
        let button = first_button(&detail)
            .expect("the composer's send")
            .key
            .clone();
        room.window
            .act(true, UiEvent::Edit(input, format!("Message {n}")));
        room.settle();
        room.window.act(true, UiEvent::Press(button));
        room.settle();
    }
    let total = first + sent;
    assert_eq!(
        messages(&mut room),
        Some(total),
        "each message is a row more"
    );
    assert_eq!(thread_widget(&mut room), before, "of the same transcript");
    let detail = room.window.detail_tree().unwrap();
    assert!(
        rendered(&detail) < total / 2,
        "only the messages shown are rendered: {} of {total}",
        rendered(&detail)
    );
    assert!(
        format!("{detail:?}").contains(&format!("Message {}", sent - 1)),
        "the newest is shown"
    );
}

/// The first button under `node`.
fn first_button(node: &UiNode) -> Option<&UiNode> {
    if node.kind == "button" {
        return Some(node);
    }
    node.children.iter().find_map(first_button)
}

/// A page as a detail column shows it: a heading and a paragraph.
fn page_node(value: &str, extra: bool) -> UiNode {
    fn leaf(kind: &str, key: &str, value: &str) -> UiNode {
        let mut node = UiNode {
            kind: kind.to_owned(),
            key: key.to_owned(),
            props: std::collections::BTreeMap::new(),
            binding: None,
            events: std::collections::BTreeMap::new(),
            anchor: None,
            children: Vec::new(),
        };
        node.props.insert("value".to_owned(), Value::text(value));
        node
    }
    let mut children = vec![
        leaf("heading", "title", "Q3 plan"),
        leaf("text", "said", value),
    ];
    if extra {
        children.push(leaf("text", "more", "and more"));
    }
    UiNode {
        children,
        ..leaf("stack", "page", "")
    }
}

#[test]
fn a_changed_value_on_a_page_sets_its_widget_and_keeps_every_other() {
    use ui::widget::{StructureChange, Widget as _};
    let mut reconciler = Reconciler::new(Rc::default(), Rc::new(|_| String::new()));
    let mut pane = Pane::new(Box::new(Spacer::fixed(0.0)));
    let mut built = None;
    fill(
        &mut pane,
        &mut reconciler,
        &mut built,
        &page_node("Ship it", false),
    );
    pane.take_structure_change();
    let mut before = Vec::new();
    widget_identities(&pane, &mut before);

    // A value changed: the widget that shows it is set, nothing is replaced or rebuilt.
    fill(
        &mut pane,
        &mut reconciler,
        &mut built,
        &page_node("Shipped", false),
    );
    let mut after = Vec::new();
    widget_identities(&pane, &mut after);
    assert_eq!(after, before, "every widget is kept");
    assert_eq!(pane.take_structure_change(), StructureChange::None);

    // A paragraph inserted: described, and every other widget kept where it was.
    fill(
        &mut pane,
        &mut reconciler,
        &mut built,
        &page_node("Shipped", true),
    );
    let mut inserted = Vec::new();
    widget_identities(&pane, &mut inserted);
    assert!(before.iter().all(|widget| inserted.contains(widget)));
    assert_eq!(inserted.len(), before.len() + 1);
    assert_eq!(pane.take_structure_change(), StructureChange::Described);

    // And taken out again.
    fill(
        &mut pane,
        &mut reconciler,
        &mut built,
        &page_node("Shipped", false),
    );
    let mut removed = Vec::new();
    widget_identities(&pane, &mut removed);
    assert_eq!(removed, before);
    assert_eq!(pane.take_structure_change(), StructureChange::Described);
}
