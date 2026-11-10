//! The window over a workspace record, driven the way the framework drives a surface, with the door
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
use ui::widget::{DescribedList, NavShell, Widget as _};

use super::*;

mod clocks;
mod mail;
mod opening;
mod personal;
mod tree;

/// The text a value of the runtime holds.
fn rt_text(value: &RtValue) -> String {
    match value {
        RtValue::Text(text) => text.clone(),
        RtValue::Opt(Some(held)) => rt_text(held),
        _ => String::new(),
    }
}

const HOST: &str = "tl:host";

struct Workspace {
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

impl Workspace {
    /// The window of `app` over an empty workspace.
    fn new(app: &str) -> Self {
        crate::app::builtin::choose(app);
        let meta = crate::app::builtin::package_named(app).expect("a built-in package");
        let sink = MemorySink::new();
        let window =
            Window::new(meta.app.clone(), "ada".to_owned(), RunMode::Live).with_sink(sink.clone());
        let mut workspace = Self {
            doc: AutoCommit::new(),
            window,
            sink,
        };
        workspace.settle();
        workspace
    }

    /// The window of `app` with its scenario `scenario` staged, as a capture stages it.
    fn staged(app: &str, scenario: &str) -> Self {
        let mut workspace = Self::new(app);
        workspace.window.stage(Some(scenario));
        workspace.settle();
        workspace
    }

    /// The window of `app` over an empty workspace as `person`, keeping `record` as their own
    /// state of it: the view state they pick, and what they have seen.
    fn personal(app: &str, person: &str, record: WorkspaceRecord<Personal>) -> Self {
        crate::app::builtin::choose(app);
        let meta = crate::app::builtin::package_named(app).expect("a built-in package");
        let sink = MemorySink::new();
        let window = Window::new(meta.app.clone(), person.to_owned(), RunMode::Live)
            .with_sink(sink.clone())
            .keeping_personal(Some(record));
        let mut workspace = Self {
            doc: AutoCommit::new(),
            window,
            sink,
        };
        workspace.settle();
        workspace
    }

    /// [`Self::personal`] with `app`'s first scenario staged.
    fn personal_seeded(app: &str, person: &str, record: WorkspaceRecord<Personal>) -> Self {
        let mut workspace = Self::personal(app, person, record);
        let scenario = crate::app::builtin::package_named(app)
            .and_then(|meta| meta.scenarios.first())
            .map(|scenario| scenario.name.clone())
            .expect("the package declares a scenario");
        workspace.window.stage(Some(&scenario));
        workspace.settle();
        workspace
    }

    /// The window of `app` with its first scenario staged.
    fn seeded(app: &str) -> Self {
        let scenario = crate::app::builtin::package_named(app)
            .and_then(|meta| meta.scenarios.first())
            .map(|scenario| scenario.name.clone())
            .expect("the package declares a scenario");
        Self::staged(app, &scenario)
    }

    /// Fill the workspace with the first scenario of another package too.
    fn seed(&mut self, app: &str) {
        let host = self.window.current_host().expect("the host started");
        let meta = crate::app::builtin::package_named(app).expect("a built-in package");
        let scenario = meta
            .scenarios
            .first()
            .expect("the package declares a scenario");
        // A scenario fills its package's root, which a workspace makes the first time it is wanted.
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
                let context = ActionContext::for_resource("workspace", "tl.workspace", "workspace")
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

    /// Frames and doors until the window has nothing more to hand over, and its instance
    /// reads the indexes its packages' load built: those are read from the record on threads
    /// of their own, which a window with a frame loop is woken by and a harness here waits for.
    fn settle(&mut self) {
        for _ in 0..4 {
            self.frame();
            self.door();
        }
        if !self.index_ready() {
            self.window.host().expect("the host started").wait_index();
        }
        assert!(self.index_ready(), "the window reads its packages' indexes");
        self.frame();
    }

    /// Whether the window's instance reads the indexes its packages' load built off the frame.
    fn index_ready(&self) -> bool {
        self.window.host().is_some_and(|host| host.index_ready())
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

    /// The tree of the row the window shows first.
    fn first_row_tree(&mut self) -> Option<tilolang_tilo::runtime::UiNode> {
        let len = self.root_rows().borrow().len();
        self.show_rows(0..len);
        self.row_tree()
    }

    /// The tree of the row the window shows first, standing open into its item where it stands
    /// — the page a person reads under the row — as a press on the row opens it.
    fn first_open_row(&mut self) -> Option<tilolang_tilo::runtime::UiNode> {
        let len = self.root_rows().borrow().len();
        self.show_rows(0..len);
        let key = self.root_rows().borrow().key_at(0)?;
        // A click tells the app which item stands open, as `on opened(x): set(opened, x)` does,
        // so its page stays open when the view runs again.
        let item = self.item_of(&key);
        let value =
            tilolang_tilo::runtime::Value::some(tilolang_tilo::runtime::Value::resource(&item));
        let view = self
            .window
            .instance()
            .view(tilo_render_protocol::patch::Lane::Root)?;
        view.borrow().set_view_state("opened", value);
        open_in_list(
            self.window.shell_mut().expect("the window shows the shell"),
            key,
        );
        // The list asks for the row standing open, the app builds its page, and the window
        // mounts it under the row.
        self.lay_out();
        self.frame();
        self.settle();
        self.lay_out();
        self.row_tree()
    }

    /// The key of the window's own list: the column the shell shows, not a list a row standing
    /// open holds in its document, which a window's rows carry too once it opens.
    fn root_list(&self) -> String {
        fn outermost(node: &UiNode) -> Option<String> {
            if tilo_render_protocol::patch::is_virtual_list(&node.kind)
                && !node.key.starts_with(DETAIL)
            {
                return Some(node.key.clone());
            }
            node.children.iter().find_map(outermost)
        }
        outermost(&self.composed()).expect("the window shows a list of its own")
    }

    /// The tree of the first row the app rendered for the list it shows.
    fn row_tree(&self) -> Option<tilolang_tilo::runtime::UiNode> {
        let list = self.root_list();
        let view = self
            .window
            .instance()
            .view(tilo_render_protocol::patch::Lane::Root)?;
        view.borrow_mut().row(&list, 0).ok().flatten().cloned()
    }

    /// The rows of the root's list, as its list reads them.
    fn root_rows(&self) -> Rc<RefCell<RemoteRows>> {
        let key = self.root_list();
        self.window.view().rows_of(&key).unwrap_or_else(|| {
            panic!(
                "the window holds the rows of {key}: {:?}",
                self.window.view().lists()
            )
        })
    }

    fn show_rows(&mut self, range: std::ops::Range<usize>) {
        self.root_rows().borrow_mut().request_range(range, None);
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
        if shell.overview_list_mut().is_some() {
            return shell.overview_list_mut().unwrap();
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

    /// The window's search, as the bar runs it: what the window offers the bar and the driver
    /// that types into it.
    fn window_search(&mut self) -> (ui::widget::SearchDriver, ui::widget::InPlace) {
        let in_place = ui::widget::InPlace::of(&mut self.window);
        let mut driver = ui::widget::SearchDriver::default();
        driver.open(in_place);
        (driver, in_place)
    }

    /// Send what the person typed to the record, as the window's next frame does.
    fn land_typed(&mut self) {
        self.window.commit_planned();
        self.settle();
    }

    /// Type `text` into the window's search, as the bar does, and take what the app answers:
    /// the bar reads the count again on the frame after, as the app's answer arrives.
    fn type_in_search(&mut self, driver: &mut ui::widget::SearchDriver, text: &str) {
        for _ in 0..2 {
            driver.drive(
                &mut self.window,
                text,
                ui::widget::FindOptions::default(),
                None,
                false,
            );
            self.settle();
        }
    }

    /// Press the header's button labelled `label`: the window bar's, or the sidebar's own where
    /// the app makes what it lists there.
    fn header_action(&mut self, label: &str) {
        // The bar's own button for the action, not a control of the page that happens to be
        // named the same (a change mark's item is titled like the note it names).
        fn walk<'a>(node: &'a UiNode, label: &str) -> Option<&'a UiNode> {
            let names = node
                .props
                .get("label")
                .is_some_and(|said| tilo_ui_host::reconcile::display(said) == label);
            if node.kind == "ui.Button" && names {
                return Some(node);
            }
            node.children.iter().find_map(|child| walk(child, label))
        }
        let composed = self.composed();
        let key = ["overview_actions", "actions", "sidebar_actions"]
            .into_iter()
            .flat_map(|slot| slot_of(&composed, slot))
            .find_map(|node| walk(node, label).map(|button| button.key.clone()))
            .unwrap_or_else(|| panic!("the window's header offers {label}"));
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

    /// The states of the invocations the workspace record holds.
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

    /// The keys in `at`'s set field `field`, as the runtime reads them.
    fn set_of(&self, at: &ResourceRef, field: &str) -> Vec<String> {
        let host = self.window.current_host().unwrap();
        match host.runtime().read(at, field).unwrap() {
            RtValue::List(items) => items.into_iter().filter_map(rt_text_opt).collect(),
            other => panic!("{field} is not a set: {other:?}"),
        }
    }
}

/// The text a value of the runtime holds, as an item of a list of texts.
fn rt_text_opt(value: RtValue) -> Option<String> {
    match value {
        RtValue::Text(text) => Some(text),
        RtValue::Opt(Some(held)) => rt_text_opt(*held),
        _ => None,
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

/// The key of the first text input under the runtime's node `node`, as a row renders it.
/// Open the row `key` where it stands in the list under `widget`, as a press on it does.
fn open_in_list(widget: &mut dyn ui::widget::Widget, key: String) -> bool {
    if let Some(list) = widget
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<DescribedList>())
        && let Some(rows) = list.list_mut()
    {
        rows.set_open(Some(key));
        return true;
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| open_in_list(child, key.clone()))
}

fn first_input_key(node: &tilolang_tilo::runtime::UiNode) -> Option<String> {
    if node.kind == "ui.TextInput" {
        return Some(node.key.clone());
    }
    node.children.iter().find_map(first_input_key)
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
fn a_notebook_is_the_native_tree_shell_with_the_header_the_platform_fills() {
    let mut workspace = Workspace::seeded("notes");
    assert!(
        !workspace.shell().has_overview(),
        "no list of notes stands between the tree and the note it opens"
    );
    assert!(
        workspace.shell().has_sidebar(),
        "the notebook is the sidebar's tree"
    );
    assert!(
        workspace.shell().has_content(),
        "the note chosen stands beside the tree"
    );
    let notes = workspace.tree_labels();
    assert!(notes.len() >= 3, "{notes:?}");
    assert_eq!(notes[0], "Q3 plan", "the notebook's notes, in its order");
    let shell = workspace.composed();
    assert_eq!(
        shell.props.get("sidebar_title"),
        Some(&Value::text("Notes"))
    );
    assert!(
        workspace.header_buttons("sidebar_actions").is_empty(),
        "the sidebar's header offers no button of the window's: {:?}",
        workspace.header_buttons("sidebar_actions")
    );
    assert!(
        workspace
            .header_buttons("actions")
            .contains(&"New note".to_owned()),
        "the window's primary action stands in its bar: {:?}",
        workspace.header_buttons("actions")
    );
    // The window opens on the first note of the tree, its page beside it.
    let opened = workspace.opened_note();
    let host = workspace.window.current_host().unwrap();
    assert_eq!(tilolang_tilo::instance::title_of(&host, &opened), "Q3 plan");
}

// USER REQUIREMENT (2026-10-20): `open(r)` "the host minimally implements it as
// push-navigation to the resource's default view with back". A note chosen in the tree stands
// beside it, in the window's own content column, and its page edits the note.
#[test]
fn choosing_a_note_in_the_tree_shows_it_beside_the_tree() {
    let mut workspace = Workspace::seeded("notes");
    let first = workspace.opened_note();
    // A note inside another: the bar names where it stands, the note under it.
    let tree = workspace.key_of("ui.TreeView", "name", "Notes");
    let inside = workspace.tree_row_note("Standups");
    workspace.fire(
        &tree,
        "select",
        Some(Value::Ref(tilolang_tilo::instance::wire::resource(&inside))),
    );
    assert_ne!(
        inside, first,
        "another note than the one the window opened on"
    );
    assert_eq!(
        workspace.opened_note(),
        inside,
        "the row's note stands in the content column"
    );
    assert_eq!(
        workspace.composed().props.get("title"),
        Some(&Value::text("Standups")),
        "the bar names the note the page shows, for when the page's heading scrolls away"
    );
    // Editing the title on the page renames the note, and the tree's row says so.
    let input = first_input(&workspace.composed())
        .expect("the composed editor")
        .key
        .clone();
    workspace
        .window
        .act(UiEvent::Edit(input, "Plans for the week".to_owned()));
    workspace.settle();
    assert!(
        workspace
            .tree_labels()
            .iter()
            .any(|shown| shown == "Plans for the week"),
        "{:?}",
        workspace.tree_labels()
    );
}

/// Every `ui.EmptyState` the node holds: what a column says when it lists nothing.
fn empty_states(node: &UiNode, out: &mut Vec<UiNode>) {
    if node.kind == "ui.EmptyState" {
        out.push(node.clone());
    }
    for child in &node.children {
        empty_states(child, out);
    }
}

#[test]
fn the_primary_action_is_the_header_and_empty_states_action_and_creates_through_the_door() {
    let mut workspace = Workspace::new("notes");
    assert_eq!(workspace.items_of("so.tilo.lang.notes", "notes"), 0);
    // An empty notebook says one sentence in its sidebar, and its primary action stands once,
    // in that sidebar's header: the empty state has no button of its own.
    let mut empty = Vec::new();
    for node in slot_of(&workspace.composed(), "sidebar") {
        empty_states(node, &mut empty);
    }
    let empty = empty.first().expect("the empty notebook says so");
    assert_eq!(empty.props.get("title"), Some(&Value::text("No notes yet")));
    assert!(
        workspace.header_buttons("sidebar_actions").is_empty(),
        "the sidebar's header offers no second button"
    );
    assert!(
        workspace
            .header_buttons("actions")
            .contains(&"New note".to_owned()),
        "the bar offers the primary once: {:?}",
        workspace.header_buttons("actions")
    );
    workspace.header_action("New note");
    assert_eq!(
        workspace.items_of("so.tilo.lang.notes", "notes"),
        1,
        "the action inserted a note"
    );
    assert_eq!(workspace.tree_labels(), ["Untitled"]);
    let made = workspace.tree_row_note("Untitled");
    assert_eq!(
        workspace.opened_note(),
        made,
        "and the note it made stands beside the tree"
    );
}

/// A change mark lists what a run changed, each item titled by the item: pressing one opens that
/// item beside the list.
#[test]
fn a_change_marks_item_opens_the_note_it_names() {
    let mut workspace = Workspace::new("notes");
    let (notebook, new_note) = new_note_on_the_door();
    workspace
        .applies_as(Some(task()), &notebook, &new_note, &Json::Null)
        .unwrap();
    let item = key_ending(&workspace.composed(), "~change~since-0~item")
        .expect("the mark lists what the run made");
    assert_eq!(
        workspace.window.detail_target(),
        None,
        "the mark's item does not open itself"
    );
    workspace.fire(&item, "press", None);
    let opened = workspace
        .window
        .detail_target()
        .cloned()
        .expect("pressing the mark's item opens it");
    let host = workspace.window.current_host().unwrap();
    let root = host.root_of("so.tilo.lang.notes").unwrap();
    let RtValue::List(notes) = host.runtime().read(&root, "notes").unwrap() else {
        panic!("the notebook lists its notes");
    };
    let notes: Vec<String> = notes
        .into_iter()
        .filter_map(|note| match note {
            RtValue::Ref(note) => Some(note.target.id_string()),
            _ => None,
        })
        .collect();
    assert!(
        notes.contains(&opened.id_string()),
        "the mark opened a note of the notebook: {opened:?}, {notes:?}"
    );
}

/// The key of the composed node whose key ends with `suffix`.
fn key_ending(node: &UiNode, suffix: &str) -> Option<String> {
    if node.key.ends_with(suffix) {
        return Some(node.key.clone());
    }
    node.children
        .iter()
        .find_map(|child| key_ending(child, suffix))
}

/// The published id and action id of the notebook's "New note".
fn new_note_on_the_door() -> (String, String) {
    let descriptor = Workspace::surveyed()
        .into_iter()
        .find(|d| {
            d.resource_kind == "tl.so.tilo.lang.notes.Notebook" && d.action_label == "New note"
        })
        .expect("the notebook publishes New note");
    (descriptor.address.resource, descriptor.address.action)
}

#[test]
fn what_an_agent_or_a_script_creates_through_the_door_moves_nobodys_view() {
    let mut workspace = Workspace::new("notes");
    let (notebook, new_note) = new_note_on_the_door();
    // An agent's task makes a note: the window stays where the person left it.
    workspace
        .applies_as(Some(task()), &notebook, &new_note, &Json::Null)
        .unwrap();
    assert_eq!(workspace.items_of("so.tilo.lang.notes", "notes"), 1);
    assert_eq!(
        workspace.window.detail_target(),
        None,
        "the agent's note did not open"
    );
    // So does a script on the control socket, which acts as the person but not at the window.
    let script = ActionInvoker::actor("ada".to_owned());
    workspace
        .applies_as(Some(script), &notebook, &new_note, &Json::Null)
        .unwrap();
    assert_eq!(workspace.items_of("so.tilo.lang.notes", "notes"), 2);
    assert_eq!(
        workspace.window.detail_target(),
        None,
        "the script's note did not open"
    );
    // The person pressing New note in the window opens what it made, whatever was made before.
    workspace.header_action("New note");
    assert_eq!(workspace.items_of("so.tilo.lang.notes", "notes"), 3);
    let opened = workspace.opened_note();
    let notes: Vec<String> = workspace.tree_labels();
    assert!(
        notes.iter().filter(|label| *label == "Untitled").count() == 3,
        "it is a note of the notebook: {notes:?}"
    );
    // And an agent's note after that leaves the person's open.
    workspace
        .applies_as(Some(task()), &notebook, &new_note, &Json::Null)
        .unwrap();
    assert_eq!(workspace.opened_note(), opened);
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
    let mut workspace = Workspace::new("chat");
    assert!(
        node_with(
            &workspace.composed(),
            "ui.EmptyState",
            "title",
            "No channels yet"
        )
        .is_some(),
        "an empty workspace says so"
    );
    // While there is no channel the empty state carries the only New channel button, so the
    // header does not show the same one twice; its button comes back with the first channel.
    let empty = workspace.key_of("ui.EmptyState", "title", "No channels yet");
    workspace.fire(&empty, "act", None);
    assert_eq!(workspace.items_of("so.tilo.lang.chat", "channels"), 1);
    let shell = workspace.composed();
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
    let seeded = Workspace::seeded("chat");
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
            // The people the workspace knows, each a conversation to open: the sidebar keeps
            // them apart from the channels, under a heading of their own.
            (
                "Direct messages".to_owned(),
                vec!["Mia".to_owned(), "Leo".to_owned(), "Ada".to_owned()]
            ),
        ],
        "each category's channels under its heading"
    );
    assert_eq!(
        all,
        [
            "general",
            "random",
            "design",
            "release",
            "planning",
            "weekend",
            "books",
            // The conversations with a person, and the one with yourself, which the sidebar
            // labels as your notes: they come after the channels they are listed with.
            "Leo",
            "Notes to self",
            "Mia",
            "Leo",
            "Ada"
        ],
        "the channels outside any category first"
    );
}

#[test]
fn a_filter_case_from_the_sidebar_narrows_the_list_and_titles_it() {
    let mut workspace = Workspace::seeded("mail");
    // The inbox narrows the account's mail; the place a person chooses says so over the list.
    let inbox = workspace.rows().len();
    assert!(inbox > 0, "the inbox lists its conversations");
    assert_eq!(
        workspace.composed().props.get("overview_title"),
        Some(&Value::text("Inbox"))
    );
    let all = workspace.key_of("ui.NavItem", "label", "All mail");
    workspace.fire(&all, "select", None);
    let shown = workspace.rows().len();
    assert!(shown > inbox, "{shown} of {inbox}");
    assert_eq!(
        workspace.composed().props.get("overview_title"),
        Some(&Value::text("All mail"))
    );
    // And the inbox narrows it back, under its own name.
    let back = workspace.key_of("ui.NavItem", "label", "All inboxes");
    workspace.fire(&back, "select", None);
    assert_eq!(workspace.rows().len(), inbox, "the inbox again");
    assert_eq!(
        workspace.composed().props.get("overview_title"),
        Some(&Value::text("Inbox"))
    );
}

#[test]
fn a_destructive_row_action_runs_through_the_menu_and_offers_undo() {
    // A thread's menu keeps Delete on the row itself, outside the runs of commands it groups.
    let mut workspace = Workspace::seeded("mail");
    let before = workspace.rows();
    let (key, title) = before.first().cloned().expect("mail has threads");
    let list = workspace.list();
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
    workspace.settle();
    assert!(
        workspace.shell().toast_text().is_some(),
        "the shell offers undo"
    );
    // The thread went to the trash, and its row stands where it stood, drawn in its new state,
    // until the person leaves the place the query chose them for.
    let kept = workspace.rows();
    assert_eq!(kept.len(), before.len(), "no row moves: {kept:?}");
    assert_eq!(
        kept.first(),
        Some(&(key.clone(), title)),
        "the row still presents the thread it presented"
    );
    // Leaving the place settles the list: the inbox does not hold the trashed thread.
    let all = workspace.key_of("ui.NavItem", "label", "All mail");
    workspace.fire(&all, "select", None);
    let inbox = workspace.key_of("ui.NavItem", "label", "All inboxes");
    workspace.fire(&inbox, "select", None);
    let settled = workspace.rows();
    assert_eq!(
        settled.len(),
        before.len() - 1,
        "the thread left the inbox: {settled:?}"
    );
    assert!(
        !settled.iter().any(|(row, _)| *row == key),
        "and its row went with it"
    );
}

#[test]
fn a_scenario_stages_exactly_its_data_however_many_frames_run() {
    let mut workspace = Workspace::seeded("notes");
    let staged = workspace.items_of("so.tilo.lang.notes", "notes");
    for _ in 0..6 {
        workspace.settle();
    }
    assert_eq!(workspace.items_of("so.tilo.lang.notes", "notes"), staged);
    assert_eq!(workspace.tree_labels().len(), staged);
    let records = tilolang_tilo::action::resource_ids(&workspace.doc, "so.tilo.lang.notes");
    assert_eq!(records.len(), 1, "one notebook");
}

#[test]
fn every_package_opens_into_its_shell() {
    for built in crate::app::builtin::compiled() {
        let meta = &built.compiled.meta;
        // A package with no scenario has nothing to open with: its records are made from the
        // resources they are about, not by the person in its own window.
        if meta
            .roots
            .first()
            .is_none_or(|root| meta.default_view(root).is_none())
            || meta.scenarios.is_empty()
        {
            continue;
        }
        let mut workspace = Workspace::seeded(built.dir);
        let tree = workspace.window.root_tree().expect("the root view runs");
        assert!(
            tilo_ui_host::shell::shell_of(&tree).is_some(),
            "{} builds its shell: {tree:#?}",
            built.dir
        );
        assert_eq!(workspace.composed().kind, "ui.NavShell");
        workspace.settle();
    }
}

#[test]
fn the_published_tree_has_the_host_and_every_record() {
    // The live window publishes its workspace while it lives.
    let _workspace = Workspace::seeded("notes");
    let resources = Workspace::resources();
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
    let mut workspace = Workspace::seeded("letters");
    let host_actions: Vec<String> = Workspace::surveyed()
        .into_iter()
        .filter(|d| d.address.resource == HOST)
        .map(|d| d.address.action)
        .collect();
    for wanted in ["run-block", "catalogue"] {
        assert!(host_actions.iter().any(|a| a == wanted), "{host_actions:?}");
    }
    let resources = Workspace::resources();
    let violations = ui::tilo_actions::validate_surface(
        &resources,
        &ActionRegistry::with(register_actions),
        &ui::widget::resource_operation_handlers(&workspace.window),
    );
    assert!(violations.is_empty(), "{violations:#?}");
    let output = workspace
        .agent_invokes(HOST, "catalogue", &Json::Null)
        .unwrap();
    let text = output
        .value
        .expect("the catalogue is the answer")
        .to_string();
    assert!(text.contains("test.letters"), "{text}");
}

#[test]
fn a_view_block_a_value_block_and_a_bad_block_answer_an_agent() {
    let mut workspace = Workspace::seeded("chat");
    let run = |workspace: &mut Workspace, source: &str| {
        workspace.agent_invokes(HOST, "run-block", &serde_json::json!({ "source": source }))
    };
    let view = run(&mut workspace, "stack { text(\"Hello from an agent\") }")
        .unwrap()
        .value
        .unwrap();
    assert_eq!(view["kind"], "view", "{view}");
    let data = run(&mut workspace, "1 + 2").unwrap().value.unwrap();
    assert_eq!(data, serde_json::json!({"kind": "data", "value": 3}));
    let refused = run(&mut workspace, "nothing.here(").unwrap_err();
    assert!(refused.contains("does not compile"), "{refused}");
}

#[test]
fn the_verbs_of_items_are_published_on_the_door_and_an_agent_calls_them() {
    let mut workspace = Workspace::seeded("notes");
    let survey = Workspace::surveyed();
    let pin = survey
        .iter()
        .find(|d| d.resource_kind == "tl.so.tilo.lang.notes.Note" && d.action_label == "Pin")
        .expect("a note publishes its own verbs");
    let (resource, action) = (pin.address.resource.clone(), pin.address.action.clone());
    assert!(
        resource.contains('/'),
        "an item is addressed by its path: {resource}"
    );
    let before = workspace.pinned(&resource);
    workspace
        .applies_as(Some(task()), &resource, &action, &Json::Null)
        .unwrap();
    assert_eq!(
        workspace.pinned(&resource),
        !before,
        "the agent pinned the note"
    );
    // What the agent passes wins over the default, which would flip the note back.
    let pinned = serde_json::json!({ "pinned": !before });
    workspace
        .applies_as(Some(task()), &resource, &action, &pinned)
        .unwrap();
    assert_eq!(
        workspace.pinned(&resource),
        !before,
        "an explicit `pinned:` is the outcome"
    );

    // A draft's send reaches outside: asked of an agent, it is recorded as a suggestion.
    let mut desk = Workspace::seeded("letters");
    let send = Workspace::surveyed()
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

/// The first draft on the desk of a letters workspace.
fn first_draft(workspace: &Workspace) -> tilolang_tilo::runtime::ResourceRef {
    let host = workspace.window.current_host().expect("the host started");
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
    let mut workspace = Workspace::new("chat");
    workspace.seed("letters");
    let host = workspace.window.current_host().unwrap();
    let source = format!(
        "letters.send(@letters.Draft:{})",
        first_draft(&workspace).id_string()
    );
    let output = workspace
        .agent_invokes(HOST, "run-block", &serde_json::json!({ "source": source }))
        .unwrap()
        .value
        .unwrap();
    assert_eq!(output["kind"], "invocation", "{output}");
    assert_eq!(
        output["state"], "suggested",
        "an agent's send asks first: {output}"
    );
    // The invocation is a resource of the workspace record, so every replica shows it.
    let id = output["invocation"].as_str().unwrap();
    let invocation = host.invocation_ref(id).unwrap();
    let in_record = tilolang_tilo::action::resource_ids(&workspace.doc, tilolang_tilo::HOST_APP);
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
    assert!(workspace.sink.delivered().is_empty());
}

fn person_decides(workspace: &mut Workspace, decision: &str, target: &str) -> Result<Json, String> {
    workspace
        .invokes_as(
            None,
            HOST,
            "resolve",
            &serde_json::json!({"target": target, "decision": decision}),
        )
        .map(|output| output.value.unwrap_or_default())
}

#[test]
fn approving_a_pressed_send_through_the_ask_bar_delivers_the_letter_once() {
    let mut workspace = Workspace::seeded("letters");
    let key = workspace.rows()[0].0.clone();
    workspace.activate(&key);
    let draft = workspace
        .window
        .detail_target()
        .cloned()
        .expect("the letter opened");
    let host = workspace.window.current_host().unwrap();
    let to = rt_text(&host.runtime().read(&draft, "to").unwrap());
    let send = node_with(&workspace.composed(), "ui.MenuItem", "label", "Send")
        .expect("the letter's Send command, behind the \"…\" beside its title")
        .key
        .clone();
    workspace
        .window
        .act(UiEvent::Fire(send, "choose".to_owned(), None));
    workspace.settle();
    assert!(
        workspace.window.view().asks().is_some(),
        "a send asks the person first"
    );
    assert!(
        workspace.sink.delivered().is_empty(),
        "nothing leaves before they say so"
    );
    workspace.window.view().answer(Answer::Allow);
    workspace.settle();
    let delivered = workspace.sink.delivered();
    assert_eq!(delivered.len(), 1, "{delivered:?}");
    assert_eq!(delivered[0].0, "post");
    assert!(delivered[0].1.to_string().contains(&to), "{delivered:?}");
    workspace.settle();
    workspace.settle();
    assert_eq!(
        workspace.sink.delivered().len(),
        1,
        "settled once, not again"
    );
    // The record says it was delivered, so no device queues it again.
    assert_eq!(
        tilolang_tilo::runtime::store::Store::outbox_pending(host.store()).len(),
        0
    );
}

#[test]
fn a_person_approves_an_agents_suggested_send_through_the_door_and_it_leaves_once() {
    let mut workspace = Workspace::new("chat");
    workspace.seed("letters");
    let host = workspace.window.current_host().unwrap();
    let draft = first_draft(&workspace);
    let to = rt_text(&host.runtime().read(&draft, "to").unwrap());
    let source = format!("letters.send(@letters.Draft:{})", draft.id_string());
    let suggested = workspace
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
    let refused = workspace.invokes_as(
        Some(task()),
        HOST,
        "resolve",
        &serde_json::json!({"target": id, "decision": "approve"}),
    );
    assert!(
        refused.unwrap_err().contains("only a person"),
        "agents never decide"
    );
    assert!(workspace.sink.delivered().is_empty());
    let decided = person_decides(&mut workspace, "approve", &id).unwrap();
    assert_eq!(decided["state"], "done", "{decided}");
    let delivered = workspace.sink.delivered();
    assert_eq!(delivered.len(), 1, "{delivered:?}");
    assert!(delivered[0].1.to_string().contains(&to), "{delivered:?}");
    // Approving again changes nothing: the suggestion is decided.
    assert!(person_decides(&mut workspace, "approve", &id).is_err());
    workspace.settle();
    assert_eq!(workspace.sink.delivered().len(), 1);
}

/// A workspace of Today with the todo list and the notebook it points at, each with its scenario, and
/// how many of the todos are finished: done itself, and so is every task under it, as the app
/// reads a branch.
fn today_workspace() -> (Workspace, usize) {
    let mut workspace = Workspace::new("today");
    workspace.seed("tasks");
    workspace.seed("notes");
    let host = workspace.window.current_host().unwrap();
    let list = host.root_of("so.tilo.lang.tasks").unwrap();
    let RtValue::List(items) = host.runtime().read(&list, "items").unwrap() else {
        panic!("the list holds items");
    };
    let mut parent: HashMap<String, Option<String>> = HashMap::new();
    let mut done: HashMap<String, bool> = HashMap::new();
    for item in items {
        let RtValue::Ref(item) = item else {
            panic!("a task is a resource: {item:?}")
        };
        let id = item.target.id_string();
        let under = match host.runtime().read(&item.target, "parent").unwrap() {
            RtValue::Opt(Some(held)) => match *held {
                RtValue::Ref(p) => Some(p.target.id_string()),
                other => panic!("a parent is a task: {other:?}"),
            },
            RtValue::Opt(None) => None,
            other => panic!("a parent may be none: {other:?}"),
        };
        let checked = host.runtime().read(&item.target, "done").unwrap() == RtValue::Bool(true);
        parent.insert(id.clone(), under);
        done.insert(id, checked);
    }
    let finished = parent
        .keys()
        .filter(|id| {
            let below: Vec<&String> = parent
                .keys()
                .filter(|other| {
                    let mut at = parent.get(*other).cloned().flatten();
                    while let Some(step) = at {
                        if &step == *id {
                            return true;
                        }
                        at = parent.get(&step).cloned().flatten();
                    }
                    false
                })
                .collect();
            if below.is_empty() {
                done[*id]
            } else {
                below.iter().all(|other| done[*other])
            }
        })
        .count();
    assert!(finished > 0, "the saturday list has finished tasks");
    (workspace, finished)
}

/// A block an agent writes that runs Today's cleanup over the list and the notebook.
fn cleanup_call(workspace: &Workspace) -> String {
    let host = workspace.window.current_host().unwrap();
    let list = host.root_of("so.tilo.lang.tasks").unwrap().id_string();
    let notebook = host.root_of("so.tilo.lang.notes").unwrap().id_string();
    format!("today.morning_cleanup(@tasks.Tasks:{list}, @notes.Notebook:{notebook})")
}

/// The titles of the tasks the list holds, in the order they stand.
fn todo_titles(workspace: &Workspace) -> Vec<String> {
    let host = workspace.window.current_host().unwrap();
    let list = host.root_of("so.tilo.lang.tasks").unwrap();
    let RtValue::List(items) = host.runtime().read(&list, "items").unwrap() else {
        panic!("the list holds items");
    };
    items
        .into_iter()
        .map(|item| {
            let RtValue::Ref(item) = item else {
                panic!("a task is a resource: {item:?}")
            };
            rt_text(&host.runtime().read(&item.target, "title").unwrap())
        })
        .collect()
}

/// The tasks `before` held that the list no longer does: the finished branches and nothing
/// else. A task with steps is finished once its steps are, so the inbox task nobody checked but
/// whose one step is done goes with that step. The week also has a task done with a step still
/// open under it — a check from before steps were tasks of their own, or made while someone
/// else added that step — and clearing keeps the step, so that branch stays too.
fn assert_the_finished_tasks_went(workspace: &Workspace, before: &[String]) {
    let after = todo_titles(workspace);
    let went: Vec<String> = before
        .iter()
        .filter(|title| !after.contains(title))
        .cloned()
        .collect();
    assert_eq!(
        went,
        [
            "Take the bins out",
            "Buy oat milk and eggs",
            "Check what is in the fridge",
            "Pack the books",
            "Set up the redirects"
        ],
        "the finished branches went"
    );
    for title in ["Outline", "Collect the beta quotes"] {
        assert!(
            after.contains(&title.to_owned()),
            "{title}: the branch with open work under it stays, {after:?}"
        );
    }
}

#[test]
fn an_agent_runs_another_packages_action_and_reads_what_it_returned() {
    let (mut workspace, finished) = today_workspace();
    let before = todo_titles(&workspace);
    let source = cleanup_call(&workspace);
    let output = workspace
        .agent_invokes(HOST, "run-block", &serde_json::json!({ "source": source }))
        .unwrap()
        .value
        .unwrap();
    assert_eq!(output["state"], "done", "{output}");
    assert_eq!(output["result"], finished.to_string(), "{output}");
    assert_the_finished_tasks_went(&workspace, &before);
}

#[test]
fn an_action_an_agent_may_not_run_alone_waits_for_a_person_and_completes_once() {
    let (mut workspace, finished) = today_workspace();
    // The person narrowed this agent in the workspace: changes to the todo app ask first.
    let mut policy = policy::WorkspacePolicy::new("workspace", "ada");
    policy.grants.push(agent_answers::grant(
        "workspace",
        "agent-task:7",
        (
            &agent_answers::AnswerScope::app("so.tilo.lang.tasks"),
            policy::ActionKind::Undoable,
        ),
        policy::Answer::Ask,
        policy::Subject::person("ada"),
    ));
    let standing = policy::Standing::person(policy, "ada");
    workspace
        .window
        .instance
        .set_standing(Rc::new(move || Some(standing.clone())));
    let todos = workspace.items_of("so.tilo.lang.tasks", "items");
    let notes = workspace.items_of("so.tilo.lang.notes", "notes");
    let source = cleanup_call(&workspace);
    let waiting = workspace
        .agent_invokes(HOST, "run-block", &serde_json::json!({ "source": source }))
        .unwrap()
        .value
        .unwrap();
    assert_eq!(waiting["state"], "suggested", "{waiting}");
    assert_eq!(
        workspace.items_of("so.tilo.lang.tasks", "items"),
        todos,
        "nothing went while it waits"
    );
    // The clear asks; the note Today files for itself does not.
    let id = waiting["invocation"].as_str().unwrap().to_owned();
    let completed = person_decides(&mut workspace, "approve", &id).unwrap();
    assert_eq!(completed["state"], "done", "{completed}");
    assert_eq!(
        workspace.items_of("so.tilo.lang.tasks", "items"),
        todos - finished,
        "the finished ones went, once"
    );
    assert_eq!(
        workspace.items_of("so.tilo.lang.notes", "notes"),
        notes + 1,
        "and one note says so"
    );
    assert!(
        person_decides(&mut workspace, "approve", &id).is_err(),
        "it completes once"
    );
    workspace.settle();
    assert_eq!(
        workspace.items_of("so.tilo.lang.tasks", "items"),
        todos - finished
    );
    assert_eq!(workspace.items_of("so.tilo.lang.notes", "notes"), notes + 1);
}

#[test]
fn a_person_presses_morning_cleanup_and_the_finished_todos_go() {
    let (mut workspace, finished) = today_workspace();
    let before = todo_titles(&workspace);
    let todos = workspace.items_of("so.tilo.lang.tasks", "items");
    let notes = workspace.items_of("so.tilo.lang.notes", "notes");
    let morning = node_with(
        &workspace.composed(),
        "ui.Button",
        "label",
        "Morning cleanup",
    )
    .expect("Today's cleanup button")
    .key
    .clone();
    workspace
        .window
        .act(UiEvent::Fire(morning, "press".to_owned(), None));
    // The person answers whatever the run asks, as the ask bar has them.
    for _ in 0..8 {
        workspace.settle();
        let Some(_) = workspace.window.view().asks() else {
            break;
        };
        workspace.window.view().answer(Answer::Allow);
    }
    workspace.settle();
    assert_eq!(
        workspace.items_of("so.tilo.lang.tasks", "items"),
        todos - finished,
        "the finished ones went"
    );
    assert_the_finished_tasks_went(&workspace, &before);
    assert_eq!(
        workspace.items_of("so.tilo.lang.notes", "notes"),
        notes + 1,
        "and one note says so"
    );
}

#[test]
fn search_finds_chat_messages_and_a_hit_opens_their_channel() {
    let mut workspace = Workspace::seeded("chat");
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
    let before = format!("{:?}", workspace.window.instance().detail_tree());
    assert!(
        !before.contains("Coffee machine on the third floor is fixed."),
        "the scenario opens another channel"
    );
    workspace
        .window
        .open_requests()
        .borrow_mut()
        .push(tilolang_tilo::OpenEffect::Open { anchor });
    workspace.settle();
    // The messages are rows of the transcript: what the app rendered of them.
    let channel = workspace
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
fn a_plain_key_in_a_focused_mail_window_archives_the_conversation_it_shows() {
    let mut room = Workspace::staged("mail", "a busy inbox");
    // The first conversation's row, opened: its window shows the thread's actions, the archive
    // among them, each carrying the key the package declared (`key: "e"`).
    let (key, _) = room.rows().remove(0);
    let thread = room.item_of(&key);
    assert!(
        room.set_of(&thread, "boxes").iter().any(|b| b == "inbox"),
        "the conversation starts in the inbox"
    );
    room.activate(&key);
    room.settle();
    // A plain `e` reaches the archive the open conversation's window offers, and runs it.
    assert!(
        room.window.run_shortcut("e"),
        "the open conversation's window offers `e`"
    );
    room.settle();
    let boxes = room.set_of(&thread, "boxes");
    assert!(
        boxes.iter().any(|b| b == "archive") && !boxes.iter().any(|b| b == "inbox"),
        "the key archived the conversation: {boxes:?}"
    );
}

#[test]
fn search_entries_find_notes_and_open_the_window_at_the_note() {
    let mut workspace = Workspace::seeded("notes");
    let entries = crate::resource::search_entries();
    // Every text is indexed, so a note's words are found beside every other type's; the one
    // that names a note is its own.
    let first = entries
        .iter()
        .find(|entry| entry.group.as_deref() == Some("Notes"))
        .unwrap_or_else(|| panic!("no note among the hits: {entries:#?}"));
    let anchor = first.anchor.clone().expect("a hit names its resource");
    workspace
        .window
        .open_requests()
        .borrow_mut()
        .push(tilolang_tilo::OpenEffect::Open { anchor });
    workspace.settle();
    let host = workspace.window.current_host().unwrap();
    let note = workspace.opened_note();
    assert_eq!(
        tilolang_tilo::instance::title_of(&host, &note),
        first.title,
        "the hit showed the note it names in the page beside the tree"
    );
}

#[test]
fn an_action_the_content_shows_as_a_button_is_not_repeated_in_the_header() {
    let workspace = Workspace::seeded("counter");
    let host = workspace.window.current_host().unwrap();
    let target = workspace.window.instance().root_target().cloned().unwrap();
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
        header_labels(&workspace.composed(), "actions"),
        Vec::<String>::new(),
        "the counter's buttons are its content's, and a page searches nothing: {:#?}",
        workspace.composed()
    );
}

// An item's actions are on the item: the page shows the task's Done switch, and what can be
// done with the task, "Complete" among it, stands behind the "…" beside its title. The window's
// bar does not repeat them.
#[test]
fn a_control_bound_to_the_state_an_action_toggles_stands_for_that_state_beside_the_title() {
    let mut workspace = Workspace::seeded("tasks");
    let row = workspace
        .first_row_tree()
        .expect("the scenario's item is shown standing in its list");
    // The item's title and its "…" are the row's, shown where the item stands: what the item
    // can be done with stands in the row's own actions, not in the window's bar.
    let items: Vec<(&String, Option<bool>)> = {
        use tilolang_tilo::runtime::Value as Run;
        fn collect<'a>(
            node: &'a tilolang_tilo::runtime::UiNode,
            out: &mut Vec<(&'a String, Option<bool>)>,
        ) {
            if node.kind == "ui.MenuItem"
                && let Some(Run::Text(label)) = node.props.get("label")
            {
                let toggled = match node.props.get("checked") {
                    Some(Run::Bool(on)) => Some(*on),
                    Some(Run::Int(n)) => Some(*n != 0),
                    _ => None,
                };
                out.push((label, toggled));
            }
            for child in &node.children {
                collect(child, out);
            }
        }
        let mut out = Vec::new();
        collect(&row, &mut out);
        out
    };
    let toggled: Vec<Option<bool>> = items
        .iter()
        .filter(|(label, _)| *label == "Complete" || *label == "Reopen")
        .map(|(_, toggled)| *toggled)
        .collect();
    let commands: Vec<String> = items.iter().map(|(label, _)| (*label).clone()).collect();
    assert!(
        !toggled.is_empty() && toggled.iter().all(Option::is_some),
        "the row's \"…\" holds the item's actions, the one that toggles a state saying which way it stands: {toggled:?} of {commands:?}"
    );
    assert!(
        commands.iter().any(|label| label == "Delete"),
        "and the one that takes it away: {commands:?}"
    );
    let composed = workspace.composed();
    let header = header_labels(&composed, "overview_actions");
    assert!(
        !header
            .iter()
            .any(|label| label == "Complete" || label == "Reopen" || label == "Delete"),
        "the bar does not repeat the item's actions: {header:?}"
    );
    let menus: Vec<String> = workspace
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
    let workspace = Workspace::seeded("today");
    let tree = workspace.window.root_tree().unwrap();
    let mut cards = Vec::new();
    walk(&tree, &mut cards);
    assert_eq!(cards.len(), 3, "the todo list, the notebook and the chats");
    assert!(
        workspace.items_of("so.tilo.lang.tasks", "items") >= 12,
        "the list holds its scenario"
    );
    assert!(
        workspace.items_of("so.tilo.lang.notes", "notes") >= 5,
        "and the notebook its"
    );
}

#[test]
fn a_scenario_that_returns_a_resource_shows_it_beside_the_list() {
    let mut workspace = Workspace::seeded("mail");
    let shown = workspace
        .window
        .detail_target()
        .cloned()
        .expect("the scenario's thread is shown");
    let host = workspace.window.current_host().unwrap();
    let subject = tilolang_tilo::instance::title_of(&host, &shown);
    assert!(!subject.is_empty(), "the thread's subject names it");
    let selected: Vec<String> = workspace
        .list()
        .list()
        .unwrap()
        .selection()
        .stored_keys()
        .cloned()
        .collect();
    let row: Vec<String> = workspace
        .rows()
        .into_iter()
        .map(|(key, _)| key)
        .filter(|key| workspace.item_of(key) == shown)
        .collect();
    assert_eq!(selected, row, "and its row is the selected one");
}

#[test]
fn a_document_edits_its_title_as_its_large_title_and_the_shell_leaves_it_to_the_page() {
    let mut workspace = Workspace::seeded("notes");
    // The bar names where the open note stands, and shows it only once the page's heading
    // scrolls away: the note's own title is the page's.
    let tree = workspace.key_of("ui.TreeView", "name", "Notes");
    let inside = workspace.tree_row_note("Standups");
    workspace.fire(
        &tree,
        "select",
        Some(Value::Ref(tilolang_tilo::instance::wire::resource(&inside))),
    );
    let shell = workspace.composed();
    assert_eq!(
        shell.props.get("title"),
        Some(&Value::text("Standups")),
        "the bar names the note the page shows"
    );
    let title = first_input(&shell).expect("the editor");
    assert!(
        matches!(
            title.props.get("title"),
            Some(&Value::Int(1) | &Value::Bool(true))
        ),
        "the editor binds the title as its large title: {:?}",
        title.props.get("title")
    );
    assert_eq!(
        workspace.opened_note(),
        inside,
        "and the page edits the note the tree shows open"
    );
}

#[test]
fn the_list_renders_only_the_rows_it_shows() {
    let mut workspace = Workspace::seeded("mail");
    let rows = workspace.root_rows();
    // What the app renders: the rows of the list in its view's tree.
    let rendered = |workspace: &Workspace| -> Vec<String> {
        let tree = workspace.window.instance().root_tree().unwrap();
        let list = first_of(&tree, "ui.VirtualList").expect("the list").clone();
        list.children
            .iter()
            .filter(|row| row.kind != tilo_ui_host::adapter::SLOT && !row.children.is_empty())
            .map(|row| row.key.clone())
            .collect()
    };
    let len = rows.borrow().len();
    workspace.show_rows(0..3);
    let first = rendered(&workspace);
    assert!(
        !first.is_empty() && first.len() < len,
        "the list asked for the rows it shows, {} of {len}",
        first.len()
    );
    workspace.show_rows(len - 2..len);
    let shown: Vec<String> = (len - 2..len)
        .filter_map(|index| rows.borrow().key_at(index))
        .collect();
    assert_eq!(
        rendered(&workspace),
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
    let mut workspace = Workspace::seeded("handmade");
    let (key, _) = workspace.rows().remove(0);
    workspace.activate(&key);
    let input = first_input(&workspace.composed())
        .expect("the book's title")
        .key
        .clone();
    workspace
        .window
        .act(UiEvent::Edit(input, "Renamed".to_owned()));
    workspace.settle();
    let title = workspace
        .rows()
        .into_iter()
        .find(|(row, _)| *row == key)
        .map(|(_, title)| title);
    assert_eq!(title.as_deref(), Some("Renamed"));
}

#[test]
fn a_rows_checkbox_shows_whether_its_item_is_done_from_its_first_read() {
    let mut workspace = Workspace::staged("tasks", "a busy week");
    // A project's place holds what is done with beside what is not: the inbox holds only what
    // nothing sorts, and never what is done.
    let flat = workspace.key_of("ui.NavItem", "label", "Move flat");
    workspace.fire(&flat, "select", None);
    workspace.settle();
    let checked: Vec<bool> = workspace
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
fn content_scroll(workspace: &mut Workspace) -> &mut ui::widget::Scrollable {
    workspace
        .window
        .shell_mut()
        .and_then(|shell| shell.content_mut::<ui::widget::Scrollable>())
        .expect("the content column scrolls")
}

/// The widgets of the content column.
fn content_widgets(workspace: &mut Workspace) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    widget_identities(content_scroll(workspace), &mut out);
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
    let mut workspace = Workspace::seeded("today");
    let mut keys = Vec::new();
    cards(&workspace.composed(), &mut keys);
    let [todos, notes, chats] = &keys[..] else {
        panic!("the todo list, the notebook and the chats: {keys:?}");
    };
    let widgets_of = |workspace: &mut Workspace, key: &str| {
        let mut out = Vec::new();
        widget_identities(
            widget_with_id(content_scroll(workspace), key).expect("the card's content"),
            &mut out,
        );
        out
    };
    let chats_before = widgets_of(&mut workspace, chats);
    let notes_before = widgets_of(&mut workspace, notes);
    let todo_before = widgets_of(&mut workspace, todos);
    let outside_before: Vec<_> = content_widgets(&mut workspace)
        .into_iter()
        .filter(|widget| !todo_before.contains(widget))
        .collect();
    content_scroll(&mut workspace).take_structure_change();

    // Ticking a todo off changes what the todo card counts and previews, and nothing else.
    let host = workspace.window.current_host().unwrap();
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
    workspace.window.commit_planned();
    workspace.settle();
    assert_eq!(
        host.runtime().read(&open.target, "done").ok(),
        Some(RtValue::Bool(true)),
        "the todo is checked"
    );

    assert_eq!(
        widgets_of(&mut workspace, chats),
        chats_before,
        "the chats card is kept"
    );
    assert_eq!(
        widgets_of(&mut workspace, notes),
        notes_before,
        "the notes card is kept"
    );
    let after = content_widgets(&mut workspace);
    let gone: Vec<_> = outside_before
        .iter()
        .filter(|widget| !after.contains(widget))
        .collect();
    // The card itself is replaced; what holds it is kept.
    assert!(
        gone.iter().all(|(_, name)| name == "Card"),
        "only the todo card is replaced: {gone:?}"
    );
    assert!(widget_with_id(content_scroll(&mut workspace), todos).is_some());
    assert_eq!(
        content_scroll(&mut workspace).take_structure_change(),
        ui::widget::StructureChange::Described,
        "the frame's damage describes the card it replaced"
    );
}

#[test]
fn a_conversation_is_a_transcript_whose_messages_are_built_as_it_shows_them() {
    fn rendered(node: &tilolang_tilo::runtime::UiNode) -> usize {
        usize::from(node.kind == "ui.ListItem") + node.children.iter().map(rendered).sum::<usize>()
    }
    fn transcript(workspace: &mut Workspace) -> &mut DescribedList {
        let shell = workspace.window.shell_mut().expect("the shell");
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
    let mut workspace = Workspace::seeded("chat");
    let messages = |workspace: &mut Workspace| {
        transcript(workspace)
            .list()
            .map(|list| ui::widget::ListModel::len(list.model()))
    };
    let first = messages(&mut workspace).expect("the conversation's transcript");
    let before = std::ptr::from_ref(transcript(&mut workspace)).cast::<()>() as usize;
    // Enough messages that the transcript shows only the last of them.
    let sent = 80;
    for n in 0..sent {
        let (input, button) = composer(&workspace.composed());
        workspace
            .window
            .act(UiEvent::EditRich(input, typed(&format!("Message {n}"))));
        workspace.settle();
        workspace
            .window
            .act(UiEvent::Fire(button, "press".to_owned(), None));
        workspace.settle();
    }
    let total = first + sent;
    assert_eq!(
        messages(&mut workspace),
        Some(total),
        "each message is a row more"
    );
    assert_eq!(
        std::ptr::from_ref(transcript(&mut workspace)).cast::<()>() as usize,
        before,
        "of the same transcript"
    );
    let detail = workspace.window.instance().detail_tree().unwrap();
    assert!(
        rendered(&detail) < total / 2,
        "only the messages shown are rendered: {} of {total}",
        rendered(&detail)
    );
}

#[test]
fn a_transcript_names_each_run_of_one_authors_messages_once_and_marks_where_the_day_changes() {
    /// The day dividers, the avatars shown and the avatars' workspaces kept empty under `widget`.
    ///
    /// A divider marks a day unless it only says where reading starts again: where a day turns
    /// over at the reader's first unread line the two are one band that says both, and that band
    /// still says the day it opens.
    fn marks(widget: &dyn ui::widget::Widget, found: &mut (usize, usize, usize)) {
        match widget.name() {
            "SectionDivider" => {
                let day = widget
                    .as_any()
                    .and_then(|any| any.downcast_ref::<ui::widget::conversation::SectionDivider>())
                    .is_some_and(ui::widget::conversation::SectionDivider::names_a_day);
                if day {
                    found.0 += 1;
                }
            }
            "Avatar" if widget.accessibility().is_some() => found.1 += 1,
            "Avatar" => found.2 += 1,
            _ => {}
        }
        for child in widget.children() {
            marks(child, found);
        }
    }
    // The design channel: Mia twice, Leo, you, the next day Ada and Mia.
    let mut workspace = Workspace::seeded("chat");
    for _ in 0..3 {
        workspace.lay_out();
        workspace.settle();
    }
    let shell = workspace.window.shell_mut().expect("the shell");
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
        "a divider naming each of the two days — the first, where reading starts again, naming it with the line it stands over — an avatar for each of the five runs, and the workspace of one under the message that continues a run"
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
    let mut workspace = Workspace::seeded("handmade");
    assert!(
        workspace.shell().has_overview(),
        "the books are the overview"
    );
    let shell = workspace.composed();
    assert_eq!(
        shell.props.get("overview_title"),
        Some(&Value::text("Shelf"))
    );
    assert_eq!(
        header_labels(&shell, "overview_actions"),
        ["New book", "search"],
        "the header leads with the shelf's primary action, then the window's search"
    );
    let items = workspace.items();
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
        "the book's actions and the shelf's on it, in the order the app declares them"
    );
    let swipe = first.swipe.as_ref().expect("a swipe takes the book off");
    assert!(swipe.destructive);
    // The scenario opened its book beside the list, which selects it.
    let shown = workspace
        .window
        .detail_target()
        .cloned()
        .expect("the book is shown");
    assert_eq!(workspace.item_of(first_key), shown);
    let selected: Vec<String> = workspace
        .list()
        .list()
        .unwrap()
        .selection()
        .stored_keys()
        .cloned()
        .collect();
    assert_eq!(selected, std::slice::from_ref(first_key));
    // A destructive action from its menu offers Undo, as from any window.
    let list = workspace.list();
    assert!(list.list_mut().unwrap().reveal(first_key));
    let area = list.menu_area_mut().expect("the rows share one menu");
    assert!(area.open_at(ui::vello::kurbo::Point::ZERO, false));
    let remove = area
        .menu()
        .items()
        .iter()
        .position(|item| item.label() == Some("Remove"))
        .expect("the shelf's Remove is on the book's row");
    area.choose(remove);
    workspace.settle();
    assert_eq!(workspace.rows().len(), 1);
    assert!(
        workspace.shell().toast_text().is_some(),
        "the shell offers undo"
    );
    // And its primary action shows what it made.
    workspace.header_action("New book");
    let made = workspace
        .window
        .detail_target()
        .cloned()
        .expect("the new book opened");
    let host = workspace.window.current_host().unwrap();
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
    let mut workspace = Workspace::seeded("notes");
    let all = workspace.tree_labels();
    // The note the person chose stands forward: their own view state.
    let tree = workspace.key_of("ui.TreeView", "name", "Notes");
    let home = workspace.tree_row_note("Home");
    workspace.fire(
        &tree,
        "select",
        Some(Value::Ref(tilolang_tilo::instance::wire::resource(&home))),
    );
    assert_eq!(workspace.opened_note(), home, "the note the person chose");

    crate::updates::hand_over(later_notes(|source| {
        source.replace("\"Write something\"", "\"Write anything\"")
    }));
    workspace.settle();

    let composed = workspace.composed();
    assert!(
        shows_text(&composed, "Write anything"),
        "the new build renders the page: {composed:#?}"
    );
    assert_eq!(
        workspace.opened_note(),
        home,
        "the same note of the notebook is still shown"
    );
    assert_eq!(
        workspace.tree_labels(),
        all,
        "and the notebook still lists every note"
    );
}

#[test]
fn a_later_build_that_changes_a_field_type_starts_over_on_the_same_data() {
    let mut workspace = Workspace::seeded("notes");
    let notes = workspace.tree_labels().len();
    assert_eq!(workspace.opened_note(), workspace.tree_row_note("Q3 plan"));

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
    workspace.settle();

    let host = workspace.window.current_host().unwrap();
    let note = host
        .package("so.tilo.lang.notes")
        .and_then(|package| package.meta.types.into_iter().find(|ty| ty.name == "Note"))
        .unwrap();
    assert!(
        note.fields.iter().any(|field| field.ty == "Set<Int>"),
        "the new build runs: {note:?}"
    );
    assert_eq!(
        workspace.tree_labels().len(),
        notes,
        "every note is still there"
    );
    assert!(
        workspace.window.detail_target().is_none(),
        "the window started over"
    );
}

#[test]
fn a_list_that_asks_for_more_rows_shows_the_ones_it_has_until_they_come() {
    let mut workspace = Workspace::staged("mail", "a busy inbox");
    let rows = workspace.root_rows();
    let len = rows.borrow().len();
    workspace.show_rows(0..len);
    assert_eq!(
        rows.borrow().rendered().len(),
        len,
        "the list shows every row"
    );
    // The list asks for fewer rows (it has not measured them yet), then for all of them: until
    // the app answers, the rows it had stay.
    rows.borrow_mut().request_range(0..len / 2, None);
    assert_eq!(
        rows.borrow().rendered().len(),
        len,
        "a row is kept until the app forgot it"
    );
    workspace.frame();
    rows.borrow_mut().request_range(0..len, None);
    assert_eq!(
        rows.borrow().rendered().len(),
        len / 2,
        "what the app forgot is gone, nothing else"
    );
    workspace.frame();
    assert_eq!(rows.borrow().rendered().len(), len, "and every row is back");
}

#[test]
fn todays_cards_show_their_places_title_count_and_first_rows() {
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
    let mut workspace = Workspace::seeded("today");
    // What the window holds of the view, as the app sent it.
    let tree = workspace.window.root_tree().unwrap();
    let mut cards = Vec::new();
    walk(&tree, "ui.Card", &mut cards);
    // A card is another app's place: the tasks due today, the notes pinned, the conversations
    // with something unread, each named as its own app names it.
    let titled = |cards: &[&UiNode]| cards.iter().map(|c| text(c, "title")).collect::<Vec<_>>();
    assert_eq!(
        titled(&cards),
        ["Today", "Pinned", "Unread"],
        "a card is headed by the place its app names it"
    );
    let mut shown = Vec::new();
    for card in &cards {
        let Some(Value::Int(count)) = card.props.get("count") else {
            panic!("every card counts what it lists: {card:?}");
        };
        // The card shows its first rows and says how many it holds.
        let preview = usize::try_from(*count).unwrap().min(5);
        let mut rows = Vec::new();
        walk(card, "ui.ListRow", &mut rows);
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
        // A card whose place holds more than it shows opens the app for the rest.
        let mut buttons = Vec::new();
        walk(card, "ui.Button", &mut buttons);
        assert_eq!(
            buttons.is_empty(),
            preview == usize::try_from(*count).unwrap(),
            "a card shows the way into the rest: {}",
            text(card, "title")
        );
    }
    assert!(
        shown.iter().any(|title| title == "Buy oat milk and eggs"),
        "the tasks due today are the todo card's rows: {shown:?}"
    );
    assert!(
        shown.iter().any(|title| title == "general") && shown.iter().any(|title| title == "Leo"),
        "the conversations with something unread are the chat card's rows: {shown:?}"
    );
    // And the window draws them: a header and a row widget for each.
    let widgets = content_widgets(&mut workspace);
    let named = |name: &str| widgets.iter().filter(|(_, n)| n == name).count();
    assert_eq!(named("SectionHeader"), 3, "{widgets:?}");
    assert_eq!(named("ListRow"), shown.len(), "{widgets:?}");
}

#[test]
fn a_send_pressed_in_the_frame_of_the_last_keystroke_sends_it_too() {
    let mut workspace = Workspace::seeded("chat");
    let (input, send) = composer(&workspace.composed());
    workspace
        .window
        .act(UiEvent::EditRich(input.clone(), typed("See you at noo")));
    workspace.settle();
    // The last keystroke and the press go to the app in one batch, before the view runs again.
    workspace
        .window
        .act(UiEvent::EditRich(input, typed("See you at noon")));
    workspace
        .window
        .act(UiEvent::Fire(send, "press".to_owned(), None));
    workspace.settle();
    let host = workspace.window.current_host().expect("the host started");
    let channel = workspace
        .window
        .detail_target()
        .expect("the channel")
        .clone();
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

/// Every node the window composes is one the toolkit describes or the host
/// builds: the tree a seed puts in a fresh window goes through the same
/// validator the window applies to each batch an app sends, so a component the
/// catalogue lacks fails here rather than blanking the window.
#[test]
fn the_composed_tree_passes_the_window_s_validator() {
    use tilo_render_protocol::patch::validate::{PatchLimits, Validator};
    for app in ["chat", "notes", "tasks", "today"] {
        let room = Workspace::seeded(app);
        let tree = room.composed();
        let validator = Validator::new(tilo_ui_host::adapter::catalogue(), PatchLimits::default());
        assert!(
            validator.node(&tree, 0).is_ok(),
            "{app}: {}",
            validator
                .node(&tree, 0)
                .err()
                .map(|refused| refused.to_string())
                .unwrap_or_default()
        );
    }
    // And the validator does refuse an undescribed component: the check above is not vacuous.
    let mut probe = Workspace::seeded("chat").composed();
    probe.kind = "ui.NoSuchPart".to_owned();
    assert!(
        Validator::new(tilo_ui_host::adapter::catalogue(), PatchLimits::default())
            .node(&probe, 0)
            .is_err(),
        "an undescribed component is refused"
    );
}

// ----- the window's search ----------------------------------------------------------------

#[test]
fn typing_in_the_window_s_search_narrows_the_tasks_list_to_what_matches() {
    let mut workspace = Workspace::staged("tasks", "a busy week");
    let flat = workspace.key_of("ui.NavItem", "label", "Move flat");
    workspace.fire(&flat, "select", None);
    let before = workspace.rows();
    assert!(
        before.len() > 1,
        "the project's place lists its tasks: {before:?}"
    );
    let (mut driver, in_place) = workspace.window_search();
    assert_eq!(
        in_place,
        ui::widget::InPlace::Query,
        "the frame says where the search goes, so typing narrows the list in place"
    );
    workspace.type_in_search(&mut driver, "boxes");
    let after = workspace.rows();
    assert_eq!(
        after.len(),
        1,
        "only the task that says boxes is left: {after:?} of {before:?}"
    );
    assert!(
        after[0].1.contains("boxes"),
        "the row left is the one the search found: {after:?}"
    );
    assert_eq!(
        driver.count(),
        Some(ui::widget::FilterCount::new(1, before.len())),
        "the search says how many it left of how many the list held"
    );
    // Closing the search widens the list again.
    driver.drive(
        &mut workspace.window,
        "",
        ui::widget::FindOptions::default(),
        None,
        true,
    );
    workspace.settle();
    assert_eq!(
        workspace.rows().len(),
        before.len(),
        "the list widens back to the place's tasks"
    );
}

#[test]
fn find_in_an_open_note_counts_its_matches_and_replaces_them_all_in_one_change() {
    let mut workspace = Workspace::seeded("notes");
    let note = workspace.opened_note();
    let body = node_with(&workspace.composed(), "ui.BlockEditor", "", "")
        .expect("the note's body")
        .key
        .clone();
    // The note's body, as the person writing it leaves it: three occurrences of a word.
    workspace.window.act(UiEvent::EditRich(
        body,
        typed("Ship the boat. Ship the crew. Ship the cargo."),
    ));
    workspace.land_typed();
    let body_of = |host: &tilolang_tilo::host::Host| match host.runtime().read(&note, "body") {
        Ok(RtValue::Rich(rich)) => rich.written_text(),
        other => panic!("a note's body is rich text: {other:?}"),
    };
    assert_eq!(
        body_of(&workspace.window.current_host().expect("the host started")),
        "Ship the boat. Ship the crew. Ship the cargo."
    );
    let (mut driver, in_place) = workspace.window_search();
    assert_eq!(
        in_place,
        ui::widget::InPlace::Query,
        "the notebook's frame narrows its tree, and the note beside it is found in too"
    );
    let find = |workspace: &mut Workspace, driver: &mut ui::widget::SearchDriver, step| {
        driver.drive(
            &mut workspace.window,
            "Ship",
            ui::widget::FindOptions::default(),
            step,
            false,
        );
        workspace.settle();
    };
    find(&mut workspace, &mut driver, None);
    assert_eq!(
        driver.position(),
        Some((1, 3)),
        "the note holds three, the first is current"
    );
    find(&mut workspace, &mut driver, Some(true));
    assert_eq!(driver.position(), Some((2, 3)), "next");
    find(&mut workspace, &mut driver, Some(false));
    assert_eq!(driver.position(), Some((1, 3)), "previous");
    // Replacing them all is one change of the field: one step to undo, as the person's own
    // typing is.
    let heads = workspace.doc.get_heads();
    let touched = driver.replace(
        &mut workspace.window,
        "Ship",
        ui::widget::FindOptions::default(),
        "Launch",
        true,
    );
    assert!(touched, "the replacement was taken");
    workspace.land_typed();
    assert_eq!(
        body_of(&workspace.window.current_host().expect("the host started")),
        "Launch the boat. Launch the crew. Launch the cargo.",
        "every match replaced"
    );
    assert_eq!(
        workspace.doc.get_changes(&heads).len(),
        1,
        "one change: one step to undo"
    );
}

// `std.browse` narrows its list to the window's search, beside the place and the order it holds.
#[test]
fn typing_in_the_window_s_search_narrows_a_standard_list_window() {
    let mut workspace = Workspace::staged("tasks", "a busy week");
    // A project's tasks: the list the search narrows.
    let launch = workspace.key_of("ui.NavItem", "label", "Website launch");
    workspace.fire(&launch, "select", None);
    let before = workspace.rows();
    assert!(before.len() > 3, "the project lists its tasks: {before:?}");
    let (mut driver, in_place) = workspace.window_search();
    assert_eq!(
        in_place,
        ui::widget::InPlace::Query,
        "the window narrows its list"
    );
    workspace.type_in_search(&mut driver, "pricing");
    let after = workspace.rows();
    assert_eq!(after.len(), 1, "one task says it: {after:?}");
    assert!(
        after[0].1.contains("pricing"),
        "the task the search found: {after:?}"
    );
    // A task's notes are rich text: their words find it too.
    workspace.type_in_search(&mut driver, "offline");
    let body = workspace.rows();
    assert_eq!(
        body.len(),
        1,
        "the task whose notes say it: {body:?} of {before:?}"
    );
    assert!(body[0].1.contains("launch post"), "{body:?}");
}
