//! Publishing tilolang resources on the action door (implementation design 7.4).
//!
//! Every resource in the room record is published as a `tilo_actions::Resource` of kind
//! `tl.<app>.<Type>`, with one typed action per tilolang action whose subject is its type. The
//! desktop agent, the runtime door, shortcuts and grants therefore reach tilolang actions with
//! no agent-side code. The build closure of each action runs the wasm **planner** and encodes
//! the resulting [`DynAction`].

use std::sync::Arc;

use serde_json::Value as Json;
use tilo_actions::resource::INVALID_ARGUMENTS_TYPE;
use tilo_actions::{
    ActionArgs, ActionParam, ActionSpec, ActionType, EncodedAction, ExecutionLocus, ParamType,
    Resource, Target,
};

use crate::action::DynAction;
use crate::background::{Pending, Poll, Waker};
use tilolang_runtime::meta::{ActionMeta, Meta, ParamMeta};

use crate::meta::{MetaExt as _, action_kind};
use crate::surface::{arguments, schema_of_type};

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
}

/// The action id of the host action that commits a queued action.
pub const COMMIT: &str = "commit";

/// The id of the published resource for the tilolang resource `id` of `app`.
#[must_use]
pub fn resource_id(app: &str, id: &str) -> String {
    format!("tl:{app}:{id}")
}

/// The id of a package's own resource, for its record-less actions.
#[must_use]
pub fn package_resource_id(app: &str) -> String {
    format!("tl:{app}")
}

/// The id of the host resource.
pub const HOST_RESOURCE: &str = "tl:host";

fn param_type(meta: &Meta, ty: &str) -> ParamType {
    let ty = ty.trim();
    let ty = ty
        .strip_prefix("Option<")
        .and_then(|rest| rest.strip_suffix('>'))
        .map_or(ty, str::trim);
    match ty {
        "Int" => ParamType::Integer,
        "Float" => ParamType::Number,
        "Bool" => ParamType::Boolean,
        "Text" | "Date" => ParamType::Text,
        "Time" => ParamType::DateTime,
        other if meta.type_named(other).is_some() => ParamType::Text,
        // A record of another app the package uses (`so.tilo.lang.notes.Notebook`) is given by
        // its resource id, as one of the package's own.
        other if crate::surface::other_apps_record(meta, other).is_some() => ParamType::Text,
        other if other.starts_with("Map<") => ParamType::Object,
        _ => ParamType::Array,
    }
}

fn action_param(meta: &Meta, param: &ParamMeta) -> ActionParam {
    let ty = param_type(meta, &param.ty);
    let description = format!("{}: {}", param.name, param.ty);
    let optional = param.ty.trim().starts_with("Option<");
    let built = if optional {
        ActionParam::optional_owned(param.name.clone(), ty, description)
    } else {
        ActionParam::required_owned(param.name.clone(), ty, description)
    };
    built.with_schema(schema_of_type(meta, &param.ty))
}

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
        .map(|param| action_param(meta, param))
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
    let spec = if params.is_empty() {
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
    let mut spec = spec.of_kind(action_kind(action.class));
    if !action.doc.is_empty() {
        spec = spec.with_description(action.doc.clone());
    }
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
    fn resource(&self, type_name: &str, id: &str, label: String, edits: bool) -> Resource {
        let mut resource = Resource::new(
            resource_id(&self.meta.app, id),
            self.meta.kind_of(type_name),
            label,
        )
        .with_action(open_spec(id));
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

/// The resource `id` of the type `type_name`, published with the actions of that type.
#[must_use]
pub fn publish_record(types: &ActionTypes, type_name: &str, id: &str, label: &str) -> Resource {
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
/// carrying the actions whose subject is its type (a draft's `send`, a note's `toggle_pin`).
///
/// An item's resource id is the root's with the path to the item (`doc/field:item`), the same
/// form an action takes an item as an argument in, so an agent that surveys the tree calls an
/// item's verbs directly.
#[must_use]
pub fn publish_tree(
    types: &ActionTypes,
    record: &crate::record::DynRecord,
    type_name: &str,
    doc: &str,
    label: &str,
) -> Resource {
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
) -> Vec<Resource> {
    use crate::record::DynValue;
    let mut out = Vec::new();
    for field in &record.schema().fields {
        let Some(DynValue::Collection(items)) = record.get(&field.id) else {
            continue;
        };
        for (item_id, item) in items.iter() {
            let path = format!("{parent}/{}:{}", field.id, item_id.as_str());
            out.push(item_resource(types, item, &path));
        }
    }
    out
}

/// The item `item` at `path`, published with its type's actions and its own items.
fn item_resource(types: &ActionTypes, item: &crate::record::DynRecord, path: &str) -> Resource {
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
    trees: std::collections::BTreeMap<String, Resource>,
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
    tree: Resource,
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
        let label = named
            .as_ref()
            .map_or_else(|| self.doc.clone(), |(text, _)| text.clone());
        let tree = match snapshot.record(&self.doc) {
            Ok(record) => publish_tree(&self.types, &record, &self.type_name, &self.doc, &label),
            Err(_) => publish_record(&self.types, &self.type_name, &self.doc, &label),
        };
        Built {
            doc: self.doc.clone(),
            tree,
            named_by: named.map(|(_, at)| at),
        }
    }
}

impl Publications {
    /// Nothing published yet; the first [`Self::update`] publishes every record.
    #[must_use]
    pub fn new(host: &crate::host::Host) -> Self {
        Self {
            watch: host.store().watch(),
            trees: std::collections::BTreeMap::new(),
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
            self.trees.insert(doc.clone(), tree);
            self.named_by.insert(doc.clone(), named_by);
            touched.push(doc);
        }
    }

    /// The published records, by document id.
    #[must_use]
    pub fn trees(&self) -> &std::collections::BTreeMap<String, Resource> {
        &self.trees
    }

    /// Republish what changed since the last update. Returns the documents whose published tree
    /// changed (or went away).
    pub fn update(&mut self, host: &crate::host::Host, planner: &Arc<dyn Planner>) -> Vec<String> {
        use std::collections::BTreeSet;

        use tilolang_runtime::store::ANY_FIELD;

        let mut touched = Vec::new();
        if let Some((docs, building)) = self.building.take() {
            match building.poll() {
                Poll::Running => {
                    self.building = Some((docs, building));
                    return touched;
                }
                built => self.take_in(host, planner, &docs, built, &mut touched),
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
            at.republish(tree, fields, self.named_by.entry(doc.clone()).or_default());
            touched.push(doc.clone());
        }
        if !whole_docs.is_empty() {
            // Built off the frame; changes from now on stay in the watch until it is taken in.
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
        touched
    }
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
        let label = named
            .as_ref()
            .map_or_else(|| self.doc.to_owned(), |(text, _)| text.clone());
        (label, named.map(|(_, at)| at))
    }

    /// The first text of the top-level item `top`, which named the record, and where it is.
    fn label_within(&self, top: &str) -> Option<(String, Option<String>)> {
        let at = tilolang_runtime::value::ResourceRef::parse_id(top, self.schema.id.clone())?;
        let (text, held) = self.store.first_text(&at)?;
        Some((text, Some(held)))
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
        let naming_field = named_by
            .and_then(|at| at.strip_prefix(self.doc)?.strip_prefix('/'))
            .and_then(|rest| rest.split(':').next());
        for key in fields {
            if key.resource == self.doc {
                let kind = self.schema.field(&key.field).map(|f| f.kind);
                if kind == Some(crate::schema::MergeKind::Collection) {
                    rework
                        .collections
                        .insert((key.resource.clone(), key.field.clone()));
                }
                if naming_field.is_none_or(|f| key.field.as_str() <= f) {
                    rework.relabel = Relabel::Whole;
                }
                continue;
            }
            let Some(top) = top_item(self.doc, &key.resource) else {
                continue;
            };
            if rework.relabel == Relabel::Kept
                && named_by.is_some_and(|at| at == top || at.starts_with(&format!("{top}/")))
            {
                rework.relabel = Relabel::Within(top);
            }
            // The item and every item holding it, up to the record.
            let mut at = key.resource.as_str();
            while at != self.doc {
                rework.relabels.insert(at.to_owned());
                at = at.rsplit_once('/').map_or(self.doc, |(parent, _)| parent);
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
    fn republish(
        &self,
        tree: &mut Resource,
        fields: &std::collections::BTreeSet<tilolang_runtime::store::FieldKey>,
        named_by: &mut Option<String>,
    ) {
        let mut rework = self.rework(fields, named_by.as_deref());
        for (path, field) in &rework.collections {
            if !within(path, &rework.nodes) {
                self.membership(tree, path, field, &mut rework.nodes);
            }
        }
        self.rebuild(tree, &rework.nodes);
        self.relabel(tree, &rework.relabels);
        let (label, named) = match &rework.relabel {
            Relabel::Kept => return,
            Relabel::Within(top) => self.label_within(top).unwrap_or_else(|| self.label()),
            Relabel::Whole => self.label(),
        };
        tree.label = label;
        *named_by = named;
    }

    /// The type of the resource at `path`: the record's, or an item's, through the collections
    /// its path goes down.
    fn schema_at(&self, path: &str) -> Option<&'static crate::schema::TypeSchema> {
        let rest = path.strip_prefix(self.doc)?;
        let mut schema = self.schema;
        for part in rest.split('/').skip(1) {
            let (field, _) = part.split_once(':')?;
            schema = crate::schema::lookup(schema.field(field)?.record.as_deref()?)?;
        }
        Some(schema)
    }

    /// The resource at `path`, as the store reads it.
    fn at(&self, path: &str) -> Option<tilolang_runtime::value::ResourceRef> {
        let schema = self.schema_at(path)?;
        tilolang_runtime::value::ResourceRef::parse_id(path, schema.id.clone())
    }

    /// Bring the children of the resource at `path` from its collection `field` in line with
    /// its items: keep the unchanged ones, build the new and the changed ones (taking them out
    /// of `nodes`).
    fn membership(
        &self,
        tree: &mut Resource,
        path: &str,
        field: &str,
        nodes: &mut std::collections::BTreeSet<String>,
    ) {
        use tilolang_runtime::store::Store as _;

        let app = &self.types.meta().app;
        let (Some(at), Some(schema), Some(node)) = (
            self.at(path),
            self.schema_at(path),
            find_node_mut(tree, self.doc, app, path),
        ) else {
            return;
        };
        let spec = tilolang_runtime::store::FieldSpec {
            id: field.to_owned(),
            kind: tilolang_runtime::meta::FieldKind::Collection,
        };
        let ids = match self.store.read(&at, &spec) {
            Ok(tilolang_runtime::store::FieldValue::Items(ids)) => ids,
            _ => Vec::new(),
        };
        let prefix = resource_id(app, &format!("{path}/{field}:"));
        let mut held = std::collections::BTreeMap::new();
        let mut others = Vec::new();
        for child in std::mem::take(&mut node.children) {
            if child.id.as_str().starts_with(&prefix) {
                held.insert(child.id.as_str().to_owned(), child);
            } else {
                others.push(child);
            }
        }
        let mut members = Vec::with_capacity(ids.len());
        for id in ids {
            let item = format!("{path}/{field}:{id}");
            match held.remove(&resource_id(app, &item)) {
                Some(child) if !nodes.contains(&item) => members.push(child),
                _ => {
                    nodes.remove(&item);
                    members.extend(self.item(&item));
                }
            }
        }
        // The resource's collections in its fields' order, as `publish_tree` has them.
        let mut ordered = Vec::with_capacity(others.len().saturating_add(members.len()));
        let mut members = Some(members);
        for f in &schema.fields {
            if f.kind != crate::schema::MergeKind::Collection {
                continue;
            }
            if f.id == field {
                ordered.extend(members.take().unwrap_or_default());
            } else {
                let p = resource_id(app, &format!("{path}/{}:", f.id));
                ordered.extend(
                    others
                        .iter()
                        .filter(|c| c.id.as_str().starts_with(&p))
                        .cloned(),
                );
            }
        }
        node.children = ordered;
    }

    /// Rebuild the items at `paths` in place, each with its own items; drop those gone. An item
    /// within another one rebuilt is rebuilt with it.
    fn rebuild(&self, tree: &mut Resource, paths: &std::collections::BTreeSet<String>) {
        let app = &self.types.meta().app;
        for path in paths {
            let Some((parent, _)) = path.rsplit_once('/') else {
                continue;
            };
            if within(parent, paths) {
                continue;
            }
            let Some(holder) = find_node_mut(tree, self.doc, app, parent) else {
                continue;
            };
            let published = resource_id(app, path);
            let at = holder
                .children
                .iter()
                .position(|child| child.id.as_str() == published);
            match (at, self.item(path)) {
                (Some(i), Some(child)) => {
                    if let Some(slot) = holder.children.get_mut(i) {
                        *slot = child;
                    }
                }
                (Some(i), None) => {
                    holder.children.remove(i);
                }
                // A new item arrives with its collection's membership.
                (None, _) => {}
            }
        }
    }

    /// Read again the labels of the items at `paths`: the first text in each, as
    /// [`item_resource`] names an item.
    fn relabel(&self, tree: &mut Resource, paths: &std::collections::BTreeSet<String>) {
        for path in paths {
            let (Some(at), Some(schema)) = (self.at(path), self.schema_at(path)) else {
                continue;
            };
            let Some(node) = find_node_mut(tree, self.doc, &self.types.meta().app, path) else {
                continue;
            };
            node.label = self
                .store
                .first_text(&at)
                .map_or_else(|| schema.name.clone(), |(text, _)| text);
        }
    }

    /// The item at `path`, read from the store and published.
    fn item(&self, path: &str) -> Option<Resource> {
        let at = self.at(path)?;
        let item = self.store.record_at(&at).ok()?;
        Some(item_resource(self.types, &item, path))
    }
}

/// Whether `path` is one of `paths` or within one of them.
fn within(path: &str, paths: &std::collections::BTreeSet<String>) -> bool {
    let mut at = path;
    loop {
        if paths.contains(at) {
            return true;
        }
        match at.rsplit_once('/') {
            Some((parent, _)) => at = parent,
            None => return false,
        }
    }
}

/// The published resource at `path` in the tree of the record `doc`.
fn find_node_mut<'t>(
    tree: &'t mut Resource,
    doc: &str,
    app: &str,
    path: &str,
) -> Option<&'t mut Resource> {
    let rest = path.strip_prefix(doc)?;
    let mut node = tree;
    let mut at = doc.to_owned();
    for part in rest.split('/').skip(1) {
        at.push('/');
        at.push_str(part);
        let id = resource_id(app, &at);
        node = node.children.iter_mut().find(|c| c.id.as_str() == id)?;
    }
    Some(node)
}

/// The top-level item an item path is in: `doc/field:item`.
fn top_item(doc: &str, path: &str) -> Option<String> {
    let rest = path.strip_prefix(doc)?.strip_prefix('/')?;
    let first = rest.split('/').next()?;
    Some(format!("{doc}/{first}"))
}

/// What names an item: the first of its texts.
fn item_label(item: &crate::record::DynRecord) -> Option<String> {
    let mut text = Vec::new();
    tilo_resource::Model::collect_text(item, &mut text);
    text.into_iter().next()
}

/// The package's own resource, carrying the actions that take no record.
#[must_use]
pub fn publish_package(meta: &Meta, planner: &Arc<dyn Planner>) -> Resource {
    let kind = meta.package_kind();
    let mut resource = Resource::new(package_resource_id(&meta.app), kind.clone(), &meta.title);
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
pub fn host_resource(committer: &Arc<dyn Committer>) -> Resource {
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
    .programmatic();
    let mut host =
        Resource::new(HOST_RESOURCE, crate::surface::HOST_KIND, "tilolang").with_action(commit);
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
        edits: vec![crate::action::DynEdit::Edit {
            id: id.to_owned(),
            edit: Edit::new(vec![Segment::Field(field.to_owned())], op),
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
