//! The window over a room record, driven the way the framework drives a surface, with the door
//! played by the test: what the window hands over is built, applied to the record and answered.

use serde_json::Value as Json;
use tilolang_tilo::action::register_actions;
use tilolang_tilo::runtime::MemorySink;
use ui::automerge::AutoCommit;
use ui::component::{Component as _, RowSource as _};
use ui::tilo_actions::{
    ActionContext, ActionInvoker, ActionRegistry, EncodedAction, apply_local_with_context,
};
use ui::widget::controls::ActionControls as _;
use ui::widget::{ItemRow, ListItemProps};

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_render_protocol::patch::Answer;
use tilo_ui_host::pane::Pane;
use tilo_ui_host::reconcile::Reconciler;
use tilo_ui_host::reconcile::UiEvent;
use tilo_ui_host::rows::RemoteRows;
use tilo_ui_host::shell::DETAIL;
use tilo_ui_host::{UiNode, Value};
use tilolang_tilo::runtime::{Principal, Value as RtValue};
use ui::framework::{ResourceSurface as _, RunMode};
use ui::permissions::policy::{self, agent_answers};
use ui::widget::{DescribedList, NavPane, NavShell, Widget as _};

use super::*;

mod mail;
mod opening;

/// The text a value of the runtime holds.
fn rt_text(value: &RtValue) -> String {
    match value {
        RtValue::Text(text) => text.clone(),
        RtValue::Opt(Some(held)) => rt_text(held),
        _ => String::new(),
    }
}

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
        // Every batch a real app sends passes the UI host's check.
        if let Some(why) = self.window.view().failed() {
            panic!("the window refused what its app sent: {why}");
        }
    }

    /// Lay the window out at 1200x800, as a host does after a frame: what changed in place is
    /// laid out in place from then on.
    fn lay_out(&mut self) {
        use ui::taffy::prelude::{AvailableSpace, Size};
        let _ = self.window.take_structure_change();
        let mut taffy = ui::taffy::TaffyTree::new();
        let mut nodes = HashMap::new();
        let node = self
            .window
            .build_layout(&mut taffy, &ui::Theme::dark(), &mut nodes);
        taffy
            .compute_layout(
                node,
                Size {
                    width: AvailableSpace::Definite(1200.0),
                    height: AvailableSpace::Definite(800.0),
                },
            )
            .expect("the window lays out");
        self.window
            .apply_layout(&taffy, node, ui::kurbo::Point::ZERO);
    }

    /// Paint the window into `scene`, as a frame does.
    fn paint(&mut self, scene: &mut ui::vello::Scene) {
        let mut text = ui::TextRenderer::new();
        self.window.draw(
            scene,
            ui::kurbo::Affine::IDENTITY,
            &mut text,
            &ui::Theme::dark(),
            &ui::presence::PresenceCtx::default(),
        );
    }

    /// A drawn frame: the window painted, nothing left to repaint.
    fn draw(&mut self) {
        let mut scene = ui::vello::Scene::new();
        self.paint(&mut scene);
        self.window.mark_drawn();
    }

    fn resources() -> Vec<ui::tilo_actions::ResourceSpec> {
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

    /// The composed shell, as the window last built it.
    fn composed(&self) -> UiNode {
        self.window
            .composed()
            .cloned()
            .expect("the window composed its shell")
    }

    /// The rows of the root's list, as its list reads them.
    fn root_rows(&self) -> Rc<RefCell<RemoteRows>> {
        self.window
            .view()
            .lists()
            .into_iter()
            .find(|key| !key.starts_with(DETAIL))
            .and_then(|key| self.window.view().rows_of(&key))
            .expect("the root lists its items")
    }

    /// Ask for `range` of the root's list, as the list does when it shows them, and let the
    /// window take the rows the app renders for it.
    fn show_rows(&mut self, range: std::ops::Range<usize>) {
        self.root_rows().borrow_mut().request_range(range);
        self.frame();
    }

    /// Each row of the root's list, every one rendered as a list tall enough for all of them
    /// renders them: its key, and its item as the list reads it.
    fn items(&mut self) -> Vec<(String, ListItemProps)> {
        let len = self.root_rows().borrow().len();
        self.show_rows(0..len);
        let rows = self.root_rows();
        (0..len)
            .filter_map(|index| rows.borrow().key_at(index))
            .filter_map(|key| {
                let mut item = None;
                rows.borrow_mut()
                    .read(&key, &mut |source| {
                        item = Some(
                            ListItemProps::from_source(source)
                                .map_err(|error| error.to_string())?,
                        );
                        Ok(())
                    })
                    .unwrap();
                Some((key, item?))
            })
            .collect()
    }

    /// The keys and titles of the rows of the root's list.
    fn rows(&mut self) -> Vec<(String, String)> {
        self.items()
            .into_iter()
            .map(|(key, item)| {
                let title = match item.row {
                    ItemRow::Row(row) => row.title,
                    _ => String::new(),
                };
                (key, title)
            })
            .collect()
    }

    /// The resource the row `key` of the root's list shows.
    fn item_of(&mut self, key: &str) -> ResourceRef {
        let id = self
            .items()
            .into_iter()
            .find(|(row, _)| row == key)
            .and_then(|(_, item)| item.item)
            .expect("the row shows a resource")
            .0;
        let host = self.window.current_host().unwrap();
        let probe = ResourceRef::parse_id(&id, String::new()).unwrap();
        let ty = tilolang_tilo::runtime::store::Store::type_of(host.store(), &probe).unwrap();
        ResourceRef::parse_id(&id, ty).unwrap()
    }

    /// The list of the overview column.
    fn list(&mut self) -> &mut DescribedList {
        let shell = self.window.shell_mut().expect("the window shows the shell");
        if shell.overview_mut::<DescribedList>().is_some() {
            return shell.overview_mut::<DescribedList>().unwrap();
        }
        shell.content_mut::<DescribedList>().expect("a list column")
    }

    /// The key of the composed node of `kind` whose prop `name` is `value`.
    fn key_of(&self, kind: &str, name: &str, value: &str) -> String {
        node_with(&self.composed(), kind, name, value)
            .unwrap_or_else(|| panic!("no {kind} with {name} {value}"))
            .key
            .clone()
    }

    /// Report what a control of the composed shell did, and let the window settle.
    fn fire(&mut self, key: &str, event: &str, value: Option<Value>) {
        self.window
            .act(UiEvent::Fire(key.to_owned(), event.to_owned(), value));
        self.settle();
    }

    /// Press the header's button labelled `label`.
    fn header_action(&mut self, label: &str) {
        let key = self.key_of("ui.Button", "label", label);
        self.fire(&key, "press", None);
    }

    /// Activate the row `key` of the root's list, as a click on it does.
    fn activate(&mut self, key: &str) {
        let item = self.item_of(key);
        let list = node_with(&self.composed(), "ui.VirtualList", "", "")
            .expect("the composed list")
            .key
            .clone();
        self.fire(
            &list,
            "activate",
            Some(Value::Ref(tilolang_tilo::instance::wire::resource(&item))),
        );
    }

    fn pinned(&self, resource: &str) -> bool {
        let host = self.window.current_host().unwrap();
        let path = resource.strip_prefix("tl:so.tilo.lang.notes:").unwrap();
        let probe = tilolang_tilo::runtime::ResourceRef::parse_id(path, String::new()).unwrap();
        let ty = tilolang_tilo::runtime::store::Store::type_of(host.store(), &probe).unwrap();
        let note = tilolang_tilo::runtime::ResourceRef::parse_id(path, ty).unwrap();
        host.runtime().read(&note, "pinned").unwrap() == RtValue::Bool(true)
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
            RtValue::List(items) => items.len(),
            other => panic!("not a list: {other:?}"),
        }
    }
}

/// The first node of the tree with `kind` whose prop `name` is `value` (any, when `name` is
/// empty).
fn node_with<'a>(node: &'a UiNode, kind: &str, name: &str, value: &str) -> Option<&'a UiNode> {
    if node.kind == kind
        && (name.is_empty()
            || node
                .props
                .get(name)
                .map(tilo_ui_host::reconcile::display)
                .as_deref()
                == Some(value))
    {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| node_with(child, kind, name, value))
}

fn first_input(node: &UiNode) -> Option<&UiNode> {
    if node.kind == "ui.TextInput" {
        return Some(node);
    }
    node.children.iter().find_map(first_input)
}

/// What a person types into a rich-text input: one paragraph of `text`.
fn typed(text: &str) -> tilo_render_protocol::patch::rich::RichText {
    tilo_render_protocol::patch::rich::RichText {
        blocks: vec![tilo_render_protocol::patch::rich::Block {
            kind: "paragraph".to_owned(),
            text: text.to_owned(),
            ..Default::default()
        }],
        embeds: Vec::new(),
    }
}

/// The composer of a channel: its rich-text input and its send button.
fn composer(shell: &UiNode) -> (String, String) {
    let input = node_with(shell, "ui.RichInput", "", "")
        .expect("the composer")
        .key
        .clone();
    let send = node_with(shell, "ui.Button", "label", "Send")
        .expect("the composer's send")
        .key
        .clone();
    (input, send)
}

/// The children of the named slot `name` of a component node.
fn slot_of<'a>(node: &'a UiNode, name: &str) -> Vec<&'a UiNode> {
    node.children
        .iter()
        .filter(|child| {
            child.kind == tilo_ui_host::adapter::SLOT
                && child.props.get("name") == Some(&Value::text(name))
        })
        .flat_map(|child| child.children.iter())
        .collect()
}

/// The labels of the buttons in the header slot `slot` of the composed shell.
fn header_labels(shell: &UiNode, slot: &str) -> Vec<String> {
    slot_of(shell, slot)
        .into_iter()
        .filter_map(|node| match node.props.get("label") {
            Some(Value::Text(label)) => Some(label.clone()),
            _ if node.kind == "ui.SearchButton" => Some("search".to_owned()),
            _ => None,
        })
        .collect()
}

// ----- the shell ---------------------------------------------------------------------------

#[test]
fn a_notebook_is_the_native_list_and_detail_shell_with_the_header_the_platform_fills() {
    let mut room = Room::seeded("notes");
    assert!(
        room.shell().has_overview(),
        "the notes list is the overview column"
    );
    assert!(
        room.shell().has_sidebar(),
        "the note's filters are the sidebar's places"
    );
    let rows = room.rows();
    assert!(rows.len() >= 3, "{rows:?}");
    let shell = room.composed();
    assert_eq!(
        shell.props.get("overview_title"),
        Some(&Value::text("All notes"))
    );
    assert_eq!(
        header_labels(&shell, "overview_actions"),
        ["Sort", "search", "New note"],
        "the sort menu, search, and the primary action last"
    );
    // The scenario showed its note; with that closed, the content says so in one sentence.
    assert!(room.window.detail_target().is_some());
    room.window.close_detail();
    room.settle();
    assert!(room.window.detail_target().is_none());
    let placeholder = slot_of(&room.composed(), "content")
        .into_iter()
        .find(|node| node.kind == "ui.EmptyState")
        .cloned()
        .expect("the content says nothing is selected");
    assert_eq!(
        placeholder.props.get("title"),
        Some(&Value::text("No note selected"))
    );
}

// USER REQUIREMENT (2026-10-20): `open(r)` "the host minimally implements it as
// push-navigation to the resource's default view with back". It now shows the resource's
// default view in the next column, which a narrow window pushes with Back.
#[test]
fn activating_a_row_shows_the_item_in_the_next_column_and_back_returns_to_the_list() {
    let mut room = Room::seeded("notes");
    let (key, _) = room.rows().remove(1);
    room.activate(&key);
    let item = room.item_of(&key);
    assert_eq!(
        room.window.detail_target(),
        Some(&item),
        "the note is shown"
    );
    let detail = room.window.detail_tree().expect("the note is shown");
    assert!(
        first_input(&detail).is_some(),
        "the note's editor: {detail:#?}"
    );
    let selected: Vec<String> = room
        .list()
        .list()
        .unwrap()
        .selection()
        .stored_keys()
        .cloned()
        .collect();
    assert_eq!(selected, std::slice::from_ref(&key), "the row is selected");
    assert!(
        !room.composed().props.contains_key("title"),
        "the note shows its title as its own, which the header does not repeat"
    );
    // Editing the title changes the row.
    let input = first_input(&room.composed())
        .expect("the composed editor")
        .key
        .clone();
    room.window
        .act(UiEvent::Edit(input, "Plans for the week".to_owned()));
    room.settle();
    assert!(
        room.rows()
            .iter()
            .any(|(_, shown)| shown == "Plans for the week"),
        "{:?}",
        room.rows()
    );
    room.window.close_detail();
    room.settle();
    assert!(room.window.detail_target().is_none() || room.shell().front_pane() != NavPane::Content);
}

#[test]
fn the_primary_action_is_the_header_and_empty_states_action_and_creates_through_the_door() {
    let mut room = Room::new("notes");
    assert_eq!(room.items_of("so.tilo.lang.notes", "items"), 0);
    // An empty notebook offers its primary action and says one sentence.
    let empty = slot_of(&room.composed(), "overview")
        .into_iter()
        .find(|node| node.kind == "ui.EmptyState")
        .cloned()
        .expect("the empty notebook says so");
    assert_eq!(empty.props.get("title"), Some(&Value::text("No notes yet")));
    assert_eq!(empty.props.get("action"), Some(&Value::text("New note")));
    room.header_action("New note");
    assert_eq!(
        room.items_of("so.tilo.lang.notes", "items"),
        1,
        "the action inserted a note"
    );
    assert_eq!(room.rows().len(), 1);
    let (key, _) = room.rows().remove(0);
    let made = room.item_of(&key);
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
        .find(|d| d.resource_kind == "tl.so.tilo.lang.notes.Notes" && d.action_label == "New note")
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
    assert_eq!(room.items_of("so.tilo.lang.notes", "items"), 1);
    assert_eq!(
        room.window.detail_target(),
        None,
        "the agent's note did not open"
    );
    // So does a script on the control socket, which acts as the person but not at the window.
    let script = ActionInvoker::actor("ada".to_owned());
    room.applies_as(Some(script), &notebook, &new_note, &Json::Null)
        .unwrap();
    assert_eq!(room.items_of("so.tilo.lang.notes", "items"), 2);
    assert_eq!(
        room.window.detail_target(),
        None,
        "the script's note did not open"
    );
    // The person pressing New note in the window opens what it made, whatever was made before.
    room.header_action("New note");
    assert_eq!(room.items_of("so.tilo.lang.notes", "items"), 3);
    let opened = room
        .window
        .detail_target()
        .cloned()
        .expect("the person's note opened");
    let keys: Vec<String> = room.rows().into_iter().map(|(key, _)| key).collect();
    let theirs = keys
        .iter()
        .filter(|key| room.item_of(key) == opened)
        .count();
    assert_eq!(theirs, 1, "it is a note of the list");
    // And an agent's note after that leaves the person's open.
    room.applies_as(Some(task()), &notebook, &new_note, &Json::Null)
        .unwrap();
    assert_eq!(room.window.detail_target(), Some(&opened));
}

#[test]
fn a_new_channel_is_in_the_sidebar_before_anyone_writes_and_every_channel_under_its_category() {
    /// The labels of the navigation items under `node`.
    fn places(node: &UiNode, out: &mut Vec<String>) {
        if node.kind == "ui.NavItem" {
            out.push(tilo_ui_host::reconcile::display(&node.props["label"]));
        }
        for child in &node.children {
            places(child, out);
        }
    }
    /// The sections of the navigation list under `node`: each heading and its places.
    fn sections(node: &UiNode, out: &mut Vec<(String, Vec<String>)>) {
        if node.kind == "ui.NavSection" {
            let mut held = Vec::new();
            places(node, &mut held);
            out.push((tilo_ui_host::reconcile::display(&node.props["title"]), held));
            return;
        }
        for child in &node.children {
            sections(child, out);
        }
    }
    let mut room = Room::new("chat");
    assert!(
        node_with(
            &room.composed(),
            "ui.EmptyState",
            "title",
            "No channels yet"
        )
        .is_some(),
        "an empty workspace says so"
    );
    room.header_action("New channel");
    assert_eq!(room.items_of("so.tilo.lang.chat", "items"), 1);
    let shell = room.composed();
    let item = node_with(&shell, "ui.NavItem", "label", "new-channel")
        .expect("the new channel is in the sidebar, no message written");
    assert_eq!(
        item.props
            .get("icon")
            .map(tilo_ui_host::reconcile::display)
            .as_deref(),
        Some("hash"),
        "with the channels' glyph"
    );
    let seeded = Room::seeded("chat");
    let shell = seeded.composed();
    let mut all = Vec::new();
    places(&shell, &mut all);
    let mut grouped = Vec::new();
    sections(&shell, &mut grouped);
    assert_eq!(
        grouped,
        vec![
            (
                "Team".to_owned(),
                vec![
                    "design".to_owned(),
                    "release".to_owned(),
                    "planning".to_owned()
                ]
            ),
            (
                "Social".to_owned(),
                vec!["weekend".to_owned(), "books".to_owned()]
            ),
        ],
        "each category's channels under its heading"
    );
    assert_eq!(
        all,
        [
            "general", "random", "design", "release", "planning", "weekend", "books"
        ],
        "the channels outside any category first"
    );
}

#[test]
fn a_filter_case_from_the_sidebar_narrows_the_list_and_titles_it() {
    let mut room = Room::seeded("notes");
    let all = room.rows().len();
    let pinned = room.key_of("ui.NavItem", "label", "Pinned");
    room.fire(&pinned, "select", None);
    let shown = room.rows().len();
    assert!(shown < all, "{shown} of {all}");
    assert_eq!(
        room.composed().props.get("overview_title"),
        Some(&Value::text("Pinned"))
    );
}

#[test]
fn a_destructive_row_action_runs_through_the_menu_and_offers_undo() {
    let mut room = Room::seeded("notes");
    let before = room.rows().len();
    let (key, _) = room.rows().remove(0);
    let list = room.list();
    assert!(list.list_mut().unwrap().reveal(&key));
    let area = list.menu_area_mut().expect("the rows share one menu");
    assert!(
        area.open_at(ui::vello::kurbo::Point::ZERO, false),
        "the keyboard opens the focused row's menu"
    );
    let labels: Vec<Option<&str>> = area
        .menu()
        .items()
        .iter()
        .map(ui::widget::MenuItem::label)
        .collect();
    assert!(labels.contains(&Some("Delete")), "{labels:?}");
    assert!(
        labels.iter().any(|label| *label != Some("Delete")),
        "the row's menu has every action: {labels:?}"
    );
    let delete = labels
        .iter()
        .position(|label| *label == Some("Delete"))
        .unwrap();
    area.choose(delete);
    room.settle();
    assert_eq!(room.rows().len(), before - 1);
    assert!(room.shell().toast_text().is_some(), "the shell offers undo");
}

#[test]
fn a_scenario_stages_exactly_its_data_however_many_frames_run() {
    let mut room = Room::seeded("notes");
    let staged = room.items_of("so.tilo.lang.notes", "items");
    for _ in 0..6 {
        room.settle();
    }
    assert_eq!(room.items_of("so.tilo.lang.notes", "items"), staged);
    assert_eq!(room.rows().len(), staged);
    let records = tilolang_tilo::action::resource_ids(&room.doc, "so.tilo.lang.notes");
    assert_eq!(records.len(), 1, "one notebook");
}

#[test]
fn every_package_opens_into_its_shell() {
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
        let tree = room.window.root_tree().expect("the root view runs");
        assert!(
            tilo_ui_host::shell::shell_of(&tree).is_some(),
            "{} builds its shell: {tree:#?}",
            built.dir
        );
        assert_eq!(room.composed().kind, "ui.NavShell");
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
    assert!(kinds.contains(&"tl.so.tilo.lang.notes.Notes"), "{kinds:?}");
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
        &ui::widget::resource_operation_handlers(&room.window),
    );
    assert!(violations.is_empty(), "{violations:#?}");
    let output = room.agent_invokes(HOST, "catalogue", &Json::Null).unwrap();
    let text = output
        .value
        .expect("the catalogue is the answer")
        .to_string();
    assert!(text.contains("test.letters"), "{text}");
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
        .find(|d| d.resource_kind == "tl.test.letters.Draft" && d.action_label == "Send")
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
    let desk = host.root_of("test.letters").unwrap();
    let RtValue::List(drafts) = host.runtime().read(&desk, "drafts").unwrap() else {
        panic!("the desk lists its drafts");
    };
    let Some(RtValue::Ref(draft)) = drafts.first() else {
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
    room.activate(&key);
    let draft = room
        .window
        .detail_target()
        .cloned()
        .expect("the letter opened");
    let host = room.window.current_host().unwrap();
    let to = rt_text(&host.runtime().read(&draft, "to").unwrap());
    let send = node_with(&room.composed(), "ui.Button", "label", "Send")
        .expect("the letter's Send button")
        .key
        .clone();
    room.window
        .act(UiEvent::Fire(send, "press".to_owned(), None));
    room.settle();
    assert!(
        room.window.view().asks().is_some(),
        "a send asks the person first"
    );
    assert!(
        room.sink.delivered().is_empty(),
        "nothing leaves before they say so"
    );
    room.window.view().answer(Answer::Allow);
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
    let to = rt_text(&host.runtime().read(&draft, "to").unwrap());
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

/// A room of Today with the todo list and the notebook it points at, each with its scenario, and
/// how many of the todos are done.
fn today_room() -> (Room, usize) {
    let mut room = Room::new("today");
    room.seed("tasks");
    room.seed("notes");
    let host = room.window.current_host().unwrap();
    let list = host.root_of("so.tilo.lang.tasks").unwrap();
    let RtValue::List(items) = host.runtime().read(&list, "items").unwrap() else {
        panic!("the list holds items");
    };
    let done = items
        .iter()
        .filter(|item| match item {
            RtValue::Ref(item) => {
                host.runtime().read(&item.target, "done").ok() == Some(RtValue::Bool(true))
            }
            _ => false,
        })
        .count();
    assert!(done > 0, "the saturday list has done items");
    (room, done)
}

fn cleanup_args(room: &Room) -> Json {
    let host = room.window.current_host().unwrap();
    let list = host.root_of("so.tilo.lang.tasks").unwrap().id_string();
    let notebook = host.root_of("so.tilo.lang.notes").unwrap().id_string();
    serde_json::json!({
        "flow": "so.tilo.lang.today.morning_cleanup",
        "args": serde_json::json!({"list": list, "nb": notebook}).to_string(),
    })
}

#[test]
fn an_agent_runs_a_flow_of_a_trusted_package_and_reads_what_it_returned() {
    let (mut room, done) = today_room();
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
    let (mut room, done) = today_room();
    // The person narrowed this agent in the room: changes to the todo app ask first.
    let mut workspace = policy::RoomPolicy::new("room", "ada");
    workspace.grants.push(agent_answers::grant(
        "room",
        "agent-task:7",
        (
            &agent_answers::AnswerScope::app("so.tilo.lang.tasks"),
            policy::ActionKind::Undoable,
        ),
        policy::Answer::Ask,
        policy::Subject::person("ada"),
    ));
    let standing = policy::Standing::person(workspace, "ada");
    room.window
        .instance
        .set_standing(Rc::new(move || Some(standing.clone())));
    let todos = room.items_of("so.tilo.lang.tasks", "items");
    let notes = room.items_of("so.tilo.lang.notes", "items");
    let args = cleanup_args(&room);
    let waiting = room
        .agent_invokes(HOST, "run-flow", &args)
        .unwrap()
        .value
        .unwrap();
    assert_eq!(waiting["state"], "suspended", "{waiting}");
    assert!(
        room.window.view().asks().is_some(),
        "the window asks the person, as for a press"
    );
    assert_eq!(
        room.items_of("so.tilo.lang.tasks", "items"),
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
        room.items_of("so.tilo.lang.tasks", "items"),
        todos - done,
        "the done ones went, once"
    );
    assert_eq!(
        room.items_of("so.tilo.lang.notes", "items"),
        notes + 1,
        "and one note says so"
    );
    assert!(
        person_decides(&mut room, "approve", &run).is_err(),
        "it completes once"
    );
    room.settle();
    assert_eq!(room.items_of("so.tilo.lang.tasks", "items"), todos - done);
    assert_eq!(room.items_of("so.tilo.lang.notes", "items"), notes + 1);
}

#[test]
fn search_finds_chat_messages_and_a_hit_opens_their_channel() {
    let mut room = Room::seeded("chat");
    let entries = crate::resource::search_entries();
    let hit = entries
        .iter()
        .find(|entry| entry.title.contains("third floor"))
        .unwrap_or_else(|| panic!("a message is found: {entries:#?}"));
    // The persistent index keeps short fields, never a prose body.
    assert!(
        entries
            .iter()
            .flat_map(|entry| &entry.fields)
            .all(|(text, _)| text.chars().count() <= 80)
    );
    let anchor = hit.anchor.clone().expect("a hit names its resource");
    let before = format!("{:?}", room.window.instance().detail_tree());
    assert!(
        !before.contains("Coffee machine on the third floor is fixed."),
        "the scenario opens another channel"
    );
    room.window.open_requests().borrow_mut().push(anchor);
    room.settle();
    // The messages are rows of the transcript: what the app rendered of them.
    let channel = room
        .window
        .instance()
        .detail_tree()
        .expect("the hit opened its channel");
    assert!(
        format!("{channel:?}").contains("Coffee machine on the third floor is fixed."),
        "the channel shows the message: {channel:#?}"
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
    assert!(
        room.window.detail_target().is_some(),
        "the hit showed the note"
    );
}

#[test]
fn an_action_the_content_shows_as_a_button_is_not_repeated_in_the_header() {
    let room = Room::seeded("counter");
    let host = room.window.current_host().unwrap();
    let target = room.window.instance().root_target().cloned().unwrap();
    let offered: Vec<String> = host
        .runtime()
        .catalogue()
        .into_iter()
        .map(|signature| signature.name)
        .collect();
    assert!(
        offered.iter().any(|name| name == "increment"),
        "{offered:?}"
    );
    let _ = target;
    assert_eq!(
        header_labels(&room.composed(), "actions"),
        ["search"],
        "the counter's buttons are its content's"
    );
}

// One control per action: an item's "Done" switch in its window is how it is checked there, so
// the header does not offer "Check" beside it. The action stays the item's, on the door and in
// its row's menu.
#[test]
fn a_control_bound_to_the_state_an_action_toggles_stands_for_it_in_the_header() {
    let mut room = Room::seeded("tasks");
    assert!(
        room.window.detail_target().is_some(),
        "the scenario's item is shown"
    );
    let header = header_labels(&room.composed(), "actions");
    assert!(
        !header
            .iter()
            .any(|label| label == "Complete" || label == "Reopen"),
        "the Done switch stands for Complete: {header:?}"
    );
    assert!(
        header.iter().any(|label| label == "Remove"),
        "the rest of the item's actions stay: {header:?}"
    );
    let menus: Vec<String> = room
        .items()
        .into_iter()
        .flat_map(|(_, item)| {
            item.menu
                .into_iter()
                .filter_map(|entry| entry.label().map(str::to_owned))
        })
        .collect();
    assert!(
        menus
            .iter()
            .any(|label| label == "Complete" || label == "Reopen"),
        "and in its row's menu: {menus:?}"
    );
}

#[test]
fn a_board_scenario_shows_the_apps_it_attaches_filled_by_their_own_scenarios() {
    fn walk<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) {
        if node.kind == "ui.Card" {
            out.push(node);
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let room = Room::seeded("today");
    let tree = room.window.root_tree().unwrap();
    let mut cards = Vec::new();
    walk(&tree, &mut cards);
    assert_eq!(cards.len(), 3, "the todo list, the notebook and the chats");
    assert!(
        room.items_of("so.tilo.lang.tasks", "items") >= 12,
        "the list holds its scenario"
    );
    assert!(
        room.items_of("so.tilo.lang.notes", "items") >= 12,
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
    assert_eq!(tilolang_tilo::instance::title_of(&host, &shown), "Q3 plan");
    let selected: Vec<String> = room
        .list()
        .list()
        .unwrap()
        .selection()
        .stored_keys()
        .cloned()
        .collect();
    let row: Vec<String> = room
        .rows()
        .into_iter()
        .map(|(key, _)| key)
        .filter(|key| room.item_of(key) == shown)
        .collect();
    assert_eq!(selected, row, "and its row is the selected one");
}

#[test]
fn a_document_that_edits_its_title_shows_it_as_its_own_and_the_header_does_not_repeat_it() {
    let room = Room::seeded("notes");
    assert!(
        room.window.detail_target().is_some(),
        "the scenario's note is shown"
    );
    let shell = room.composed();
    assert!(
        !shell.props.contains_key("title"),
        "the note's title is its own, not the header's"
    );
    let title = first_input(&shell).expect("the editor");
    assert_eq!(
        title.props.get("title"),
        Some(&Value::Int(1)),
        "the editor binds the title as its large title"
    );
}

#[test]
fn the_list_renders_only_the_rows_it_shows() {
    let mut room = Room::seeded("notes");
    let rows = room.root_rows();
    // What the app renders: the rows of the list in its view's tree.
    let rendered = |room: &Room| -> Vec<String> {
        let tree = room.window.instance().root_tree().unwrap();
        let list = first_of(&tree, "ui.VirtualList").expect("the list").clone();
        list.children
            .iter()
            .filter(|row| row.kind != tilo_ui_host::adapter::SLOT && !row.children.is_empty())
            .map(|row| row.key.clone())
            .collect()
    };
    let len = rows.borrow().len();
    room.show_rows(0..3);
    let first = rendered(&room);
    assert!(
        !first.is_empty() && first.len() < len,
        "the list asked for the rows it shows, {} of {len}",
        first.len()
    );
    room.show_rows(len - 2..len);
    let shown: Vec<String> = (len - 2..len)
        .filter_map(|index| rows.borrow().key_at(index))
        .collect();
    assert_eq!(
        rendered(&room),
        shown,
        "the rows scrolled away are forgotten"
    );
    assert_eq!(
        rows.borrow().rendered(),
        {
            let mut sorted = shown;
            sorted.sort();
            sorted
        },
        "and the window holds only those"
    );
}

/// The first node of the runtime's tree of `kind`.
fn first_of<'a>(
    node: &'a tilolang_tilo::runtime::UiNode,
    kind: &str,
) -> Option<&'a tilolang_tilo::runtime::UiNode> {
    if node.kind == kind {
        return Some(node);
    }
    node.children.iter().find_map(|child| first_of(child, kind))
}

#[test]
fn a_change_to_an_item_reaches_its_shown_row() {
    let mut room = Room::seeded("notes");
    let (key, _) = room.rows().remove(0);
    room.activate(&key);
    let input = first_input(&room.composed())
        .expect("the note's title")
        .key
        .clone();
    room.window.act(UiEvent::Edit(input, "Renamed".to_owned()));
    room.settle();
    let title = room
        .rows()
        .into_iter()
        .find(|(row, _)| *row == key)
        .map(|(_, title)| title);
    assert_eq!(title.as_deref(), Some("Renamed"));
}

#[test]
fn a_rows_checkbox_shows_whether_its_item_is_done_from_its_first_read() {
    let mut room = Room::staged("tasks", "a busy week");
    let checked: Vec<bool> = room
        .items()
        .into_iter()
        .filter_map(|(_, item)| match item.row {
            ItemRow::Row(row) => row.checkbox.map(|bound| bound.state.get()),
            _ => None,
        })
        .collect();
    assert!(
        checked.iter().any(|done| *done) && checked.iter().any(|done| !*done),
        "the done items' checkboxes are checked, the others not: {checked:?}"
    );
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

/// The content column's scroll.
fn content_scroll(room: &mut Room) -> &mut ui::widget::Scrollable {
    room.window
        .shell_mut()
        .and_then(|shell| shell.content_mut::<ui::widget::Scrollable>())
        .expect("the content column scrolls")
}

/// The widgets of the content column.
fn content_widgets(room: &mut Room) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    widget_identities(content_scroll(room), &mut out);
    out
}

#[test]
fn a_change_on_a_canvas_replaces_the_card_it_changed_and_keeps_every_other_widget() {
    use ui::widget::Widget as _;
    fn cards(node: &UiNode, out: &mut Vec<String>) {
        if node.kind == "ui.Card" {
            out.push(node.key.clone());
        }
        for child in &node.children {
            cards(child, out);
        }
    }
    let mut room = Room::seeded("today");
    let mut keys = Vec::new();
    cards(&room.composed(), &mut keys);
    let [todos, notes, chats] = &keys[..] else {
        panic!("the todo list, the notebook and the chats: {keys:?}");
    };
    let widgets_of = |room: &mut Room, key: &str| {
        let mut out = Vec::new();
        widget_identities(
            widget_with_id(content_scroll(room), key).expect("the card's content"),
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
    content_scroll(&mut room).take_structure_change();

    // Ticking a todo off changes what the todo card counts and previews, and nothing else.
    let host = room.window.current_host().unwrap();
    let list = host.root_of("so.tilo.lang.tasks").unwrap();
    let RtValue::List(items) = host.runtime().read(&list, "items").unwrap() else {
        panic!("the list holds items");
    };
    let open = items
        .iter()
        .find_map(|item| match item {
            RtValue::Ref(item)
                if host.runtime().read(&item.target, "done").ok() == Some(RtValue::Bool(false)) =>
            {
                Some(item.clone())
            }
            _ => None,
        })
        .expect("an open todo");
    let check = host
        .packages()
        .into_iter()
        .find_map(|package| {
            package
                .meta
                .actions
                .iter()
                .find(|action| action.name == "check")
                .map(|action| action.id.clone())
        })
        .expect("todo checks its items");
    host.runtime()
        .invoke_action(
            &Principal::new("ada"),
            (check.as_str(), vec![RtValue::Ref(open.clone())]),
        )
        .unwrap();
    room.window.commit_planned();
    room.settle();
    assert_eq!(
        host.runtime().read(&open.target, "done").ok(),
        Some(RtValue::Bool(true)),
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
    assert!(widget_with_id(content_scroll(&mut room), todos).is_some());
    assert_eq!(
        content_scroll(&mut room).take_structure_change(),
        ui::widget::StructureChange::Described,
        "the frame's damage describes the card it replaced"
    );
}

#[test]
fn a_conversation_is_a_transcript_whose_messages_are_built_as_it_shows_them() {
    fn rendered(node: &tilolang_tilo::runtime::UiNode) -> usize {
        usize::from(node.kind == "ui.ListItem") + node.children.iter().map(rendered).sum::<usize>()
    }
    fn transcript(room: &mut Room) -> &mut DescribedList {
        let shell = room.window.shell_mut().expect("the shell");
        let docked = shell
            .content_mut::<ui::widget::Docked>()
            .expect("the conversation above its composer");
        let mut found = None;
        for child in docked.children_mut() {
            if let Some(list) = child
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<DescribedList>())
            {
                found = Some(list);
                break;
            }
        }
        found.expect("the transcript")
    }
    let mut room = Room::seeded("chat");
    let messages = |room: &mut Room| {
        transcript(room)
            .list()
            .map(|list| ui::widget::ListModel::len(list.model()))
    };
    let first = messages(&mut room).expect("the conversation's transcript");
    let before = std::ptr::from_ref(transcript(&mut room)).cast::<()>() as usize;
    // Enough messages that the transcript shows only the last of them.
    let sent = 80;
    for n in 0..sent {
        let (input, button) = composer(&room.composed());
        room.window
            .act(UiEvent::EditRich(input, typed(&format!("Message {n}"))));
        room.settle();
        room.window
            .act(UiEvent::Fire(button, "press".to_owned(), None));
        room.settle();
    }
    let total = first + sent;
    assert_eq!(
        messages(&mut room),
        Some(total),
        "each message is a row more"
    );
    assert_eq!(
        std::ptr::from_ref(transcript(&mut room)).cast::<()>() as usize,
        before,
        "of the same transcript"
    );
    let detail = room.window.instance().detail_tree().unwrap();
    assert!(
        rendered(&detail) < total / 2,
        "only the messages shown are rendered: {} of {total}",
        rendered(&detail)
    );
}

#[test]
fn a_transcript_names_each_run_of_one_authors_messages_once_and_marks_where_the_day_changes() {
    /// The day dividers, the avatars shown and the avatars' rooms kept empty under `widget`.
    fn marks(widget: &dyn ui::widget::Widget, found: &mut (usize, usize, usize)) {
        match widget.name() {
            "DayDivider" => found.0 += 1,
            "Avatar" if widget.accessibility().is_some() => found.1 += 1,
            "Avatar" => found.2 += 1,
            _ => {}
        }
        for child in widget.children() {
            marks(child, found);
        }
    }
    // The design channel: Mia twice, Leo, you, the next day Ada and Mia.
    let mut room = Room::seeded("chat");
    for _ in 0..3 {
        room.lay_out();
        room.settle();
    }
    let shell = room.window.shell_mut().expect("the shell");
    let docked = shell
        .content_mut::<ui::widget::Docked>()
        .expect("the conversation above its composer");
    let transcript = docked
        .children()
        .into_iter()
        .find(|child| {
            child
                .as_any()
                .is_some_and(|any| any.downcast_ref::<DescribedList>().is_some())
        })
        .expect("the transcript");
    let mut found = (0, 0, 0);
    marks(transcript, &mut found);
    assert_eq!(
        found,
        (2, 5, 1),
        "a divider for each of the two days, an avatar for each of the five runs, and the room of one under the message that continues a run"
    );
}

/// A page as a detail column shows it: a heading and a paragraph.
fn page_node(value: &str, extra: bool) -> UiNode {
    fn leaf(kind: &str, key: &str, value: &str) -> UiNode {
        let mut node = UiNode::empty(kind, key);
        node.props.insert("value".to_owned(), Value::text(value));
        node
    }
    let mut children = vec![
        leaf("ui.Text", "title", "Q3 plan"),
        leaf("ui.Text", "said", value),
    ];
    if extra {
        children.push(leaf("ui.Text", "more", "and more"));
    }
    UiNode {
        children,
        ..leaf("stack", "page", "")
    }
}

#[test]
fn a_changed_value_on_a_page_sets_its_widget_and_keeps_every_other() {
    use ui::widget::{StructureChange, Widget as _};
    let mut reconciler = Reconciler::new(Rc::default());
    let first = page_node("Ship it", false);
    let mut pane = Pane::new(reconciler.build(&first));
    pane.take_structure_change();
    let mut before = Vec::new();
    widget_identities(&pane, &mut before);
    let mut step = |pane: &mut Pane, old: &UiNode, new: &UiNode| {
        if let Some(replaced) = reconciler.update_in(pane.content_mut(), old, new) {
            pane.replace(replaced);
        }
    };

    // A value changed: the widget that shows it is set, nothing is replaced or rebuilt.
    let changed = page_node("Shipped", false);
    step(&mut pane, &first, &changed);
    let mut after = Vec::new();
    widget_identities(&pane, &mut after);
    assert_eq!(after, before, "every widget is kept");
    assert_eq!(pane.take_structure_change(), StructureChange::None);

    // A paragraph inserted: described, and every other widget kept where it was.
    let longer = page_node("Shipped", true);
    step(&mut pane, &changed, &longer);
    let mut inserted = Vec::new();
    widget_identities(&pane, &mut inserted);
    assert!(before.iter().all(|widget| inserted.contains(widget)));
    assert_eq!(inserted.len(), before.len() + 1);
    assert_eq!(pane.take_structure_change(), StructureChange::Described);

    // And taken out again.
    step(&mut pane, &longer, &changed);
    let mut removed = Vec::new();
    widget_identities(&pane, &mut removed);
    assert_eq!(removed, before);
    assert_eq!(pane.take_structure_change(), StructureChange::Described);
}

// ----- an app's own frame ------------------------------------------------------------------

// What `std` does, an app does by composing the toolkit's components with the platform's
// answers: the shelf builds its own list window, with the header, the menus, the swipe and the
// next column, and gets what the platform does for every window.
#[test]
fn an_app_composes_its_own_list_window_from_the_toolkit_and_the_platform() {
    let mut room = Room::seeded("handmade");
    assert!(room.shell().has_overview(), "the books are the overview");
    let shell = room.composed();
    assert_eq!(
        shell.props.get("overview_title"),
        Some(&Value::text("Shelf"))
    );
    assert_eq!(
        header_labels(&shell, "overview_actions"),
        ["search", "New book"],
        "the header holds the shelf's primary action"
    );
    let items = room.items();
    assert_eq!(items.len(), 2);
    let (first_key, first) = &items[0];
    let menu: Vec<&str> = first
        .menu
        .iter()
        .filter_map(ui::widget::MenuPart::label)
        .collect();
    assert_eq!(
        menu,
        ["Mark read", "Remove"],
        "the book's actions and the shelf's on it"
    );
    let swipe = first.swipe.as_ref().expect("a swipe takes the book off");
    assert!(swipe.destructive);
    // The scenario opened its book beside the list, which selects it.
    let shown = room
        .window
        .detail_target()
        .cloned()
        .expect("the book is shown");
    assert_eq!(room.item_of(first_key), shown);
    let selected: Vec<String> = room
        .list()
        .list()
        .unwrap()
        .selection()
        .stored_keys()
        .cloned()
        .collect();
    assert_eq!(selected, std::slice::from_ref(first_key));
    // A destructive action from its menu offers Undo, as from any window.
    let list = room.list();
    assert!(list.list_mut().unwrap().reveal(first_key));
    let area = list.menu_area_mut().expect("the rows share one menu");
    assert!(area.open_at(ui::vello::kurbo::Point::ZERO, false));
    area.choose(1);
    room.settle();
    assert_eq!(room.rows().len(), 1);
    assert!(room.shell().toast_text().is_some(), "the shell offers undo");
    // And its primary action shows what it made.
    room.header_action("New book");
    let made = room
        .window
        .detail_target()
        .cloned()
        .expect("the new book opened");
    let host = room.window.current_host().unwrap();
    assert_eq!(tilolang_tilo::instance::title_of(&host, &made), "Untitled");
}

// ----- later builds ------------------------------------------------------------------------

/// Notes compiled again with `edit` applied to its source, against its ids lock: a later build
/// of the package, as `tilolang dev` makes one.
fn later_notes(edit: impl Fn(&str) -> String) -> tilolang_tilo::bundle::Bundle {
    let package = crate::packages::EMBEDDED
        .iter()
        .find(|package| package.dir == "notes")
        .expect("notes is built in");
    let lock = tilolang_tilo::tilolang::IdsLock::parse(package.ids).unwrap();
    let used: Vec<_> = crate::app::builtin::compiled()
        .iter()
        .take_while(|built| built.dir != "notes")
        .map(|built| &built.compiled)
        .collect();
    let imports: Vec<_> = used.iter().map(|compiled| compiled.meta.clone()).collect();
    let compiled = tilolang_tilo::tilolang::compile_package(&edit(package.source), &lock, &imports)
        .unwrap_or_else(|diagnostics| panic!("the edit compiles: {diagnostics:?}"));
    tilolang_tilo::bundle::Bundle::new(&compiled, used)
}

/// Whether `text` is shown anywhere under `node`.
fn shows_text(node: &UiNode, text: &str) -> bool {
    node.props
        .values()
        .any(|value| matches!(value, Value::Text(shown) if shown == text))
        || node.children.iter().any(|child| shows_text(child, text))
}

#[test]
fn a_later_build_takes_over_in_place_keeping_window_state_selection_and_detail() {
    let mut room = Room::seeded("notes");
    let all = room.rows().len();
    // A place of the sidebar narrows the list: the person's filter.
    let pinned = room.key_of("ui.NavItem", "label", "Pinned");
    room.fire(&pinned, "select", None);
    let filtered = room.rows().len();
    assert!(filtered < all, "{filtered} of {all}");
    let (key, _) = room.rows().remove(0);
    room.activate(&key);
    let shown = room
        .window
        .detail_target()
        .cloned()
        .expect("a note is shown");
    let detail = room.window.detail_tree().unwrap();
    assert!(shows_text(&detail, "Write something"), "{detail:#?}");

    crate::updates::hand_over(later_notes(|source| {
        source.replace("\"Write something\"", "\"Write anything\"")
    }));
    room.settle();

    let detail = room.window.detail_tree().expect("the note is still shown");
    assert!(
        shows_text(&detail, "Write anything"),
        "the new build renders it: {detail:#?}"
    );
    assert_eq!(room.window.detail_target(), Some(&shown), "the same note");
    assert_eq!(
        room.composed().props.get("overview_title"),
        Some(&Value::text("Pinned")),
        "the same place of the sidebar is chosen"
    );
    assert_eq!(room.rows().len(), filtered, "the list is still filtered");
    let selected: Vec<String> = room
        .list()
        .list()
        .unwrap()
        .selection()
        .stored_keys()
        .cloned()
        .collect();
    assert_eq!(
        selected,
        std::slice::from_ref(&key),
        "the row stays selected"
    );
}

#[test]
fn a_later_build_that_changes_a_field_type_starts_over_on_the_same_data() {
    let mut room = Room::seeded("notes");
    let notes = room.rows().len();
    assert!(room.window.detail_target().is_some());

    crate::updates::hand_over(later_notes(|source| {
        let source = source
            .replace("tags: Set<Text>", "tags: Set<Int>")
            .replace(
                "action tag(n: Note, t: Text)",
                "action tag(n: Note, t: Int)",
            )
            .replace(
                "action untag(n: Note, t: Text)",
                "action untag(n: Note, t: Int)",
            )
            .replace("ui.Chip(t)", "ui.Chip(\"{t}\")");
        ["planning", "work", "q3"]
            .iter()
            .enumerate()
            .fold(source, |source, (n, tag)| {
                source.replace(&format!("tags.add(\"{tag}\")"), &format!("tags.add({n})"))
            })
    }));
    room.settle();

    let host = room.window.current_host().unwrap();
    let note = host
        .package("so.tilo.lang.notes")
        .and_then(|package| package.meta.types.into_iter().find(|ty| ty.name == "Note"))
        .unwrap();
    assert!(
        note.fields.iter().any(|field| field.ty == "Set<Int>"),
        "the new build runs: {note:?}"
    );
    assert_eq!(room.rows().len(), notes, "every note is still there");
    assert_eq!(room.window.detail_target(), None, "the window started over");
}

#[test]
fn typing_into_a_shown_items_title_keeps_the_windows_tree() {
    use ui::widget::Widget as _;
    let mut room = Room::seeded("tasks");
    let (key, _) = room.rows().remove(0);
    room.activate(&key);
    let detail = room.window.detail_tree().expect("the item is shown");
    let input = first_input(&detail).expect("the item's title").key.clone();
    // The list mounts the rows its laid-out height shows, measures them as they are laid out,
    // then holds them.
    room.lay_out();
    room.settle();
    room.lay_out();
    room.lay_out();
    let mut typed = String::new();
    for letter in ['p', 'l', 'a', 'n'] {
        typed.push(letter);
        room.window.act(UiEvent::Edit(input.clone(), typed.clone()));
        room.settle();
        let change = room.window.take_structure_change();
        assert!(
            matches!(
                change,
                ui::widget::StructureChange::None | ui::widget::StructureChange::Children
            ),
            "a keystroke rebuilt the window's tree ({change:?}): its row and its field change \
             in place"
        );
        room.lay_out();
    }
}

#[test]
fn a_keystroke_repaints_its_field_not_the_window() {
    // The item's column slides in: what a keystroke repaints is asked of the
    // window at rest.
    let _settled = ui::motion::testing::settled();
    let mut room = Room::seeded("tasks");
    let (key, _) = room.rows().remove(0);
    room.activate(&key);
    let detail = room.window.detail_tree().expect("the item is shown");
    let input = first_input(&detail).expect("the item's title").key.clone();
    room.lay_out();
    room.settle();
    room.lay_out();
    room.draw();
    let window = room.window.get_rect();
    let mut typed = String::new();
    for letter in ['p', 'l', 'a', 'n'] {
        typed.push(letter);
        room.window.act(UiEvent::Edit(input.clone(), typed.clone()));
        room.settle();
        // Drawn where the last layout placed it: the harness lays a window
        // out only by rebuilding it, which the live window does not do for
        // a keystroke.
        let mut scene = ui::vello::Scene::new();
        room.paint(&mut scene);
        let mut damage = Vec::new();
        room.window.collect_damage(&mut damage);
        room.window.mark_drawn();
        // The field and the row change; nothing that holds them repaints
        // as a whole (the harness, which presents no frame, leaves the
        // list's column to repaint its box).
        assert!(
            !damage.is_empty() && damage.iter().all(|rect| rect.area() < window.area() / 2.0),
            "a keystroke repainted what holds the field: {damage:?} of {window:?}"
        );
    }
}

#[test]
fn a_list_that_asks_for_more_rows_shows_the_ones_it_has_until_they_come() {
    let mut room = Room::staged("notes", "a week of notes");
    let rows = room.root_rows();
    let len = rows.borrow().len();
    room.show_rows(0..len);
    assert_eq!(
        rows.borrow().rendered().len(),
        len,
        "the list shows every row"
    );
    // The list asks for fewer rows (it has not measured them yet), then for all of them: until
    // the app answers, the rows it had stay.
    rows.borrow_mut().request_range(0..len / 2);
    assert_eq!(
        rows.borrow().rendered().len(),
        len,
        "a row is kept until the app forgot it"
    );
    room.frame();
    rows.borrow_mut().request_range(0..len);
    assert_eq!(
        rows.borrow().rendered().len(),
        len / 2,
        "what the app forgot is gone, nothing else"
    );
    room.frame();
    assert_eq!(rows.borrow().rendered().len(), len, "and every row is back");
}

#[test]
fn todays_cards_show_their_resources_title_count_and_first_rows() {
    fn walk<'a>(node: &'a UiNode, kind: &str, out: &mut Vec<&'a UiNode>) {
        if node.kind == kind {
            out.push(node);
        }
        for child in &node.children {
            walk(child, kind, out);
        }
    }
    let text = |node: &UiNode, name: &str| match node.props.get(name) {
        Some(Value::Text(text)) => text.clone(),
        _ => String::new(),
    };
    let mut room = Room::seeded("today");
    // What the window holds of the view, as the app sent it.
    let tree = room.window.root_tree().unwrap();
    let mut cards = Vec::new();
    walk(&tree, "ui.Card", &mut cards);
    assert_eq!(cards.len(), 3, "the todo list, the notebook and the chats");
    assert_eq!(
        text(cards[0], "title"),
        "Tasks",
        "a card is headed by its resource's title"
    );
    let mut shown = Vec::new();
    let mut talks = Vec::new();
    for card in &cards {
        assert!(
            !text(card, "title").is_empty(),
            "every card has a title: {card:?}"
        );
        let Some(Value::Int(count)) = card.props.get("count") else {
            panic!("every card counts what it lists: {card:?}");
        };
        let preview = usize::try_from(*count).unwrap().min(5);
        let mut rows = Vec::new();
        walk(card, "ui.ListRow", &mut rows);
        if text(card, "title") == "Chat" {
            // The chat's channels and categories: a button that opens each channel outside a
            // category, and each category's name.
            let mut buttons = Vec::new();
            walk(card, "ui.Button", &mut buttons);
            let mut captions = Vec::new();
            walk(card, "ui.Text", &mut captions);
            talks.extend(buttons.iter().map(|button| text(button, "label")));
            talks.extend(captions.iter().map(|caption| text(caption, "value")));
            assert!(rows.is_empty(), "{rows:?}");
            assert_eq!(talks.len(), preview, "a card shows its first entries: Chat");
            continue;
        }
        assert_eq!(
            rows.len(),
            preview,
            "a card shows its first rows: {}",
            text(card, "title")
        );
        assert!(
            rows.iter().all(|row| !text(row, "title").is_empty()),
            "each row is titled"
        );
        shown.extend(rows.iter().map(|row| text(row, "title")));
    }
    assert!(
        shown.iter().any(|title| title == "Buy oat milk and eggs"),
        "the open todos are the todo card's rows: {shown:?}"
    );
    assert_eq!(
        talks,
        ["# general", "# random", "Team", "Social"],
        "the chat card's entries"
    );
    // And the window draws them: a header and a row widget for each.
    let widgets = content_widgets(&mut room);
    let named = |name: &str| widgets.iter().filter(|(_, n)| n == name).count();
    assert_eq!(named("SectionHeader"), 3, "{widgets:?}");
    assert_eq!(named("ListRow"), shown.len(), "{widgets:?}");
}

#[test]
fn a_send_pressed_in_the_frame_of_the_last_keystroke_sends_it_too() {
    let mut room = Room::seeded("chat");
    let (input, send) = composer(&room.composed());
    room.window
        .act(UiEvent::EditRich(input.clone(), typed("See you at noo")));
    room.settle();
    // The last keystroke and the press go to the app in one batch, before the view runs again.
    room.window
        .act(UiEvent::EditRich(input, typed("See you at noon")));
    room.window
        .act(UiEvent::Fire(send, "press".to_owned(), None));
    room.settle();
    let host = room.window.current_host().expect("the host started");
    let channel = room.window.detail_target().expect("the channel").clone();
    let items = |target: &ResourceRef, field: &str| match host.runtime().read(target, field) {
        Ok(RtValue::List(items)) => items
            .into_iter()
            .filter_map(|item| match item {
                RtValue::Ref(item) => Some(ResourceRef::clone(&item.target)),
                _ => None,
            })
            .collect::<Vec<_>>(),
        other => panic!("{field} is a collection: {other:?}"),
    };
    let last = items(&channel, "messages").pop().expect("a message");
    let RtValue::Rich(body) = host.runtime().read(&last, "body").unwrap() else {
        panic!("a message's body is rich text");
    };
    assert_eq!(
        body.written_text(),
        "See you at noon",
        "the message has the keystroke typed in the frame of the press"
    );
}
