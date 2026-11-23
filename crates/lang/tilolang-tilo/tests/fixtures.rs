//! Packages compiled from source, and replicas of one workspace that run them.

use std::rc::Rc;

use tilolang_runtime::meta::Meta;
use tilolang_runtime::value::Value;
use tilolang_runtime::{Actor, ResourceRef, UiNode, ViewInstance};
use tilolang_tilo::action::DynAction;
use tilolang_tilo::host::{Host, Package};
use tilolang_tilo::publish::PlanRequest;
use tilolang_tilo::resources::{LocalPort, ResourceStore, Watch};

/// The counter package (`apps/tl/counter`): the first package of the host.
pub const COUNTER: &str = include_str!("../../../../apps/tl/counter/app.tl");

/// The Tasks package (`apps/tl/tasks`): its app and the scenarios staged beside it.
pub const TASKS_SOURCE: &str = concat!(
    include_str!("../../../../apps/tl/tasks/app.tl"),
    "\n",
    include_str!("../../../../apps/tl/tasks/scenarios.tl")
);

/// A list with text, a collection and a set, for the merge kinds the counter does not use.
pub const TODO: &str = r#"app so.tilo.todo "Todo"

type Item {
  text: Text merge text
  done: Bool
}

type TodoList {
  title: Text merge text
  items: Collection<Item>
  tags: Set<Text>
}

action add(l: TodoList, text: Text) { l.items.insert(Item(text: text, done: false)) }

action move_first(l: TodoList, i: Item) {
  if let first = l.items.first() { l.items.move(i, before: first) }
}

action place(l: TodoList, i: Item, at: Int) { l.items.move(i, to: at) }

action tag(l: TodoList, t: Text) { l.tags.add(t) }

action finish(i: Item) { i.done = true }

root TodoList

default view main for TodoList

view main(l: TodoList) {
  page {
    input(bind: l.title, placeholder: "Title")
    for i in l.items {
      text(i.text)
    }
  }
}
"#;

/// Who acts in a test: a person by their id, or any actor.
pub trait AsActor {
    fn actor(&self) -> Actor;
}

impl AsActor for str {
    fn actor(&self) -> Actor {
        tilolang_runtime::ids::MemberId::person(self).into()
    }
}

impl AsActor for Actor {
    fn actor(&self) -> Actor {
        self.clone()
    }
}

/// One member's machine's resources: the store a replica's host writes into. A clone is a copy
/// of it, as a fork of the workspace on another device is: what one writes after reaches the
/// other only by [`Replica::sync_with`].
pub struct Workspace(Rc<LocalPort>);

impl Workspace {
    /// An empty workspace.
    #[must_use]
    pub fn new() -> Self {
        Self(Rc::new(LocalPort::in_memory()))
    }

    /// A copy of every document this workspace holds, as another device holds it.
    #[must_use]
    pub fn fork(&self) -> Self {
        let copy = Self::new();
        copy.take_in(self);
        copy
    }

    /// Take in every document `other` holds, as sync does: each merged into this one's own.
    /// Whether anything changed here.
    pub fn take_in(&self, other: &Self) -> bool {
        self.take_in_as(other, None)
    }

    /// As [`Self::take_in`], on a device of `person` when one is named: a document sealed to
    /// readers they are not among reaches it, but its keys do not, so it opens none of it.
    pub fn take_in_as(&self, other: &Self, person: Option<&str>) -> bool {
        // A document opens by the reader set its id names (a record's content names it too):
        // every member's, or one of the sets the person is among.
        let theirs: std::collections::BTreeSet<String> = other
            .0
            .documents()
            .iter()
            .filter_map(|doc| other.0.readers(doc))
            .filter(|readers| {
                person.is_some_and(|person| readers.split(',').any(|reader| reader == person))
            })
            .map(|readers| collab_types::reader_set_name(&readers))
            .collect();
        let opens = |doc: &str| {
            person.is_none()
                || collab_types::store_document_readers(doc).is_none_or(|named| {
                    named == collab_types::EVERY_MEMBER || theirs.contains(named)
                })
        };
        let mut changed = false;
        for doc in other.0.documents().into_iter().filter(|doc| opens(doc)) {
            if let Some(bytes) = other.0.save(&doc) {
                changed |= self.0.merge(&doc, &bytes).unwrap();
            }
        }
        changed
    }

    /// The store answering `command` as this device's person reads it.
    pub fn command(
        &self,
        command: tilo_resource_store::wire::Command,
    ) -> tilo_resource_store::wire::Reply {
        tilolang_tilo::resources::Port::command(&*self.0, command).unwrap()
    }

    /// The store over this workspace, as a host opens it.
    #[must_use]
    pub fn store(&self) -> ResourceStore {
        ResourceStore::new(Rc::clone(&self.0) as Rc<dyn tilolang_tilo::resources::Port>)
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for Workspace {
    fn clone(&self) -> Self {
        self.fork()
    }
}

/// One member's machine: a copy of the workspace and a host over it.
pub struct Replica {
    pub name: String,
    pub doc: Workspace,
    pub host: Host,
    /// What moved in the store since [`Self::commit`] last looked.
    watch: Watch,
}

impl Replica {
    /// A replica of `doc` for `name`, with `packages` (source and ids lock) loaded.
    pub fn new(name: &str, doc: Workspace, packages: &[(&str, &str)]) -> (Self, Vec<Package>) {
        Self::access(name, doc, packages, None)
    }

    /// As [`Self::new`], deciding what the runtime invokes by `access`, the person's access
    /// in the workspace; without one, the API decides the person's own part.
    pub fn access(
        name: &str,
        doc: Workspace,
        packages: &[(&str, &str)],
        access: Option<tilo_action_policy::MemberAccess>,
    ) -> (Self, Vec<Package>) {
        Self::following(name, doc, packages, move || access.clone())
    }

    /// As [`Self::access`], deciding by what `access` answers when the host asks: the access the
    /// person has now, which a test may change.
    pub fn following(
        name: &str,
        doc: Workspace,
        packages: &[(&str, &str)],
        access: impl Fn() -> Option<tilo_action_policy::MemberAccess> + 'static,
    ) -> (Self, Vec<Package>) {
        let store = doc.store();
        let watch = store.watch();
        let host = Host::open(
            store,
            Actor::from(tilolang_tilo::runtime::ids::MemberId::person(name)),
            None,
            access,
        )
        .unwrap();
        let loaded = packages
            .iter()
            .map(|(source, ids)| host.load_builtin(source, ids).unwrap())
            .collect();
        let _ = watch.take();
        (
            Self {
                name: name.to_owned(),
                doc,
                host,
                watch,
            },
            loaded,
        )
    }

    /// A second replica of this one's workspace, for `name`, running the same packages.
    pub fn fork(&self, name: &str, packages: &[(&str, &str)]) -> Self {
        Self::new(name, self.doc.fork(), packages).0
    }

    /// Carry out the action `request` plans, as this replica's person: what the API does when
    /// it is invoked. The action it ran.
    pub fn carry_out(&self, request: &PlanRequest<'_>) -> Result<DynAction, String> {
        let actor = self.host.viewer().clone();
        let host = &self.host;
        let action = host.carrying_out(&actor, || host.plan(request))?;
        if let Some(refused) = host.store().take_refusal() {
            return Err(refused);
        }
        Ok(action)
    }

    /// How many documents the runtime wrote in since the last call: its writes go to the store
    /// as it makes them, so this only says what they were. A document whose resources only
    /// heard that something now links to them was not written in.
    pub fn commit(&self) -> usize {
        if let Some(refused) = self.host.store().take_refusal() {
            panic!("the store refused a write: {refused}");
        }
        let moved = self.watch.take();
        moved
            .docs
            .iter()
            .filter(|doc| {
                moved.fields.get(*doc).is_none_or(|fields| {
                    fields
                        .iter()
                        .any(|(_, field)| field != tilolang_runtime::store::LINKS)
                })
            })
            .count()
    }

    /// Exchange every change with `other`, as sync does.
    pub fn sync_with(&self, other: &Self) {
        self.send_to(other);
        other.send_to(self);
    }

    /// Hand this replica's changes to `other`, as a sync does from one side: the order two
    /// replicas receive each other's changes in is theirs, so what they read must not depend
    /// on it.
    pub fn send_to(&self, other: &Self) {
        if other.doc.take_in_as(&self.doc, Some(&other.name)) {
            other.host.store().moved();
            let _ = other.watch.take();
        }
    }

    /// Open the `main` view of `app`.
    pub fn view(&self, app: &str) -> ViewInstance {
        self.host.open_main(app).unwrap()
    }

    /// The root resource of `app`.
    pub fn root(&self, app: &str) -> ResourceRef {
        self.host.root_of(app).unwrap()
    }

    /// The field `field` (by name or id) of `at` as the runtime reads it now.
    pub fn read(&self, at: &ResourceRef, field: &str) -> Value {
        self.host.runtime().read(at, field).unwrap()
    }

    /// The service a session keeps in the workspace over this replica's host, for `person`, with
    /// the workspace still reachable as the other devices' sync takes it in.
    pub fn into_service_with_workspace(
        self,
        person: &str,
    ) -> (tilolang_tilo::resource_service::ResourceService, Workspace) {
        let Self { host, doc, .. } = self;
        (
            tilolang_tilo::resource_service::ResourceService::new(host, person),
            doc,
        )
    }

    /// The service a session keeps in the workspace over this replica's host, for `person`.
    pub fn into_service(self, person: &str) -> tilolang_tilo::resource_service::ResourceService {
        let Self { host, .. } = self;
        tilolang_tilo::resource_service::ResourceService::new(host, person)
    }
}

impl Replica {
    /// A replica of `doc` for `name` whose host `host` runs, as a test that builds the host
    /// itself does.
    pub fn with_host(name: &str, doc: Workspace, host: Host) -> Self {
        let watch = host.store().watch();
        let _ = watch.take();
        Self {
            name: name.to_owned(),
            doc,
            host,
            watch,
        }
    }

    /// Carry out the action `action_id` of `app` on `resource` (when the action has one) as this
    /// replica's person, with `args` by parameter name, made from `from` when one is given.
    pub fn carry(
        &self,
        app: &str,
        kind: &str,
        action_id: &str,
        resource: Option<&str>,
        args: &serde_json::Map<String, serde_json::Value>,
        caller: Option<&tilolang_tilo::publish::Caller>,
        from: Option<&ResourceRef>,
    ) -> Result<DynAction, String> {
        let request = PlanRequest {
            app,
            kind,
            action_id,
            resource,
            args,
            caller,
            from,
        };
        self.carry_out(&request)
    }

    /// Set the field `field` (a field id) of `at` to `value`, as the control bound to it in the
    /// resource's own view does when the person edits it.
    pub fn edit(&self, at: &ResourceRef, field: &str, value: &Value) {
        let view = self.host.open_resource(at, None).unwrap();
        let key = bound_key(&view, field);
        view.edit(&key, value).unwrap();
    }

    /// The versions of the resource `at`, oldest first, as the store keeps them.
    pub fn versions(&self, at: &ResourceRef) -> Vec<tilolang_runtime::store::Version> {
        use tilolang_runtime::store::Store as _;
        self.host.store().versions(at).unwrap()
    }

    /// Take back the last change written here, as the window's undo does: as the person at it.
    pub fn undo_last(&self) -> bool {
        self.host
            .store()
            .undo_last(Some(&tilolang_tilo::personal::actor(&self.name)))
            .unwrap()
    }

    /// Make again the last change taken back here, as the window's redo does.
    pub fn redo_last(&self) -> bool {
        self.host
            .store()
            .redo_last(Some(&tilolang_tilo::personal::actor(&self.name)))
            .unwrap()
    }
}

/// A place as a sidebar draws it, the shape its tests read: its label, and the count the
/// sidebar itself works out for the places it counts.
pub type ShownPlace = (String, Option<i64>);

/// A heading of a sidebar with the places under it, in the order they stand.
pub type ShownSection = (String, Vec<ShownPlace>);

/// The places a sidebar node holds, in the order the tree stands them: each place's label and
/// the count it shows, where its filter asks for one.
pub fn places_in(node: &UiNode) -> Vec<ShownPlace> {
    nodes(node)
        .into_iter()
        .filter(|node| node.kind == "ui.NavItem")
        .filter_map(|node| match node.props.get("label") {
            Some(Value::Text(label)) => {
                let count = match node.props.get("count") {
                    Some(Value::Int(count)) => Some(*count),
                    _ => None,
                };
                Some((label.clone(), count))
            }
            _ => None,
        })
        .collect()
}

/// The headings of a sidebar with their places, in the order the tree stands them.
pub fn sections(node: &UiNode) -> Vec<ShownSection> {
    node.of_kind("ui.NavSection")
        .into_iter()
        .map(|section| {
            let title = match section.props.get("title") {
                Some(Value::Text(title)) => title.clone(),
                other => panic!("a section is headed: {other:?}"),
            };
            (title, places_in(section))
        })
        .collect()
}

/// Every node of a drawn tree, depth first, `node` itself included: what a test reads the
/// drawn view through, whether it looks for a kind, a prop or every text.
pub fn nodes(node: &UiNode) -> Vec<&UiNode> {
    let mut out = Vec::new();
    let mut at = vec![node];
    while let Some(node) = at.pop() {
        out.push(node);
        at.extend(node.children.iter().rev());
    }
    out
}

/// The key of the control in `view` bound to the field `field` (a field id).
pub fn bound_key(view: &ViewInstance, field: &str) -> String {
    fn find(node: &tilolang_runtime::UiNode, field: &str) -> Option<String> {
        if node
            .binding
            .as_ref()
            .and_then(|binding| binding.field())
            .is_some_and(|(_, bound)| bound == field)
        {
            return Some(node.key.clone());
        }
        node.children.iter().find_map(|child| find(child, field))
    }
    find(view.tree(), field).unwrap_or_else(|| panic!("the view binds {field}"))
}

/// The resource `value` refers to.
pub fn target(value: &Value) -> tilolang_runtime::ResourceRef {
    match value {
        Value::Ref(r) => (*r.target).clone(),
        other => panic!("a resource: {other:?}"),
    }
}

/// The id the action `name` of `meta` is stored and planned by.
pub fn action_id(meta: &Meta, name: &str) -> String {
    meta.actions
        .iter()
        .find(|action| action.name == name)
        .unwrap_or_else(|| panic!("the action `{name}`"))
        .id
        .clone()
}

/// The id the type `ty` of `meta` is stored by.
pub fn type_id(meta: &Meta, ty: &str) -> String {
    meta.types
        .iter()
        .find(|declared| declared.name == ty)
        .map(|declared| declared.id.clone())
        .unwrap()
}

pub fn field_id(meta: &Meta, ty: &str, field: &str) -> String {
    meta.types
        .iter()
        .find(|declared| declared.name == ty)
        .and_then(|declared| declared.fields.iter().find(|f| f.name == field))
        .map(|f| f.id.clone())
        .unwrap()
}

/// The key of the button that adds: the counter's `Increment`, else the first button of a view.
pub fn button_key(view: &ViewInstance) -> String {
    fn plus(node: &tilolang_runtime::UiNode) -> Option<String> {
        if node.kind == "ui.Button"
            && node.props.get("label") == Some(&tilolang_runtime::Value::text("Increment"))
        {
            return Some(node.key.clone());
        }
        node.children.iter().find_map(plus)
    }
    fn find(node: &tilolang_runtime::UiNode) -> Option<String> {
        if node.kind == "ui.Button" {
            return Some(node.key.clone());
        }
        node.children.iter().find_map(find)
    }
    plus(view.tree())
        .or_else(|| find(view.tree()))
        .expect("the view has a button")
}
