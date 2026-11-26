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

/// The Calendar package (`apps/tl/calendar`), and the Mail and Contacts packages it reads.
pub const CALENDAR: (&str, &str) = (
    include_str!("../../../../apps/tl/calendar/app.tl"),
    include_str!("../../../../apps/tl/calendar/app.ids"),
);

/// The packages a host loads to run Calendar.
pub const CALENDAR_SET: &[(&str, &str)] = &[CONTACTS, MAIL, CALENDAR];

/// The Feeds package (`apps/tl/feeds`): the subscriptions a person follows.
pub const FEEDS: (&str, &str) = (
    include_str!("../../../../apps/tl/feeds/app.tl"),
    include_str!("../../../../apps/tl/feeds/app.ids"),
);

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
        command: collab_types::store::wire::Command,
    ) -> collab_types::store::wire::Reply {
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
/// record, named in the order it was asked (`call-1`, `call-2`, ...), and no other call is sent
/// here, since every package is loaded as the host's own.
#[derive(Default)]
struct Dispatch(std::cell::Cell<usize>);

impl Invoker for Dispatch {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        match invocation {
            Invocation::Action { asks: true, .. } => {
                let asked = self.0.get().saturating_add(1);
                self.0.set(asked);
                Invoked::Pending(format!("call-{asked}"))
            }
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
        host.runtime().set_foreign_invoker(Dispatch::default());
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
        let policy = access().map(|access| access.policy);
        let host = Host::open(
            store,
            Actor::from(tilolang_tilo::runtime::ids::MemberId::person(name)),
            None,
            access,
        )
        .unwrap();
        if let Some(policy) = policy {
            stage_people(&host, &policy);
        }
        host.runtime().set_foreign_invoker(Dispatch::default());
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

    /// Run `app`'s action `action` with `args` as this replica's person, and what it answered.
    /// Panics when the action is not declared or does not finish.
    pub fn call(&self, app: &str, action: &str, args: Vec<Value>) -> Value {
        self.call_as(self.name.as_str(), app, action, args)
    }

    /// As [`Self::call`], with `who` acting.
    pub fn call_as(
        &self,
        who: &(impl AsActor + ?Sized),
        app: &str,
        action: &str,
        args: Vec<Value>,
    ) -> Value {
        let meta = self.host.package(app).expect("the app is loaded").meta;
        let id = action_id(&meta, action);
        match self
            .host
            .runtime()
            .invoke_action(&who.actor(), (id.as_str(), args))
        {
            Ok(tilolang_runtime::InvokeOutcome::Done(value)) => value,
            other => panic!("{action}: {other:?}"),
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
/// packages, the fleet's native ones beside it (the device's migrations, which its account rows
/// ask), and the fleet's own packages it `use`s compiled as `build` compiles them (Files' access
/// list, a template view whose text travels in the imported package's metadata and expands in
/// Settings' view).
pub fn settings_packages() -> Result<Vec<tilolang::PackageMeta>, String> {
    static PACKAGES: std::sync::OnceLock<Result<Vec<tilolang::PackageMeta>, String>> =
        std::sync::OnceLock::new();
    PACKAGES
        .get_or_init(|| {
            let fleet = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl");
            let mut published =
                tilolang_tilo::catalogue::published_packages(std::slice::from_ref(&fleet))?;
            let settings = fleet.join("settings");
            let dirs = tilolang::source::dependency_order_with(
                &settings,
                &[],
                &tilolang::source::apps_of(&published),
            )?;
            // The packages before Settings itself, dependencies first: its own source the caller
            // compiles.
            let used = &dirs[..dirs.len().saturating_sub(1)];
            let built = tilolang::source::compile_dirs(
                used,
                &published,
                &mut tilolang::OsEntropy::default(),
            )
            .map_err(|error| error.to_string())?;
            for package in built {
                let meta = package.compiled.meta;
                if !published.iter().any(|known| known.app == meta.app) {
                    published.push(meta);
                }
            }
            Ok(published)
        })
        .clone()
}

/// Hand `host` each of Settings' sections, empty: a host with no session publishes none of
/// them, so a pane that reads a record of one would wait for it forever. A test that exercises
/// a section sets its own reading over this one.
pub fn settings_sections(host: &Host) {
    for (published, ty) in tilolang_tilo::platform::platform_roots() {
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

/// The reading of the platform resource published at `published` (`settings/notifications`,
/// `session:people`), as the session hands it to a view.
pub fn set_reading(host: &Host, published: &str, reading: &serde_json::Value) {
    let (published, ty) = tilolang_tilo::platform::platform_roots()
        .into_iter()
        .find(|(path, _)| path == published)
        .unwrap_or_else(|| panic!("{published} is a root of the platform"));
    let target = ResourceRef::parse_id(&published, ty).expect("a resource id");
    host.runtime()
        .set_reading(&target, &reading.to_string())
        .expect("the reading is set");
}

/// What the view drew, kind by kind, for a failing assertion to read.
pub fn drawn(tree: &UiNode) -> String {
    let mut lines = Vec::new();
    drawn_lines(tree, &mut lines);
    lines.join("\n")
}

/// `node` and everything under it, one line each: its kind and the props it carries.
fn drawn_lines(node: &UiNode, lines: &mut Vec<String>) {
    let words: Vec<String> = node
        .props
        .iter()
        .filter_map(|(name, value)| match value {
            Value::Text(text) => Some(format!("{name}={text:?}")),
            Value::Bool(flag) => Some(format!("{name}={flag}")),
            _ => None,
        })
        .collect();
    lines.push(format!("{}{{{}}}", node.kind, words.join(" ")));
    for child in &node.children {
        drawn_lines(child, lines);
    }
}

/// The key of the control of `kind` whose label reads `label`, as a view lays it out.
pub fn labelled(tree: &UiNode, kind: &str, label: &str) -> String {
    tree.of_kind(kind)
        .into_iter()
        .find(|node| node.props.get("label") == Some(&Value::text(label)))
        .unwrap_or_else(|| panic!("a {kind} labelled {label}; the view drew {}", drawn(tree)))
        .key
        .clone()
}

/// The key of the first control of `kind` under a row titled `title`: a setting's control, by
/// the words beside it. Rows may share a title — the person's own rule and one app's are both
/// "Show banners" — so the first row that holds a control of `kind` answers.
pub fn control_under(tree: &UiNode, title: &str, kind: &str) -> String {
    fn find<'a>(node: &'a UiNode, kind: &str) -> Option<&'a UiNode> {
        if node.kind == kind {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, kind))
    }
    tree.of_kind("ui.ListRow")
        .into_iter()
        .filter(|row| row.props.get("title") == Some(&Value::text(title)))
        .find_map(|row| find(row, kind))
        .unwrap_or_else(|| {
            panic!(
                "a {kind} under a row titled {title}; the view drew {}",
                drawn(tree)
            )
        })
        .key
        .clone()
}

/// Stage the workspace `host` is in as having `members`: each person among them added through
/// the workspace's add-member action and anyone no longer among them (a person or an agent a
/// scenario named) removed through its remove-member action, as the host's viewer, and every
/// one of them with their role in the policy the host decides by, which decides by `reads` who
/// reads what (`None`: everyone reads everything).
pub fn stage_members(
    host: &Host,
    members: Vec<tilolang_runtime::members::Member>,
    reads: Option<Rc<tilolang_runtime::members::Reads>>,
) {
    for gone in host.runtime().members() {
        if !members.iter().any(|member| member.id == gone.id) {
            host.remove_workspace_member(&gone.who()).unwrap();
        }
    }
    for member in members.iter().filter(|member| member.id.is_person()) {
        host.add_workspace_member(&member.who()).unwrap();
    }
    host.take_member_policy(members, reads);
}

/// Stage the people `policy` names (its owner and its members) in the workspace `host` is in,
/// through the workspace's add-member action: the store here keeps no governance of its own, so
/// its members' resource is a plain record the test writes as the workspace would.
pub fn stage_people(host: &Host, policy: &tilo_action_policy::WorkspacePolicy) {
    for person in policy.owner.iter().chain(policy.members.keys()) {
        host.add_workspace_member(&tilolang_tilo::personal::actor(person))
            .expect("the workspace adds its people");
    }
}

/// What a window sent to other apps, in order: the invocations the shell would carry out, each
/// answered with nothing.
#[derive(Clone, Default)]
pub struct Sent(pub Rc<std::cell::RefCell<Vec<Invocation>>>);

impl Sent {
    fn matching(&self, action: &str) -> Vec<(String, Vec<(String, Value)>)> {
        self.0
            .borrow()
            .iter()
            .filter_map(|invocation| match invocation {
                Invocation::Action { name, args, .. }
                    if name == action || name == &action.replace('_', "-") =>
                {
                    Some((name.clone(), args.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// The arguments (after the resource) of the first `action` asked.
    pub fn asked(&self, action: &str) -> Vec<Value> {
        let (_, args) = self
            .matching(action)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("the app asks `{action}`; it asked {:?}", self.names()));
        args.into_iter().map(|(_, value)| value).collect()
    }

    /// The arguments (after the resource) of the last `action` asked.
    pub fn last_asked(&self, action: &str) -> Vec<Value> {
        let (_, args) = self
            .matching(action)
            .pop()
            .unwrap_or_else(|| panic!("the app asks `{action}`; it asked {:?}", self.names()));
        args.into_iter().map(|(_, value)| value).collect()
    }

    /// One argument of the first `action` asked, by its parameter name.
    pub fn arg(&self, action: &str, name: &str) -> Option<Value> {
        let (_, args) = self.matching(action).into_iter().next()?;
        args.into_iter()
            .find(|(arg, _)| arg == name)
            .map(|(_, value)| value)
    }

    /// The names of the actions asked, in order.
    pub fn names(&self) -> Vec<String> {
        self.0
            .borrow()
            .iter()
            .filter_map(|invocation| match invocation {
                Invocation::Action { name, .. } => Some(name.clone()),
                Invocation::Edit { .. } => None,
            })
            .collect()
    }
}

impl Invoker for Sent {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
}

/// An app that shows a window over what the device reads, on a host of its own: the platform
/// packages it `use`s loaded, its `window` view found, and what it asks of other apps recorded.
pub struct Pane {
    pub host: Host,
    pub sent: Sent,
    me: Actor,
    root: Option<ResourceRef>,
    view: String,
    actions: Vec<(String, String)>,
    roles: Vec<(String, Option<String>)>,
    views: Vec<(String, String)>,
    types: Vec<(String, String)>,
}

impl Pane {
    /// `source` loaded on a host that runs as `me`. Only the packages it `use`s: the whole
    /// platform carries the actions package the host already runs, and a host loads one package
    /// once.
    pub fn install(me: &Actor, source: &str) -> Self {
        let host = Host::new(me.clone(), || None).expect("a host");
        let metas = tilolang_tilo::platform::named_platform_packages(source)
            .expect("the platform's packages read");
        for meta in &metas {
            host.runtime()
                .load_foreign(meta.clone())
                .expect("the device is loaded");
        }
        let compiled = tilolang::compile_package(source, &tilolang::IdsLock::new(), &metas)
            .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
        let package = host.load_compiled(&compiled).expect("the app loads");
        let views: Vec<(String, String)> = package
            .meta
            .views
            .iter()
            .map(|view| (view.name.clone(), view.id.clone()))
            .collect();
        let types = package
            .meta
            .types
            .iter()
            .map(|ty| (ty.name.clone(), ty.id.clone()))
            .collect();
        let view = views
            .iter()
            .find(|(name, _)| name == "window")
            .map_or_else(String::new, |(_, id)| id.clone());
        let actions = package
            .meta
            .actions
            .iter()
            .map(|action| (action.name.clone(), action.id.clone()))
            .collect();
        let roles = package
            .meta
            .actions
            .iter()
            .map(|action| (action.name.clone(), action.role.clone()))
            .collect();
        let sent = Sent::default();
        host.runtime().set_foreign_invoker(sent.clone());
        Self {
            host,
            sent,
            roles,
            views,
            types,
            me: me.clone(),
            root: None,
            view,
            actions,
        }
    }

    /// What the app asks of other apps is run by the host's own resources, not carried out
    /// elsewhere: the runtime of a task's host, which holds no records of its own.
    pub fn delegating(self) -> Self {
        self.host.runtime().set_invoker(self.sent.clone());
        self
    }

    /// The device's reading at `resource`, as this JSON.
    pub fn reading(self, resource: &ResourceRef, json: &str) -> Self {
        self.host
            .runtime()
            .set_reading(resource, json)
            .expect("the reading is set");
        self
    }

    /// The window is the view `name` (the app's `window` view when this is not said).
    pub fn viewing(mut self, name: &str) -> Self {
        self.view = self
            .views
            .iter()
            .find(|(view, _)| view == name)
            .map_or_else(|| panic!("the {name} view"), |(_, id)| id.clone());
        self
    }

    /// The app's root, made from its type `name` (an id, or a name the app's own types give).
    pub fn rooted(mut self, name: &str) -> Self {
        let ty = self
            .types
            .iter()
            .find(|(ty, _)| ty == name)
            .map_or(name, |(_, id)| id.as_str());
        self.root = Some(self.host.runtime().create_root(ty).expect("the app's root"));
        self
    }

    /// The app's root, the one its application id names.
    pub fn rooted_of(mut self, app: &str) -> Self {
        self.root = Some(self.host.root_of(app).expect("the app's root"));
        self
    }

    /// The id the action `name` is stored and invoked by.
    pub fn action_id(&self, name: &str) -> &str {
        self.actions
            .iter()
            .find(|(asked, _)| asked == name)
            .map_or_else(|| panic!("no action {name}"), |(_, id)| id.as_str())
    }

    /// How the action `name` presents itself (`destructive`, `primary`, ...), where it says.
    pub fn role(&self, name: &str) -> Option<&str> {
        self.roles
            .iter()
            .find(|(asked, _)| asked == name)
            .and_then(|(_, role)| role.as_deref())
    }

    /// The device's reading at `target` changes to this JSON.
    pub fn device_says(&self, target: &ResourceRef, json: &str) {
        self.host
            .runtime()
            .set_reading(target, json)
            .expect("the reading is set");
    }

    pub fn root(&self) -> &ResourceRef {
        self.root.as_ref().expect("the app has a root")
    }

    pub fn root_value(&self) -> Value {
        Value::resource(self.root())
    }

    /// Run the action `name` as the person, answering what the runtime says when it refuses.
    pub fn try_call(&self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        let id = self
            .actions
            .iter()
            .find(|(asked, _)| asked == name)
            .map(|(_, id)| id.clone())
            .ok_or_else(|| format!("no action {name}"))?;
        match self
            .host
            .runtime()
            .invoke_action(&self.me, (id.as_str(), args))
            .map_err(|why| why.to_string())?
        {
            tilolang_runtime::InvokeOutcome::Done(value) => Ok(value),
            other => Err(format!("{name} answers {other:?}")),
        }
    }

    /// Run the action `name` as the person, which must answer.
    pub fn call(&self, name: &str, args: Vec<Value>) -> Value {
        self.try_call(name, args)
            .unwrap_or_else(|why| panic!("{name} runs: {why}"))
    }

    /// The window, opened over the root.
    pub fn window(&self) -> ViewInstance {
        self.host
            .runtime()
            .open_view(&self.me, &self.view, vec![self.root_value()])
            .expect("the window opens")
    }
}

/// The Settings app as it ships, on a host that runs as Ada with the platform's packages it
/// reads through loaded, as a window's process loads them.
pub fn settings_host() -> (Host, tilolang::Compiled) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/settings");
    let source = tilolang::PackageSource::read_dir(&dir).expect("the package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the package's ids"),
    )
    .expect("its ids parse");
    let metas = settings_packages().expect("the platform's packages read");
    let compiled =
        tilolang::compile_package_source(&source, &ids, &metas).expect("the package compiles");
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .expect("a host");
    host.load_compiled(&compiled).expect("the package loads");
    tilolang_tilo::platform::load(&host, &compiled.meta.uses);
    (host, compiled)
}
