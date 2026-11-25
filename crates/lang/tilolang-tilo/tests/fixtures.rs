//! Packages compiled from source, and replicas of one workspace that run them.

use std::rc::Rc;

use tilo_resource_store::LocalPort;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::meta::Meta;
use tilolang_runtime::value::Value;
use tilolang_runtime::{Actor, ResourceRef, UiNode, ViewInstance};
use tilolang_tilo::action::DynAction;
use tilolang_tilo::host::{Host, Package};
use tilolang_tilo::publish::PlanRequest;
use tilolang_tilo::resources::{ResourceStore, Watch};

/// The counter package (`apps/tl/counter`): the first package of the host.
pub const COUNTER: &str = include_str!("../../../../apps/tl/counter/app.tl");

/// The Tasks package (`apps/tl/tasks`): its app and the scenarios staged beside it.
pub const TASKS_SOURCE: &str = concat!(
    include_str!("../../../../apps/tl/tasks/app.tl"),
    "\n",
    include_str!("../../../../apps/tl/tasks/scenarios.tl")
);

/// The Discussion package (`apps/tl/discussion`): the threads under a task and a note.
pub const DISCUSSION: (&str, &str) = (
    include_str!("../../../../apps/tl/discussion/app.tl"),
    include_str!("../../../../apps/tl/discussion/app.ids"),
);

/// The Contacts package (`apps/tl/contacts`): its book and the directory's account panel.
pub const CONTACTS: (&str, &str) = (
    concat!(
        include_str!("../../../../apps/tl/contacts/app.tl"),
        "\n",
        include_str!("../../../../apps/tl/contacts/identities.tl")
    ),
    include_str!("../../../../apps/tl/contacts/app.ids"),
);

/// The Notes package (`apps/tl/notes`).
pub const NOTES: (&str, &str) = (
    include_str!("../../../../apps/tl/notes/app.tl"),
    include_str!("../../../../apps/tl/notes/app.ids"),
);

/// The Tasks package, its scenarios included.
pub const TASKS: (&str, &str) = (
    TASKS_SOURCE,
    include_str!("../../../../apps/tl/tasks/app.ids"),
);

/// The packages a host loads to run Tasks: the apps it names with `use`, each before it, as a
/// package compiles against the ones loaded before it.
pub const TASKS_SET: &[(&str, &str)] = &[CONTACTS, DISCUSSION, NOTES, TASKS];

/// The Weather package (`apps/tl/weather`), and the places it follows.
pub const WEATHER: (&str, &str) = (
    include_str!("../../../../apps/tl/weather/app.tl"),
    include_str!("../../../../apps/tl/weather/app.ids"),
);

/// The Mail package (`apps/tl/mail`), and the Contacts package it reads an address as.
pub const MAIL: (&str, &str) = (
    include_str!("../../../../apps/tl/mail/app.tl"),
    include_str!("../../../../apps/tl/mail/app.ids"),
);

/// The packages a host loads to run Mail.
pub const MAIL_SET: &[(&str, &str)] = &[CONTACTS, MAIL];

/// The Chat package (`apps/tl/chat`): its own app, with no service's records in it.
pub const CHAT: (&str, &str) = (
    include_str!("../../../../apps/tl/chat/app.tl"),
    include_str!("../../../../apps/tl/chat/app.ids"),
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
        self.take_in_as(other, None, None)
    }

    /// As [`Self::take_in`], on a device of `person` when one is named, whose role in the
    /// workspace is `role`: a document sealed to readers they are not among reaches it, but its
    /// keys do not, so it opens none of it. A set naming a role is theirs while their role
    /// includes it, as its key group follows the workspace's governance.
    pub fn take_in_as(
        &self,
        other: &Self,
        person: Option<&str>,
        role: Option<tilo_action_policy::Role>,
    ) -> bool {
        // A document opens by the reader set its id names (a record's content names it too):
        // every member's, or one of the sets the person is among.
        let among = |readers: &str, person: &str| {
            let (named, people) = match readers.strip_prefix(collab_types::ROLE_READERS) {
                Some(rest) => {
                    let (named, people) = rest.split_once(',').unwrap_or((rest, ""));
                    (tilo_action_policy::Role::parse(named), people)
                }
                None => (None, readers),
            };
            people.split(',').any(|reader| reader == person)
                || named
                    .zip(role)
                    .is_some_and(|(named, held)| held.includes(named))
        };
        let theirs: std::collections::BTreeSet<String> = other
            .0
            .documents()
            .iter()
            .filter_map(|doc| other.0.readers(doc))
            .filter(|readers| person.is_some_and(|person| among(readers, person)))
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

    /// Take in the documents `docs` of `other`, as a share of those records between two
    /// workspaces does: nothing else of `other`'s travels. Whether anything changed here.
    pub fn take_in_docs(&self, other: &Self, docs: &std::collections::BTreeSet<String>) -> bool {
        let mut changed = false;
        for doc in other
            .0
            .documents()
            .into_iter()
            .filter(|doc| docs.contains(doc))
        {
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
        ResourceStore::new(Rc::clone(&self.0) as Rc<dyn tilo_resource_store::Port>)
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

/// The host's dispatch in these tests: a call that needs a person's yes waits as its own
/// record, and no other call is sent here, since every package is loaded as the host's own.
struct Dispatch;

impl Invoker for Dispatch {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        match invocation {
            Invocation::Action { asks: true, .. } => Invoked::Pending("call".to_owned()),
            _ => Invoked::Refused("this test host runs no call unasked".to_owned()),
        }
    }
}

/// One member's machine: a copy of the workspace and a host over it.
pub struct Replica {
    pub name: String,
    /// The person's role in the workspace, which decides the reader sets naming a role that
    /// this replica opens: none until a test gives one ([`Self::hold`]).
    role: std::cell::Cell<Option<tilo_action_policy::Role>>,
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

    /// A replica of `doc` for `name` running as the device `device`: the device whose roots a
    /// package keeps one of its own for (`device root readings: Readings`) it reads and writes.
    pub fn on_device(
        name: &str,
        device: &str,
        doc: Workspace,
        packages: &[(&str, &str)],
    ) -> (Self, Vec<Package>) {
        let store = doc.store();
        let watch = store.watch();
        let host = Host::open(
            store,
            Actor::from(tilolang_tilo::runtime::ids::MemberId::person(name)),
            Some(device),
            || None,
        )
        .unwrap();
        host.runtime().set_foreign_invoker(Dispatch);
        let loaded = packages
            .iter()
            .map(|(source, ids)| host.load_builtin(source, ids).unwrap())
            .collect();
        let _ = watch.take();
        (
            Self {
                name: name.to_owned(),
                role: std::cell::Cell::new(None),
                doc,
                host,
                watch,
            },
            loaded,
        )
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
        host.runtime().set_foreign_invoker(Dispatch);
        let loaded = packages
            .iter()
            .map(|(source, ids)| host.load_builtin(source, ids).unwrap())
            .collect();
        let _ = watch.take();
        (
            Self {
                name: name.to_owned(),
                role: std::cell::Cell::new(None),
                doc,
                host,
                watch,
            },
            loaded,
        )
    }

    /// Give this replica's person `role` in the workspace, as its governance would: from now on
    /// it opens what a reader set naming a role its role includes is sealed to.
    pub fn hold(&self, role: tilo_action_policy::Role) {
        self.role.set(Some(role));
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
        if other
            .doc
            .take_in_as(&self.doc, Some(&other.name), other.role.get())
        {
            other.host.store().moved();
            let _ = other.watch.take();
        }
    }

    /// Hand this replica's documents `docs` to `other`, as a share of those records between two
    /// workspaces does: nothing else of this one's travels.
    pub fn send_docs_to(&self, other: &Self, docs: &std::collections::BTreeSet<String>) {
        if other.doc.take_in_docs(&self.doc, docs) {
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
}

impl Replica {
    /// A replica of `doc` for `name` whose host `host` runs, as a test that builds the host
    /// itself does.
    pub fn with_host(name: &str, doc: Workspace, host: Host) -> Self {
        let watch = host.store().watch();
        let _ = watch.take();
        Self {
            name: name.to_owned(),
            role: std::cell::Cell::new(None),
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

/// The documents a native app wrote, as a `legacy/*.hex` fixture keeps them: one document a line,
/// its document id, a tab, and the saved Automerge document as hex. Each fixture was written by
/// the native app's own code (`legacy/README.md` says how), never by hand.
pub fn native_documents(fixture: &str) -> Vec<(String, tilo_resource::Doc)> {
    fixture
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (id, hex) = line.split_once('\t').expect("an id and the document");
            let bytes: Vec<u8> = hex
                .as_bytes()
                .chunks(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect();
            (id.to_owned(), tilo_resource::Doc::load(&bytes).unwrap())
        })
        .collect()
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

/// Run the action named `action` of `app` as `who`, with `args`, and answer what it returned.
pub fn call(
    r: &Replica,
    who: &(impl AsActor + ?Sized),
    app: &str,
    action: &str,
    args: Vec<Value>,
) -> Value {
    let meta = r.host.package(app).unwrap().meta;
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("no action {action}"))
        .id
        .clone();
    match r
        .host
        .runtime()
        .invoke_action(&who.actor(), (id.as_str(), args))
        .unwrap_or_else(|error| panic!("{app} {action}: {error}"))
    {
        tilolang_runtime::InvokeOutcome::Done(v) => v,
        other => panic!("{action}: {other:?}"),
    }
}

/// What Settings compiles against, as `tilolang check` and `build` find it: the platform's
/// packages and the fleet's native ones beside it (the device's migrations, which its account
/// rows ask).
pub fn settings_packages() -> Result<Vec<tilolang::PackageMeta>, String> {
    let fleet = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl");
    tilolang_tilo::catalogue::published_packages(&[fleet])
}

/// Hand `host` each of Settings' sections, empty: a host with no session publishes none of
/// them, so a pane that reads a record of one would wait for it forever. A test that exercises
/// a section sets its own reading over this one.
pub fn settings_sections(host: &Host) {
    for (published, ty) in tilolang_tilo::platform_apps::platform_roots() {
        if !(published.starts_with("settings/") || published.contains("/settings/")) {
            continue;
        }
        let Some(root) = ResourceRef::parse_id(&published, ty.as_str()) else {
            continue;
        };
        if let Err(why) = host.runtime().set_reading(&root, "{}") {
            panic!("{published} is read: {why}");
        }
    }
}
