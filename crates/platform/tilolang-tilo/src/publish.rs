//! Publishing tilolang resources on the action door.
//!
//! Every resource in the room record is published as a `tilo_actions::ResourceSpec` of kind
//! `tl.<app>.<Type>`, with one typed action per tilolang action whose subject is its type. The
//! desktop agent, the runtime door, shortcuts and grants therefore reach tilolang actions with
//! no agent-side code. The build closure of each action runs the wasm **planner** and encodes
//! the resulting [`DynAction`].

use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value as Json;
use tilo_actions::resource::INVALID_ARGUMENTS_TYPE;
use tilo_actions::{
    ActionArgs, ActionParam, ActionSpec, ActionType, EncodedAction, ExecutionLocus, ParamType,
    ResourceSpec, ResourceViewSpec, Target,
};

use crate::action::DynAction;
use crate::background::{Pending, Poll, Waker};
use tilolang_runtime::meta::{ActionMeta, Meta};

use crate::meta::{MetaExt as _, action_kind};
use crate::surface::{arguments, is_destructive, published_param};

/// Who is invoking: what the door admitted of the caller, for the runtime's principal.
///
/// An agent is a task of a person under that person's narrowed rights, and the door tells the
/// host which task (the delegate's id) and for whom. A person at the window is `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    /// The authenticated principal: a person, or an agent task's delegate.
    pub actor: String,
    /// Whether the actor is software acting for a person.
    pub agent: bool,
    /// The person an agent acts for.
    pub on_behalf_of: Option<String>,
}

/// Where the planner reads the caller of the invocation being built, on the thread that builds it.
pub type CallerSource = dyn Fn() -> Option<Caller> + Send + Sync;

/// What the planner is asked to run.
#[derive(Clone, Debug)]
pub struct PlanRequest<'a> {
    pub app: &'a str,
    /// The kind the action was invoked on.
    pub kind: &'a str,
    pub action_id: &'a str,
    /// The subject resource's id, when the action has one.
    pub resource: Option<&'a str>,
    /// The arguments beyond the subject, by parameter name.
    pub args: &'a serde_json::Map<String, Json>,
    /// Who invokes, when it is not the person at the window.
    pub caller: Option<&'a Caller>,
}

/// Runs a tilolang action as a planner and records what it wrote.
///
/// The implementation is the host's runtime: a transaction over a copy of the room record
/// collects the module's mutations as edits, and nothing is committed here.
pub trait Planner: Send + Sync {
    /// # Errors
    /// Why the action cannot run: a trap, a refused argument, an unknown action.
    fn plan(&self, request: &PlanRequest<'_>) -> Result<DynAction, String>;

    /// The caller of the invocation being built now, as the door admitted it.
    fn caller(&self) -> Option<Caller> {
        None
    }
}

/// Hands out an action the runtime already committed, for the executor to commit.
pub trait Committer: Send + Sync {
    /// The encoded action queued under `ticket`.
    ///
    /// # Errors
    /// Nothing is queued under the ticket.
    fn encoded(&self, ticket: u64) -> Result<EncodedAction, String>;

    /// What committing the action queued under `ticket` needs of a room
    /// ([`DynAction::need`]).
    ///
    /// # Errors
    /// Nothing is queued under the ticket.
    fn need(&self, ticket: u64) -> Result<tilo_actions::ActionNeed, String>;
}

/// The action id of the host action that commits a queued action.
pub const COMMIT: &str = "commit";

/// The id of the published resource for the tilolang resource `id` of `app`.
#[must_use]
pub fn resource_id(app: &str, id: &str) -> String {
    format!("tl:{app}:{id}")
}

/// What a resource of `schema` in the package `meta` is called while none of its own text names
/// it: a package's root by the app's title ("Notes"), anything else by its type ("Todo list").
#[must_use]
pub fn untitled(meta: &Meta, schema: &crate::schema::TypeSchema) -> String {
    if meta.roots.contains(&schema.id) {
        meta.title.clone()
    } else {
        schema.spoken_name()
    }
}

/// The id of a package's own resource, for its record-less actions.
#[must_use]
pub fn package_resource_id(app: &str) -> String {
    format!("tl:{app}")
}

/// The id of the host resource.
pub const HOST_RESOURCE: &str = "tl:host";

/// The type of the typed action `action` on resources of kind `kind`, whose build runs
/// `planner` on the subject its spec is bound to: the resource id it is published on, a
/// [`String`] [`Target`], or none for an action that takes no resource.
#[must_use]
pub fn action_type(
    meta: &Meta,
    kind: &str,
    action: &ActionMeta,
    planner: &Arc<dyn Planner>,
) -> Arc<ActionType> {
    let params: Vec<ActionParam> = arguments(meta, &action.params)
        .iter()
        .map(|param| published_param(meta, param))
        .collect();
    let (app, kind, id) = (meta.app.clone(), kind.to_owned(), action.id.clone());
    let planner = Arc::clone(planner);
    let build = move |subject: &Target, args: &ActionArgs| {
        let json = args.to_json();
        let caller = planner.caller();
        let request = PlanRequest {
            app: &app,
            kind: &kind,
            action_id: &id,
            resource: subject.get::<String>().map(String::as_str),
            args: &json,
            caller: caller.as_ref(),
        };
        planner
            .plan(&request)
            .and_then(|planned| planned.encode().map_err(|error| error.to_string()))
            .unwrap_or_else(|message| EncodedAction {
                type_id: INVALID_ARGUMENTS_TYPE.to_owned(),
                payload: message.into_bytes(),
            })
    };
    // What the platform calls the action, as the compiler derived it; a package compiled before
    // labels existed is named by its action.
    let label = if action.label.is_empty() {
        action.name.clone()
    } else {
        action.label.clone()
    };
    let placeholder = if action.doc.is_empty() {
        label.clone()
    } else {
        action.doc.clone()
    };
    // An action without parameters is planned when it runs, not when it is published: planning
    // is a trial run of the action, and publishing every item's actions must not run them all.
    let no_params = params.is_empty();
    let spec = if no_params {
        ActionSpec::deferred_on(
            action.id.clone(),
            label,
            ExecutionLocus::ClientLocal,
            crate::action::ACTION_TYPE,
            move |subject| build(subject, &ActionArgs::none()),
        )
    } else {
        ActionSpec::typed_on(
            action.id.clone(),
            label,
            placeholder,
            ExecutionLocus::ClientLocal,
            params,
            build,
        )
    };
    // Every argument has a default (a checkbox's `check(item, done: Bool = not item.done)`): a
    // person choosing it is asked nothing, and an agent may still pass `done:`.
    let defaulted = arguments(meta, &action.params)
        .iter()
        .all(|param| param.default.is_some());
    let spec = if defaulted && !no_params {
        spec.with_immediate_per_target()
    } else {
        spec
    };
    // In the order a native verb's declaration is made into its spec: its kind, description,
    // icon, then whether it is destructive, which settles its confirmation.
    let mut spec = spec.of_kind(action_kind(action.class));
    if !action.doc.is_empty() {
        spec = spec.with_description(action.doc.clone());
    }
    if let Some(icon) = &action.icon {
        spec = spec.with_icon(icon.clone());
    }
    if is_destructive(action) {
        spec = spec.destructive();
    }
    spec.type_mut().contract.settle_confirmation();
    Arc::clone(spec.action_type())
}

/// The action types a package's resources offer, built once per package and shared by every
/// resource that offers them: an item's spec is its type bound to the item.
pub struct ActionTypes {
    meta: Meta,
    planner: Arc<dyn Planner>,
    /// Per type name, the action types of its resources.
    on_type: std::collections::BTreeMap<String, TypeActions>,
}

/// The action types of the resources of one tilolang type.
struct TypeActions {
    /// The two direct edits ([`edit_types`]), which a record offers.
    edits: [Arc<ActionType>; 2],
    /// The actions whose subject is the type.
    actions: Vec<Arc<ActionType>>,
}

impl ActionTypes {
    /// What a record of the type `schema` is called when no text in it names it: a package's
    /// root (one per room) by the package's title, another by its type. Never by its id.
    fn untitled(&self, schema: &crate::schema::TypeSchema) -> String {
        untitled(&self.meta, schema)
    }

    /// The action types of the package `meta`, whose actions `planner` runs.
    #[must_use]
    pub fn new(meta: &Meta, planner: &Arc<dyn Planner>) -> Self {
        let mut on_type = std::collections::BTreeMap::new();
        for declared in &meta.types {
            let kind = meta.kind_of(&declared.name);
            let actions = meta
                .offered_actions()
                .filter(|action| {
                    meta.subject_of(&action.params)
                        .is_some_and(|subject| subject.name == declared.name)
                })
                .map(|action| action_type(meta, &kind, action, planner))
                .collect();
            on_type.insert(
                declared.name.clone(),
                TypeActions {
                    edits: edit_types(meta, &declared.name),
                    actions,
                },
            );
        }
        Self {
            meta: meta.clone(),
            planner: Arc::clone(planner),
            on_type,
        }
    }

    /// The package these types are of.
    #[must_use]
    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    /// Whether these are the types of the package as `meta` describes it now, run by
    /// `planner`.
    fn describe(&self, meta: &Meta, planner: &Arc<dyn Planner>) -> bool {
        self.meta.app == meta.app
            && self.meta.module_hash == meta.module_hash
            && Arc::ptr_eq(&self.planner, planner)
    }

    /// The resource `id` of the type `type_name`, labelled `label`, carrying its `open`, the
    /// direct edits when it is a record (`edits`), and the actions on its type.
    fn resource(&self, type_name: &str, id: &str, label: String, edits: bool) -> ResourceSpec {
        let mut resource = ResourceSpec::new(
            resource_id(&self.meta.app, id),
            self.meta.kind_of(type_name),
            label,
        )
        .with_action(open_spec(id));
        for view in views_of(&self.meta, type_name) {
            resource = resource.with_view(view);
        }
        let Some(of_type) = self.on_type.get(type_name) else {
            return resource;
        };
        let subject = Target::new(id.to_owned());
        let edits = if edits { &of_type.edits[..] } else { &[] };
        for ty in edits.iter().chain(&of_type.actions) {
            resource = resource.with_action(ty.bind(subject.clone()));
        }
        resource
    }
}

/// The views a resource of `type_name` is shown through: the package's offered views whose
/// first parameter is one, each opening through the resource's `open`. The type's default view
/// is the primary one; a view that shows a resource the same way under a name a person chose is
/// listed beside it.
fn views_of(meta: &Meta, type_name: &str) -> Vec<ResourceViewSpec> {
    let Some(declared) = meta.types.iter().find(|t| t.name == type_name) else {
        return Vec::new();
    };
    let primary = meta.default_view(&declared.id);
    let of_type = |ty: &str| {
        matches!(
            tilolang_runtime::meta::parse_ty(ty, &meta.app),
            Ok(tilolang_runtime::meta::Ty::Record(key))
                if key.app == meta.app && key.name == type_name
        )
    };
    meta.offered_views()
        .filter(|view| view.params.first().is_some_and(|p| of_type(&p.ty)))
        .map(|view| {
            let label = if view.name.is_empty() {
                view.id.clone()
            } else {
                view.name.clone()
            };
            let mut spec = ResourceViewSpec::new(view.id.clone(), label, "open");
            if let Some(line) = view
                .doc
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
            {
                spec = spec.with_description(line);
            }
            spec.with_primary(primary == Some(view.id.as_str()))
        })
        .collect()
}

/// The resource `id` of the type `type_name`, published with the actions of that type.
#[must_use]
pub fn publish_record(types: &ActionTypes, type_name: &str, id: &str, label: &str) -> ResourceSpec {
    types.resource(type_name, id, label.to_owned(), true)
}

/// The `open` view action of the published resource `anchor`: what a search hit invokes.
fn open_spec(anchor: &str) -> ActionSpec {
    use tilo_resource::Effect as _;
    crate::effects::OpenEffect::Open {
        anchor: anchor.to_owned(),
    }
    .bound()
}

/// The resource of a root record with the items of its collections as child resources, each
/// carrying the actions whose subject is its type (a draft's `send`, a note's `pin`).
///
/// An item's resource id is the root's with the item's id (`doc/item`), the same form an
/// action takes an item as an argument in, so an agent that surveys the tree calls an
/// item's verbs directly.
#[must_use]
pub fn publish_tree(
    types: &ActionTypes,
    record: &crate::record::DynRecord,
    type_name: &str,
    doc: &str,
    label: &str,
) -> ResourceSpec {
    let mut resource = publish_record(types, type_name, doc, label);
    for child in item_resources(types, record, doc) {
        resource = resource.with_child(child);
    }
    resource
}

fn item_resources(
    types: &ActionTypes,
    record: &crate::record::DynRecord,
    parent: &str,
) -> Vec<ResourceSpec> {
    use crate::record::DynValue;
    let mut out = Vec::new();
    for field in &record.schema().fields {
        let Some(DynValue::Collection(items)) = record.get(&field.id) else {
            continue;
        };
        for (item_id, item) in items.iter() {
            let path = crate::index::derive::item_path(parent, item_id.as_str());
            out.push(item_resource(types, item, &path));
        }
    }
    out
}

/// The item `item` at `path`, published with its type's actions and its own items.
fn item_resource(types: &ActionTypes, item: &crate::record::DynRecord, path: &str) -> ResourceSpec {
    let type_name = &item.schema().name;
    let label = item_label(item).unwrap_or_else(|| type_name.clone());
    let mut resource = types.resource(type_name, path, label, false);
    for child in item_resources(types, item, path) {
        resource = resource.with_child(child);
    }
    resource
}

/// What a room publishes of its records, kept up to date by republishing only what changed.
///
/// A record is republished in its cached tree where a change reached it, at that resource's
/// depth: an item whose own field changed has its label read again (and so do the items holding
/// it); items added to, removed from or moved in a collection are added, removed or moved among
/// the children of the resource holding it; an item replaced as a whole is built anew.
/// Only a record that appeared, or one that changed as a whole, is read and built in full, and
/// that happens on a thread of its own, from a snapshot of the store, as search's first index
/// does ([`crate::search::SearchIndex`]): until it ends, updates change nothing.
pub struct Publications {
    watch: crate::store::Watch,
    /// Each the very tree it was while the record publishes the same: a reader holding it
    /// knows by its pointer that nothing in it changed.
    trees: std::collections::BTreeMap<String, Rc<ResourceSpec>>,
    /// Moves whenever a tree is added, replaced or removed.
    revision: u64,
    /// The last revision at which a tree was added, removed or changed in more than its labels.
    structural: u64,
    /// The labels each recent revision that changed only labels moved, oldest first: what a
    /// reader holding an earlier revision patches instead of reading every tree again.
    relabels: std::collections::VecDeque<(u64, Vec<(String, String)>)>,
    /// Per record, the resource whose text names it now.
    named_by: std::collections::BTreeMap<String, Option<String>>,
    /// Records being built whole off the frame.
    building: Option<(Vec<String>, Pending<Vec<Built>>)>,
    wake: Option<Waker>,
    /// Per app, the action types its records offer.
    types: std::collections::BTreeMap<String, Arc<ActionTypes>>,
}

/// A record built whole off the frame: its tree and the resource whose text names it.
struct Built {
    doc: String,
    tree: ResourceSpec,
    named_by: Option<String>,
}

/// A record to build whole.
struct Whole {
    doc: String,
    types: Arc<ActionTypes>,
    type_name: String,
    type_id: String,
}

impl Whole {
    fn build(&self, snapshot: &crate::store::Snapshot) -> Built {
        let root = tilolang_runtime::value::ResourceRef::root(&self.doc, self.type_id.clone());
        let named = snapshot.first_text(&root);
        let schema = crate::schema::lookup(&self.type_id);
        let label = named.as_ref().map_or_else(
            || {
                schema.map_or_else(
                    || self.type_name.clone(),
                    |schema| self.types.untitled(schema),
                )
            },
            |(text, _)| text.clone(),
        );
        let view = snapshot
            .record_of(&self.doc)
            .and_then(|(app, _)| snapshot.view(app));
        let mut tree = publish_record(&self.types, &self.type_name, &self.doc, &label);
        if let (Some(view), Some(schema)) = (view, schema) {
            for child in indexed_items(&self.types, &view, snapshot, &self.doc, schema) {
                tree = tree.with_child(child);
            }
        }
        Built {
            doc: self.doc.clone(),
            tree,
            named_by: named.map(|(_, at)| at),
        }
    }
}

/// The items of the collections of the resource at `parent` (of `schema`), as the index holds
/// them, each published with its own items.
fn indexed_items(
    types: &ActionTypes,
    view: &crate::index::view::View<'_>,
    reader: &crate::store::Snapshot,
    parent: &str,
    schema: &crate::schema::TypeSchema,
) -> Vec<ResourceSpec> {
    let mut out = Vec::new();
    for field in schema
        .fields
        .iter()
        .filter(|f| f.kind == crate::schema::MergeKind::Collection)
    {
        let Ok(items) = view.list(parent, &field.id) else {
            continue;
        };
        for index in 0..items.len() {
            let Some(id) = items.get(index) else {
                break;
            };
            let path = crate::index::view::child(parent, &id);
            if let Some(item) = indexed_item(types, view, reader, &path, field.record.as_deref()) {
                out.push(item);
            }
        }
    }
    out
}

/// The item at `path`, as the index holds it, published with its type's actions and its own
/// items; `item_type` is its collection's item type, for an item the index holds no type of. Its
/// label is its first text as the store's reader reads it (`reader`, [`crate::reading`]).
fn indexed_item(
    types: &ActionTypes,
    view: &crate::index::view::View<'_>,
    reader: &crate::store::Snapshot,
    path: &str,
    item_type: Option<&str>,
) -> Option<ResourceSpec> {
    let ty = view.type_of(path).ok().flatten();
    let schema = ty
        .as_deref()
        .or(item_type)
        .and_then(crate::schema::lookup)?;
    let label = view
        .first_text(path, schema)
        .ok()
        .flatten()
        .and_then(|(text, _)| reader.said(path, text))
        .unwrap_or_else(|| schema.name.clone());
    let mut resource = types.resource(&schema.name, path, label, false);
    for child in indexed_items(types, view, reader, path, schema) {
        resource = resource.with_child(child);
    }
    Some(resource)
}

impl Publications {
    /// Nothing published yet; the first [`Self::update`] publishes every record.
    #[must_use]
    pub fn new(host: &crate::host::Host) -> Self {
        Self {
            watch: host.store().watch(),
            trees: std::collections::BTreeMap::new(),
            revision: 0,
            structural: 0,
            relabels: std::collections::VecDeque::new(),
            named_by: std::collections::BTreeMap::new(),
            building: None,
            wake: None,
            types: std::collections::BTreeMap::new(),
        }
    }

    /// The action types of the package `meta`, built once per package and planner.
    fn types_of(&mut self, meta: &Meta, planner: &Arc<dyn Planner>) -> Arc<ActionTypes> {
        match self.types.get(&meta.app) {
            Some(types) if types.describe(meta, planner) => Arc::clone(types),
            _ => {
                let types = Arc::new(ActionTypes::new(meta, planner));
                self.types.insert(meta.app.clone(), Arc::clone(&types));
                types
            }
        }
    }

    /// The records among `docs` to build whole, as the host has them now.
    fn wholes(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
        docs: &[String],
    ) -> Vec<Whole> {
        host.records()
            .into_iter()
            .filter(|(_, _, doc)| docs.contains(doc))
            .filter_map(|(package, type_name, doc)| {
                let type_id = package
                    .meta
                    .types
                    .iter()
                    .find(|t| t.name == type_name)?
                    .id
                    .clone();
                Some(Whole {
                    doc,
                    types: self.types_of(&package.meta, planner),
                    type_name,
                    type_id,
                })
            })
            .collect()
    }

    /// Have `wake` called (from another thread) when records built off the frame are ready
    /// for the next [`Self::update`] to take in.
    pub fn set_waker(&mut self, wake: Waker) {
        self.wake = Some(wake);
    }

    /// Whether records are being built off the frame.
    #[must_use]
    pub fn is_building(&self) -> bool {
        self.building.is_some()
    }

    /// [`Self::update`], waiting first for records being built off the frame: for a host
    /// without frames.
    pub fn settle(&mut self, host: &crate::host::Host, planner: &Arc<dyn Planner>) -> Vec<String> {
        let mut touched = self.update(host, planner);
        while let Some((docs, building)) = self.building.take() {
            self.take_in(host, planner, &docs, building.wait(), &mut touched);
            touched.extend(self.update(host, planner));
        }
        touched.sort();
        touched.dedup();
        touched
    }

    /// Take in records built off the frame; all of them are built here if the building was
    /// lost.
    fn take_in(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
        docs: &[String],
        built: Poll<Vec<Built>>,
        touched: &mut Vec<String>,
    ) {
        let built = match built {
            Poll::Ready(built) => built,
            Poll::Running | Poll::Lost => {
                let snapshot = host.store().snapshot();
                self.wholes(host, planner, docs)
                    .iter()
                    .map(|whole| whole.build(&snapshot))
                    .collect()
            }
        };
        for Built {
            doc,
            tree,
            named_by,
        } in built
        {
            self.trees.insert(doc.clone(), Rc::new(tree));
            self.named_by.insert(doc.clone(), named_by);
            touched.push(doc);
            self.revision = self.revision.wrapping_add(1);
            self.structural = self.revision;
        }
    }

    /// The published records, by document id. A record whose tree did not change since an
    /// earlier call is the very same tree.
    #[must_use]
    pub fn trees(&self) -> &std::collections::BTreeMap<String, Rc<ResourceSpec>> {
        &self.trees
    }

    /// A number that moves whenever [`Self::trees`] changes, and only then.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The labels that moved since `revision`, by published resource id, each with its label
    /// now: all that changed in [`Self::trees`] since then. `None` when more than labels changed
    /// since (a tree was added, removed or changed in what it holds), or when what changed that
    /// long ago is no longer kept.
    #[must_use]
    pub fn labels_since(&self, revision: u64) -> Option<Vec<(String, String)>> {
        if revision == self.revision {
            return Some(Vec::new());
        }
        if self.structural > revision || revision > self.revision {
            return None;
        }
        let first = self.relabels.iter().position(|(at, _)| *at > revision)?;
        // Every revision after `revision` is one the journal holds.
        if self.relabels.get(first)?.0 != revision.wrapping_add(1) {
            return None;
        }
        let mut out: Vec<(String, String)> = Vec::new();
        for (_, labels) in self.relabels.iter().skip(first) {
            for (id, label) in labels {
                match out.iter_mut().find(|(held, _)| held == id) {
                    Some(slot) => slot.1.clone_from(label),
                    None => out.push((id.clone(), label.clone())),
                }
            }
        }
        Some(out)
    }

    /// Republish what changed since the last update. Returns the documents whose published tree
    /// changed (or went away).
    pub fn update(&mut self, host: &crate::host::Host, planner: &Arc<dyn Planner>) -> Vec<String> {
        let mut relabels = Some(Vec::new());
        let touched = self.update_trees(host, planner, &mut relabels);
        if !touched.is_empty() {
            self.revision = self.revision.wrapping_add(1);
            match relabels {
                Some(labels) => {
                    if self.relabels.len() >= RELABELS_KEPT {
                        self.relabels.pop_front();
                    }
                    self.relabels.push_back((self.revision, labels));
                }
                None => self.structural = self.revision,
            }
        }
        touched
    }

    /// [`Self::update`]'s work on the trees; `relabels` gathers the labels it moved, and is
    /// `None` once it changed more than labels.
    fn update_trees(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
        relabels: &mut Option<Vec<(String, String)>>,
    ) -> Vec<String> {
        use std::collections::BTreeSet;

        use tilolang_runtime::store::ANY_FIELD;

        let mut touched = Vec::new();
        if let Some((docs, building)) = self.building.take() {
            match building.poll() {
                Poll::Running => {
                    self.building = Some((docs, building));
                    return touched;
                }
                built => {
                    self.take_in(host, planner, &docs, built, &mut touched);
                    *relabels = None;
                }
            }
        }
        let changes = self.watch.take();
        let mut whole_docs = Vec::new();
        let records = host.records();
        let now: BTreeSet<&str> = records.iter().map(|(_, _, doc)| doc.as_str()).collect();
        let gone: Vec<String> = self
            .trees
            .keys()
            .filter(|doc| !now.contains(doc.as_str()))
            .cloned()
            .collect();
        for doc in gone {
            self.trees.remove(&doc);
            self.named_by.remove(&doc);
            touched.push(doc);
            *relabels = None;
        }
        for (package, type_name, doc) in &records {
            let Some(schema) = package
                .meta
                .types
                .iter()
                .find(|t| &t.name == type_name)
                .and_then(|t| crate::schema::lookup(&t.id))
            else {
                continue;
            };
            let types = self.types_of(&package.meta, planner);
            let at = Published {
                store: host.store(),
                types: &types,
                schema,
                doc,
            };
            let whole = changes.everything
                || changes.whole.contains(doc)
                || !self.trees.contains_key(doc)
                || changes.docs.get(doc).is_some_and(|fields| {
                    fields
                        .iter()
                        .any(|key| key.resource == *doc && key.field == ANY_FIELD)
                });
            if whole {
                whole_docs.push(Whole {
                    doc: doc.clone(),
                    types: Arc::clone(&types),
                    type_name: type_name.clone(),
                    type_id: schema.id.clone(),
                });
                continue;
            }
            let (Some(fields), Some(tree)) = (changes.docs.get(doc), self.trees.get_mut(doc))
            else {
                continue;
            };
            match at.republish(
                tree,
                (fields, &changes),
                self.named_by.entry(doc.clone()).or_default(),
            ) {
                Republished::Unchanged => {}
                Republished::Relabeled(labels) => {
                    if let Some(gathered) = relabels.as_mut() {
                        gathered.extend(labels);
                    }
                    touched.push(doc.clone());
                }
                Republished::Rebuilt => {
                    *relabels = None;
                    touched.push(doc.clone());
                }
            }
        }
        if !whole_docs.is_empty() {
            self.build_off_frame(host, whole_docs);
        }
        touched
    }

    /// Build `whole_docs` off the frame; changes from now on stay in the watch until they are
    /// taken in.
    fn build_off_frame(&mut self, host: &crate::host::Host, whole_docs: Vec<Whole>) {
        let snapshot = host.store().snapshot();
        let docs = whole_docs.iter().map(|whole| whole.doc.clone()).collect();
        self.building = Some((
            docs,
            Pending::start(
                move || {
                    whole_docs
                        .iter()
                        .map(|whole| whole.build(&snapshot))
                        .collect()
                },
                self.wake.clone(),
            ),
        ));
    }
}

/// How many label-only revisions [`Publications::labels_since`] reaches back over.
const RELABELS_KEPT: usize = 256;

/// What republishing a record did to its tree.
enum Republished {
    /// It is the very tree it was.
    Unchanged,
    /// Only labels moved: these, by published resource id, each with its label now.
    Relabeled(Vec<(String, String)>),
    /// Items were built, dropped or moved.
    Rebuilt,
}

/// What to rebuild of a published record: the resources a change reached, each at its own
/// depth, so a message added to a conversation of ten thousand is one member added to it, and
/// a draft typed into the conversation is its label read again.
struct Rework {
    /// The items created, replaced or removed as a whole: built anew, with their own items.
    nodes: std::collections::BTreeSet<String>,
    /// The collections whose membership or order changed, by the resource holding them (the
    /// record or one of its items) and field.
    collections: std::collections::BTreeSet<(String, String)>,
    /// The items whose label may have changed: those a change reached, and the items holding
    /// them, as an item's label is the first text in it.
    relabels: std::collections::BTreeSet<String>,
    /// Whether the text that names the record may have changed.
    relabel: Relabel,
}

/// Where to look again for the text that names a record (the first text of its fields in id
/// order, a collection's items in order: [`crate::reads::first_text`]).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Relabel {
    /// Nothing changed at or before the text that names it.
    Kept,
    /// Only the top-level item that holds the name changed: if it still has a text, the first is
    /// the name, as the items and fields before it hold none; else the whole record is read.
    Within(String),
    /// A field at or before the one that names it changed.
    Whole,
}

/// One record being republished.
struct Published<'a> {
    store: &'a crate::store::TiloStore,
    types: &'a ActionTypes,
    schema: &'static crate::schema::TypeSchema,
    doc: &'a str,
}

impl Published<'_> {
    fn root(&self) -> tilolang_runtime::value::ResourceRef {
        tilolang_runtime::value::ResourceRef::root(self.doc, self.schema.id.clone())
    }

    /// The text that names the record, and the resource that holds it.
    fn label(&self) -> (String, Option<String>) {
        let named = self.store.first_text(&self.root());
        let label = named.as_ref().map_or_else(
            || self.types.untitled(self.schema),
            |(text, _)| text.clone(),
        );
        (label, named.map(|(_, at)| at))
    }

    /// The first text of the top-level item `top`, which named the record, and where it is.
    fn label_within(&self, top: &str) -> Option<(String, Option<String>)> {
        let at = self.at(top)?;
        let (text, held) = self.store.first_text(&at)?;
        Some((text, Some(held)))
    }

    /// The resource holding the collection the item at `path` is in now, and the field.
    fn holder(&self, path: &str) -> Option<(String, String)> {
        use tilolang_runtime::store::Store as _;
        let at = tilolang_runtime::value::ResourceRef::parse_id(path, String::new())?;
        at.item.as_ref()?;
        let (holder, field) = self.store.container(&at).ok()??;
        Some((holder.id_string(), field))
    }

    /// The resources from the record down to the resource at `path`, the record first and
    /// `path` last, as the store has them now; `None` for one in no collection.
    fn chain(&self, path: &str) -> Option<Vec<String>> {
        let mut out = vec![path.to_owned()];
        let mut at = path.to_owned();
        while at != self.doc {
            let (holder, _) = self.holder(&at)?;
            if out.contains(&holder) {
                return None;
            }
            out.push(holder.clone());
            at = holder;
        }
        out.reverse();
        Some(out)
    }

    fn rework(
        &self,
        fields: &std::collections::BTreeSet<tilolang_runtime::store::FieldKey>,
        named_by: Option<&str>,
    ) -> Rework {
        use tilolang_runtime::store::ANY_FIELD;

        let mut rework = Rework {
            nodes: std::collections::BTreeSet::new(),
            collections: std::collections::BTreeSet::new(),
            relabels: std::collections::BTreeSet::new(),
            relabel: Relabel::Kept,
        };
        // The field of the record the name is read through, when an item holds it: fields after
        // it in id order cannot name the record while it does.
        let named_chain = named_by
            .filter(|at| *at != self.doc)
            .and_then(|at| self.chain(at));
        let naming_field = named_chain
            .as_ref()
            .and_then(|chain| chain.get(1))
            .and_then(|top| self.holder(top))
            .map(|(_, field)| field);
        for key in fields {
            // A resource's first and last versions (`created(r)`, `updated(r)`), which every
            // change moves: nothing published reads them.
            if tilolang_runtime::store::Reflected::of(&key.field).is_some() {
                continue;
            }
            if key.resource == self.doc {
                let kind = self.schema.field(&key.field).map(|f| f.kind);
                if kind == Some(crate::schema::MergeKind::Collection) {
                    rework
                        .collections
                        .insert((key.resource.clone(), key.field.clone()));
                }
                if naming_field
                    .as_deref()
                    .is_none_or(|f| key.field.as_str() <= f)
                {
                    rework.relabel = Relabel::Whole;
                }
                continue;
            }
            let Some(chain) = self.chain(&key.resource) else {
                // An item in no collection now: what held it lists it no more.
                rework.nodes.insert(key.resource.clone());
                continue;
            };
            let Some(top) = chain.get(1).cloned() else {
                continue;
            };
            if rework.relabel == Relabel::Kept
                && named_chain
                    .as_ref()
                    .is_some_and(|named| named.contains(&top))
            {
                rework.relabel = Relabel::Within(top);
            }
            // The item and every item holding it, up to the record.
            for at in chain.iter().skip(1) {
                rework.relabels.insert(at.clone());
            }
            let kind = self
                .schema_at(&key.resource)
                .and_then(|schema| schema.field(&key.field))
                .map(|field| field.kind);
            match kind {
                Some(crate::schema::MergeKind::Collection) => {
                    rework
                        .collections
                        .insert((key.resource.clone(), key.field.clone()));
                    // An item in it may have moved: the one whose text names the record among
                    // them, or one now before it.
                    if named_chain.is_some() && rework.relabel != Relabel::Whole {
                        rework.relabel = Relabel::Whole;
                    }
                }
                // A field of the item's own: its label, read again.
                Some(_) if key.field != ANY_FIELD => {}
                _ => {
                    rework.nodes.insert(key.resource.clone());
                }
            }
        }
        rework
    }

    /// Republish in `tree` what the changes to `fields` reached; `named_by` is the resource
    /// whose text names the record, kept up to date.
    /// What changed in the tree. A change that reached nothing the tree shows (a note's body
    /// typed into, which no label reads) leaves it the very tree it was.
    fn republish(
        &self,
        tree: &mut Rc<ResourceSpec>,
        (fields, changes): (
            &std::collections::BTreeSet<tilolang_runtime::store::FieldKey>,
            &crate::store::Changes,
        ),
        named_by: &mut Option<String>,
    ) -> Republished {
        let mut rework = self.rework(fields, named_by.as_deref());
        if self.shows_the_same(tree, &rework, named_by.as_deref()) {
            return Republished::Unchanged;
        }
        let tree = Rc::make_mut(tree);
        if rework.nodes.is_empty() && rework.collections.is_empty() {
            // A title typed into: only labels move.
            let mut labels = self.relabel(tree, &rework.relabels);
            let (label, named) = match &rework.relabel {
                Relabel::Kept => return Republished::Relabeled(labels),
                Relabel::Within(top) => self.label_within(top).unwrap_or_else(|| self.label()),
                Relabel::Whole => self.label(),
            };
            if tree.label != label {
                labels.push((tree.id.as_str().to_owned(), label.clone()));
                tree.label = label;
            }
            *named_by = named;
            return Republished::Relabeled(labels);
        }
        for (path, field) in &rework.collections {
            if self.within(path, &rework.nodes) {
                continue;
            }
            let key = tilolang_runtime::store::FieldKey {
                resource: path.clone(),
                field: field.clone(),
            };
            let spliced = changes.listing(&key).is_some_and(|splices| {
                self.splice_members(tree, (path, field), &mut rework.nodes, splices)
            });
            if !spliced {
                self.membership(tree, path, &mut rework.nodes);
            }
        }
        self.rebuild(tree, &rework.nodes);
        self.relabel(tree, &rework.relabels);
        let (label, named) = match &rework.relabel {
            Relabel::Kept => return Republished::Rebuilt,
            Relabel::Within(top) => self.label_within(top).unwrap_or_else(|| self.label()),
            Relabel::Whole => self.label(),
        };
        tree.label = label;
        *named_by = named;
        Republished::Rebuilt
    }

    /// Whether `rework` would leave `tree` as it is: it builds and moves no item, and every
    /// label it reads again reads as the tree has it.
    fn shows_the_same(&self, tree: &ResourceSpec, rework: &Rework, named_by: Option<&str>) -> bool {
        if !rework.nodes.is_empty() || !rework.collections.is_empty() {
            return false;
        }
        let relabeled = rework.relabels.iter().all(|path| {
            let (Some(at), Some(schema), Some(node)) =
                (self.at(path), self.schema_at(path), self.node(tree, path))
            else {
                return false;
            };
            let label = self
                .store
                .first_text(&at)
                .map_or_else(|| schema.name.clone(), |(text, _)| text);
            node.label == label
        });
        if !relabeled {
            return false;
        }
        let (label, named) = match &rework.relabel {
            Relabel::Kept => return true,
            Relabel::Within(top) => self.label_within(top).unwrap_or_else(|| self.label()),
            Relabel::Whole => self.label(),
        };
        tree.label == label && named.as_deref() == named_by
    }

    /// Whether the resource at `path` is one of `paths` or within one of them, as the store has
    /// it now.
    fn within(&self, path: &str, paths: &std::collections::BTreeSet<String>) -> bool {
        if paths.contains(path) {
            return true;
        }
        self.chain(path)
            .is_some_and(|chain| chain.iter().any(|at| paths.contains(at)))
    }

    /// The type of the resource at `path`: the record's, or an item's as the store holds it.
    fn schema_at(&self, path: &str) -> Option<&'static crate::schema::TypeSchema> {
        use tilolang_runtime::store::Store as _;
        if path == self.doc {
            return Some(self.schema);
        }
        let at = tilolang_runtime::value::ResourceRef::parse_id(path, String::new())?;
        crate::schema::lookup(&self.store.type_of(&at).ok()?)
    }

    /// The resource at `path`, as the store reads it.
    fn at(&self, path: &str) -> Option<tilolang_runtime::value::ResourceRef> {
        let schema = self.schema_at(path)?;
        tilolang_runtime::value::ResourceRef::parse_id(path, schema.id.clone())
    }

    /// Bring the children of the resource at `path` in line with the items of its
    /// collections: keep the unchanged ones, build the new and the changed ones (taking them out
    /// of `nodes`).
    fn membership(
        &self,
        tree: &mut ResourceSpec,
        path: &str,
        nodes: &mut std::collections::BTreeSet<String>,
    ) {
        use tilolang_runtime::store::Store as _;

        let app = &self.types.meta().app;
        let (Some(at), Some(schema), Some(node)) = (
            self.at(path),
            self.schema_at(path),
            self.node_mut(tree, path),
        ) else {
            return;
        };
        let mut held: std::collections::BTreeMap<String, ResourceSpec> =
            std::mem::take(&mut node.children)
                .into_iter()
                .map(|child| (child.id.as_str().to_owned(), child))
                .collect();
        // The resource's collections in its fields' order, as `publish_tree` has them.
        let mut ordered = Vec::new();
        for f in &schema.fields {
            if f.kind != crate::schema::MergeKind::Collection {
                continue;
            }
            let spec = tilolang_runtime::store::FieldSpec {
                id: f.id.clone(),
                kind: tilolang_runtime::meta::FieldKind::Collection,
            };
            let ids = match self.store.read(&at, &spec) {
                Ok(tilolang_runtime::store::FieldValue::Items(ids)) => ids,
                _ => Vec::new(),
            };
            for id in ids {
                let item = crate::index::derive::item_path(path, &id);
                match held.remove(&resource_id(app, &item)) {
                    Some(child) if !nodes.contains(&item) => ordered.push(child),
                    _ => {
                        nodes.remove(&item);
                        ordered.extend(self.item(&item));
                    }
                }
            }
        }
        node.children = ordered;
    }

    /// Rebuild the items at `paths` in place, each with its own items; drop those gone. An item
    /// within another one rebuilt is rebuilt with it.
    fn rebuild(&self, tree: &mut ResourceSpec, paths: &std::collections::BTreeSet<String>) {
        let app = &self.types.meta().app;
        for path in paths {
            let published = resource_id(app, path);
            // Where it is published now: an item gone from the store is found in the tree.
            let holder = match self.chain(path) {
                Some(chain) => {
                    if chain
                        .iter()
                        .take(chain.len().saturating_sub(1))
                        .any(|at| paths.contains(at))
                    {
                        continue;
                    }
                    chain
                        .len()
                        .checked_sub(2)
                        .and_then(|at| chain.get(at))
                        .cloned()
                }
                None => published_holder(tree, &published).map(|holder| {
                    holder
                        .strip_prefix(&format!("tl:{app}:"))
                        .unwrap_or(holder.as_str())
                        .to_owned()
                }),
            };
            let Some(holder) = holder else {
                continue;
            };
            let Some(node) = self.node_mut(tree, &holder) else {
                continue;
            };
            let at = self.child_index(node, path, &published);
            match (at, self.item(path)) {
                (Some(i), Some(child)) => {
                    if let Some(slot) = node.children.get_mut(i) {
                        *slot = child;
                    }
                }
                (Some(i), None) => {
                    node.children.remove(i);
                }
                // A new item arrives with its collection's membership.
                (None, _) => {}
            }
        }
    }

    /// Read again the labels of the items at `paths`: the first text in each, as
    /// [`item_resource`] names an item. The labels that moved, by published resource id.
    fn relabel(
        &self,
        tree: &mut ResourceSpec,
        paths: &std::collections::BTreeSet<String>,
    ) -> Vec<(String, String)> {
        let mut moved = Vec::new();
        for path in paths {
            let (Some(at), Some(schema)) = (self.at(path), self.schema_at(path)) else {
                continue;
            };
            let Some(node) = self.node_mut(tree, path) else {
                continue;
            };
            let label = self
                .store
                .first_text(&at)
                .map_or_else(|| schema.name.clone(), |(text, _)| text);
            if node.label != label {
                moved.push((node.id.as_str().to_owned(), label.clone()));
                node.label = label;
            }
        }
        moved
    }

    /// Apply the steps of the listing of the collection `field` of the resource at `path` to
    /// its published children: an item inserted is built (and taken out of `nodes`), one removed
    /// dropped, one moved kept. False when the steps do not fit the children, or the children
    /// are then not as many as the items: they are brought in line with the listing instead.
    fn splice_members(
        &self,
        tree: &mut ResourceSpec,
        (path, field): (&str, &str),
        nodes: &mut std::collections::BTreeSet<String>,
        splices: &[tilolang_runtime::store::Splice],
    ) -> bool {
        use tilolang_runtime::store::Splice;

        let app = &self.types.meta().app;
        let (Some(at), Some(schema)) = (self.at(path), self.schema_at(path)) else {
            return false;
        };
        let Some((before, len, after)) = self.members(&at, schema, field) else {
            return false;
        };
        let Some(node) = self.node_mut(tree, path) else {
            return false;
        };
        let mut moving: Option<ResourceSpec> = None;
        for splice in splices {
            match splice {
                Splice::Remove { at: index, item } => {
                    let id = resource_id(app, &crate::index::derive::item_path(path, item));
                    let index = before.saturating_add(*index);
                    if node.children.get(index).is_none_or(|c| c.id.as_str() != id) {
                        return false;
                    }
                    moving = Some(node.children.remove(index));
                }
                Splice::Insert { at: index, item } => {
                    let item = crate::index::derive::item_path(path, item);
                    let index = before.saturating_add(*index);
                    if index > node.children.len() {
                        return false;
                    }
                    let id = resource_id(app, &item);
                    let kept = moving
                        .take()
                        .filter(|child| child.id.as_str() == id && !nodes.contains(&item));
                    nodes.remove(&item);
                    let Some(child) = kept.or_else(|| self.item(&item)) else {
                        return false;
                    };
                    node.children.insert(index, child);
                }
            }
        }
        node.children.len() == before.saturating_add(len).saturating_add(after)
    }

    /// How many published children of `at` the collections before `field` hold, `field`
    /// holds, and those after it hold, as the store lists them now.
    fn members(
        &self,
        at: &tilolang_runtime::value::ResourceRef,
        schema: &crate::schema::TypeSchema,
        field: &str,
    ) -> Option<(usize, usize, usize)> {
        use tilolang_runtime::store::Store as _;

        let (mut before, mut len, mut after) = (0usize, None, 0usize);
        for f in &schema.fields {
            if f.kind != crate::schema::MergeKind::Collection {
                continue;
            }
            let spec = tilolang_runtime::store::FieldSpec {
                id: f.id.clone(),
                kind: tilolang_runtime::meta::FieldKind::Collection,
            };
            let items = self.store.listing(at, &spec).ok()?.len();
            match len {
                _ if f.id == field => len = Some(items),
                None => before = before.saturating_add(items),
                Some(_) => after = after.saturating_add(items),
            }
        }
        Some((before, len?, after))
    }

    /// The index among `holder`'s children of the item at `path`, published as `id`: where
    /// its listing puts it, checked, else looked for.
    fn child_index(&self, holder: &ResourceSpec, path: &str, id: &str) -> Option<usize> {
        let placed = self.holder(path).and_then(|(parent, field)| {
            let item = crate::index::derive::id_of(path)?;
            let at = self.at(&parent)?;
            let (before, _, _) = self.members(&at, self.schema_at(&parent)?, &field)?;
            before.checked_add(self.store.position(&at, &field, item)?)
        });
        placed
            .filter(|i| holder.children.get(*i).is_some_and(|c| c.id.as_str() == id))
            .or_else(|| holder.children.iter().position(|c| c.id.as_str() == id))
    }

    /// The published resource at `path` in `tree`, the record's: each item found where its
    /// listing puts it.
    fn node<'t>(&self, tree: &'t ResourceSpec, path: &str) -> Option<&'t ResourceSpec> {
        let app = &self.types.meta().app;
        let chain = self.chain(path)?;
        let mut node = tree;
        for at in chain.iter().skip(1) {
            let id = resource_id(app, at);
            let index = self.child_index(node, at, &id)?;
            node = node.children.get(index)?;
        }
        Some(node)
    }

    fn node_mut<'t>(&self, tree: &'t mut ResourceSpec, path: &str) -> Option<&'t mut ResourceSpec> {
        let app = &self.types.meta().app;
        let chain = self.chain(path)?;
        let mut node = tree;
        for at in chain.iter().skip(1) {
            let id = resource_id(app, at);
            let index = self.child_index(node, at, &id)?;
            node = node.children.get_mut(index)?;
        }
        Some(node)
    }

    /// The item at `path`, read from the index and published.
    fn item(&self, path: &str) -> Option<ResourceSpec> {
        let schema = self.schema_at(path)?;
        let snapshot = self.store.snapshot();
        let view = snapshot.view(&self.types.meta().app)?;
        indexed_item(self.types, &view, &snapshot, path, Some(&schema.id))
    }
}

/// The id of the published resource holding the one published as `id` in `tree`.
fn published_holder(tree: &ResourceSpec, id: &str) -> Option<String> {
    for child in &tree.children {
        if child.id.as_str() == id {
            return Some(tree.id.as_str().to_owned());
        }
        if let Some(found) = published_holder(child, id) {
            return Some(found);
        }
    }
    None
}

/// What names an item: the first of its texts.
fn item_label(item: &crate::record::DynRecord) -> Option<String> {
    let mut text = Vec::new();
    tilo_resource::Model::collect_text(item, &mut text);
    text.into_iter().next()
}

/// The package's own resource, carrying the actions that take no record.
#[must_use]
pub fn publish_package(meta: &Meta, planner: &Arc<dyn Planner>) -> ResourceSpec {
    let kind = meta.package_kind();
    let mut resource = ResourceSpec::new(package_resource_id(&meta.app), kind.clone(), &meta.title);
    for action in meta.offered_actions() {
        if meta.subject_of(&action.params).is_none() {
            resource = resource
                .with_action(action_type(meta, &kind, action, planner).bind(Target::none()));
        }
    }
    resource
}

/// The host's own resource: the commit of an action the runtime planned while a window handled a
/// press. It is not offered in the palette; the window invokes it.
///
/// It also carries the [`HostEffect`] vocabulary an agent uses: run a block, run a flow, read
/// the catalogue. The window answers them with `Host::answer`.
#[must_use]
pub fn host_resource(committer: &Arc<dyn Committer>) -> ResourceSpec {
    let classify = Arc::clone(committer);
    let committer = Arc::clone(committer);
    let commit = ActionSpec::typed(
        COMMIT,
        "Commit a planned action",
        "ticket",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::required(
            "ticket",
            ParamType::Integer,
            "The ticket of the planned action",
        )],
        move |args| {
            args.integer("ticket")
                .and_then(|ticket| u64::try_from(ticket).ok())
                .ok_or_else(|| "the ticket is not a number".to_owned())
                .and_then(|ticket| committer.encoded(ticket))
                .unwrap_or_else(|message| EncodedAction {
                    type_id: INVALID_ARGUMENTS_TYPE.to_owned(),
                    payload: message.into_bytes(),
                })
        },
    )
    .for_audience(tilo_actions::Audience::Agents)
    // Offered as the least a commit is, a change of the record, as the host's surface declares
    // it; each commit is what the action it carries is, so one that queues an outside effect
    // needs what that effect needs.
    .of_kind(tilo_actions::ActionKind::Undoable)
    .with_need_of(move |args| {
        args.integer("ticket")
            .and_then(|ticket| u64::try_from(ticket).ok())
            .and_then(|ticket| classify.need(ticket).ok())
            .unwrap_or_else(|| tilo_actions::ActionNeed::of(tilo_actions::ActionKind::Undoable))
    });
    let mut host =
        ResourceSpec::new(HOST_RESOURCE, crate::surface::HOST_KIND, "tilolang").with_action(commit);
    for spec in <crate::effects::HostEffect as tilo_resource::Effect>::specs() {
        host = host.with_action(spec);
    }
    host
}

/// The plain edit a bound control makes: set `value` into the field `field` of the resource
/// `id` of the type `type_name`. It needs no planner, since it is not module code.
///
/// # Errors
/// The type or field is not registered, or the value does not fit the field.
pub fn edit_action(
    meta: &Meta,
    type_name: &str,
    id: &str,
    field: &str,
    value: &Json,
) -> Result<DynAction, String> {
    use crate::schema::{MergeKind, lookup};
    use tilo_resource::{Edit, EditOp, Segment};

    let declared = meta
        .type_named(type_name)
        .ok_or_else(|| format!("the package declares no type `{type_name}`"))?;
    let schema =
        lookup(&declared.id).ok_or_else(|| format!("the type `{type_name}` is not registered"))?;
    let schema_field = schema
        .field(field)
        .ok_or_else(|| format!("the type `{type_name}` has no field `{field}`"))?;
    let op = match (schema_field.kind, value) {
        (MergeKind::Text, Json::String(text)) => EditOp::Replace { text: text.clone() },
        (MergeKind::Register, value) => EditOp::Set {
            value: value.clone(),
        },
        _ => return Err(format!("the field `{field}` cannot be set to that value")),
    };
    let action_id = if value.is_boolean() {
        crate::action::EDIT_FLAG_ACTION
    } else {
        crate::action::EDIT_TEXT_ACTION
    };
    Ok(DynAction {
        app: meta.app.clone(),
        kind: meta.kind_of(type_name),
        action_id: action_id.to_owned(),
        args: serde_json::json!({"field": field, "value": value}),
        authorship: crate::action::Authorship {
            module: meta.module_hash.clone(),
            ..crate::action::Authorship::default()
        },
        // An item (`<record>/<item>`) is edited in its record, by its id there.
        edits: vec![match id.split_once('/') {
            Some((record, item)) => crate::action::DynEdit::Edit {
                id: record.to_owned(),
                edit: Edit::new(
                    vec![
                        Segment::Item(tilo_resource::Id::from_raw(item)),
                        Segment::Field(field.to_owned()),
                    ],
                    op,
                ),
            },
            None => crate::action::DynEdit::Edit {
                id: id.to_owned(),
                edit: Edit::new(vec![Segment::Field(field.to_owned())], op),
            },
        }],
    })
}

/// The two direct edit actions of a resource, whose target is its id: a bound text input and
/// a bound checkbox.
fn edit_types(meta: &Meta, type_name: &str) -> [Arc<ActionType>; 2] {
    let make = |action: &'static str, label: &'static str, ty: ParamType| {
        let (meta, type_name) = (meta.clone(), type_name.to_owned());
        let spec = ActionSpec::typed_on(
            action,
            label,
            label,
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required("field", ParamType::Text, "The field id"),
                ActionParam::required("value", ty, "The new value"),
            ],
            move |target: &Target, args: &ActionArgs| {
                let json = args.to_json();
                let field = json.get("field").and_then(Json::as_str).unwrap_or_default();
                let value = json.get("value").cloned().unwrap_or(Json::Null);
                let id = target.get::<String>().map_or("", String::as_str);
                edit_action(&meta, &type_name, id, field, &value)
                    .and_then(|planned| planned.encode().map_err(|error| error.to_string()))
                    .unwrap_or_else(|message| EncodedAction {
                        type_id: INVALID_ARGUMENTS_TYPE.to_owned(),
                        payload: message.into_bytes(),
                    })
            },
        );
        Arc::clone(spec.action_type())
    };
    [
        make(
            crate::action::EDIT_TEXT_ACTION,
            "Edit a text field",
            ParamType::Text,
        ),
        make(
            crate::action::EDIT_FLAG_ACTION,
            "Set a flag field",
            ParamType::Boolean,
        ),
    ]
}
