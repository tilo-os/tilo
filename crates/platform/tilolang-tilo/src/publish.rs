//! Publishing tilolang resources on the action door.
//!
//! Every resource in the workspace record is published as a `tilo_actions::ResourceSpec` of kind
//! `tl.<app>.<Type>`, with one typed action per tilolang action whose subject is its type. The
//! desktop agent, the runtime door, shortcuts and grants therefore reach tilolang actions with
//! no agent-side code. The build closure of each action runs the wasm **planner** and encodes
//! the resulting [`DynAction`].

use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value as Json;
use tilo_actions::resource::INVALID_ARGUMENTS_TYPE;
use tilo_actions::{
    ActionArgs, ActionParam, ActionSpec, ActionType, EncodedAction, ExecutionLocus, Noun,
    ParamType, ResourceSpec, ResourceType, ResourceViewSpec, Target,
};

use crate::action::{DIRECT_EDITS, DirectEdit, DynAction};
use crate::background::{Pending, Poll, Waker};
use tilolang_runtime::meta::{ActionMeta, Meta};
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::words::Words;

use crate::meta::{MetaExt as _, action_kind};
use crate::surface::{is_destructive, published_param};

/// What the declaration `id` is called in the reader's language: `source`, its words in the
/// package's own, when no catalogue of theirs holds it.
fn label_of(words: &dyn Words, id: &str, source: &str) -> String {
    words.word(&tilolang::messages::key_of(id), &[], source)
}

/// What a sentence of the declaration `id` says in the reader's language — its description
/// (`doc`), or its `noun` for `count` — and `source` when no catalogue of theirs holds it.
fn said_of(
    words: &dyn Words,
    id: &str,
    attribute: &str,
    count: Option<&str>,
    source: &str,
) -> String {
    let key = format!("{}.{attribute}", tilolang::messages::key_of(id));
    let args = count.map_or_else(Vec::new, |count| {
        vec![("count".to_owned(), count.to_owned())]
    });
    words.word(&key, &args, source)
}

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
    /// The resource the invocation is made from, when it is made from one: what the resources
    /// it creates were made from (`created(r)?.from`, `linked(r)`). A "make from this" menu,
    /// send-to, or an agent working from what it read sets it.
    pub from: Option<&'a ResourceRef>,
}

/// Runs a tilolang action as a planner and records what it wrote.
///
/// The implementation is the host's runtime: a transaction over a copy of the workspace record
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

    /// What committing the action queued under `ticket` needs of a workspace
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

/// The app whose published resource `resource` is: a record's or item's (`tl:<app>:<id>`,
/// [`resource_id`]) or its package's own (`tl:<app>`, [`package_resource_id`]).
#[must_use]
pub fn app_of(resource: &str) -> Option<&str> {
    let rest = resource.strip_prefix("tl:")?;
    Some(rest.split_once(':').map_or(rest, |(app, _)| app))
}

/// What a resource of `schema` in the package `meta` is called while none of its own text names
/// it: what its type declares (`(untitled: "Untitled")`, in the reader's words), else a
/// package's root by the app's title ("Notes"), anything else by its type ("Todo list").
#[must_use]
pub fn untitled(meta: &Meta, schema: &crate::schema::TypeSchema, words: &dyn Words) -> String {
    meta.types
        .iter()
        .find(|declared| declared.id == schema.id)
        .map_or_else(
            || {
                if meta.roots.contains(&schema.id) {
                    meta.title.clone()
                } else {
                    schema.untitled()
                }
            },
            |declared| untitled_of(meta, declared, words),
        )
}

/// What a record of `declared` is called while its title is empty: the words its type declares,
/// translated; else an app's root by the app's title; else its type's name.
fn untitled_of(meta: &Meta, declared: &tilolang::meta::TypeMeta, words: &dyn Words) -> String {
    if let Some(untitled) = &declared.untitled {
        return said_of(words, &declared.id, "untitled", None, untitled);
    }
    if meta.roots.contains(&declared.id) {
        meta.title.clone()
    } else {
        crate::schema::spoken(&declared.name)
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
/// [`String`] [`Target`], or none for an action that takes no resource. What it is called and
/// what it says read through `words` in the reader's language.
#[must_use]
pub fn action_type(
    meta: &Meta,
    kind: &str,
    action: &ActionMeta,
    planner: &Arc<dyn Planner>,
    words: &dyn Words,
) -> Arc<ActionType> {
    let params: Vec<ActionParam> = meta
        .arguments(&action.params)
        .iter()
        .map(|param| {
            // What the parameter is called where a person gives it (a form, a consent sheet).
            let key = tilolang::messages::param_key(&action.id, &param.name);
            published_param(meta, param).titled(words.word(&key, &[], &param.words()))
        })
        .collect();
    let (app, kind, id) = (meta.app.clone(), kind.to_owned(), action.id.clone());
    let planner = Arc::clone(planner);
    let build = move |subject: &Target, args: &ActionArgs| {
        let json = args.to_json();
        let caller = planner.caller();
        // What the invocation is made from, as the caller named it with the arguments.
        let from = args
            .from()
            .and_then(|from| ResourceRef::parse_id(&from.resource, from.kind.clone()));
        let request = PlanRequest {
            app: &app,
            kind: &kind,
            action_id: &id,
            resource: subject.get::<String>().map(String::as_str),
            args: &json,
            caller: caller.as_ref(),
            from: from.as_ref(),
        };
        planner
            .plan(&request)
            .and_then(|planned| planned.encode().map_err(|error| error.to_string()))
            .unwrap_or_else(|message| EncodedAction {
                type_id: INVALID_ARGUMENTS_TYPE.to_owned(),
                payload: message.into_bytes(),
            })
    };
    // What the platform calls the action, as the compiler derived it, in the reader's words.
    let label = label_of(
        words,
        &action.id,
        &tilolang_runtime::platform::label_of(action),
    );
    let placeholder = if action.doc.is_empty() {
        label.clone()
    } else {
        said_of(words, &action.id, "doc", None, &action.doc)
    };
    // A command the person choosing it still has to answer for opens its form: its label says
    // so with "…", as the runtime's own surfaces say it. What the spec is given is what it does
    // not ask for — the subject, the resource it runs against — and the parameters after it are
    // the form's. The form's own placeholder names the command without the dots, which ask.
    let mut label = label;
    if action
        .unbound(action.params.len().saturating_sub(params.len()))
        .next()
        .is_some()
        && !label.ends_with('…')
    {
        label.push('…');
    }
    // An action without parameters is planned when it runs, not when it is published: planning
    // is a trial run of the action, and publishing every item's actions must not run them all.
    let no_params = params.is_empty();
    // Published by its name, as a native verb is: what an agent at the door lists and calls. The
    // build above plans it by the id the declaration keeps, which is also what a change of it
    // says in the record (and its words are keyed by).
    let spec = if no_params {
        ActionSpec::deferred_on(
            action.name.clone(),
            label,
            ExecutionLocus::ClientLocal,
            crate::action::ACTION_TYPE,
            move |subject| build(subject, &ActionArgs::none()),
        )
    } else {
        ActionSpec::typed_on(
            action.name.clone(),
            label,
            placeholder,
            ExecutionLocus::ClientLocal,
            params,
            build,
        )
    };
    // Every argument has a default (a checkbox's `check(item, done: Bool = not item.done)`): a
    // person choosing it is asked nothing, and an agent may still pass `done:`.
    let defaulted = meta
        .arguments(&action.params)
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
        spec = spec.with_description(said_of(words, &action.id, "doc", None, &action.doc));
    }
    if let Some(icon) = &action.icon {
        spec = spec.with_icon(icon.clone());
    }
    if is_destructive(action) {
        spec = spec.destructive();
    }
    // What a change of it says it by: the id the declaration keeps, which is not the name it is
    // published under.
    action.id.clone_into(&mut spec.type_mut().declared);
    // `key: "e"`: the plain key that runs it while its window has the keyboard, shown at the
    // action's row as the keyboard in use spells it.
    if let Some(key) = &action.key {
        spec.type_mut().state.shortcut = Some(key.clone());
    }
    spec.type_mut().contract.settle_confirmation();
    Arc::clone(spec.action_type())
}

/// The action types a package's resources offer, built once per package and shared by every
/// resource that offers them: an item's spec is its type bound to the item.
pub struct ActionTypes {
    meta: Meta,
    planner: Arc<dyn Planner>,
    /// The words the labels, descriptions and nouns below were read in
    /// ([`Words::revision`]): another language publishes them again.
    words: u64,
    /// Per type name, the action types of its resources.
    on_type: std::collections::BTreeMap<String, TypeActions>,
}

/// The action types of the resources of one tilolang type.
struct TypeActions {
    /// The two direct edits ([`edit_types`]), which every record of the type offers, root and
    /// item alike.
    edits: [Arc<ActionType>; 2],
    /// The actions whose subject is the type.
    actions: Vec<Arc<ActionType>>,
    /// The views that show a resource of the type, each with the action that opens it through
    /// that view, when that is not the resource's own `open` ([`views_of`]).
    views: Vec<(ResourceViewSpec, Option<Arc<ActionType>>)>,
    /// What its resources are, as people name them: its noun, its app's name, and whether
    /// it is a root (each resource published with it says so, [`ResourceType`]).
    named: ResourceType,
    /// What one of its resources is called while its title is empty ([`untitled_of`]).
    untitled: String,
}

impl ActionTypes {
    /// What a record of the type `schema` is called when no text in it names it: the words its
    /// type declares, else a package's root (one per workspace) by the package's title, another
    /// by its type. Never by its id.
    fn untitled(&self, schema: &crate::schema::TypeSchema) -> String {
        self.untitled_of(&schema.name)
    }

    /// What a record of the type `type_name` is called while its title is empty.
    fn untitled_of(&self, type_name: &str) -> String {
        self.on_type.get(type_name).map_or_else(
            || crate::schema::spoken(type_name),
            |of_type| of_type.untitled.clone(),
        )
    }

    /// The action types of the package `meta`, whose actions `planner` runs, described in the
    /// reader's language through `words`.
    #[must_use]
    pub fn new(meta: &Meta, planner: &Arc<dyn Planner>, words: &dyn Words) -> Self {
        let mut on_type = std::collections::BTreeMap::new();
        for declared in &meta.types {
            let kind = meta.kind_of(&declared.name);
            let actions = meta
                .offered_actions()
                .filter(|action| {
                    meta.subject_of(&action.params)
                        .is_some_and(|subject| subject.name == declared.name)
                })
                .map(|action| action_type(meta, &kind, action, planner, words))
                .collect();
            on_type.insert(
                declared.name.clone(),
                TypeActions {
                    edits: edit_types(meta, &declared.name, planner),
                    actions,
                    views: views_of(meta, &declared.name, words),
                    named: ResourceType {
                        // What one and several of the type are called, by count: the catalogue
                        // refines a noun English cannot derive ("notebooks", not "noteses").
                        noun: Noun::new(
                            said_of(words, &declared.id, "noun", Some("1"), &declared.noun()),
                            said_of(words, &declared.id, "noun", Some("2"), &declared.plural()),
                        ),
                        app: Noun::new(
                            meta.title.clone(),
                            tilolang::meta::plural(&meta.title.to_lowercase()),
                        ),
                        // A root is the app's one resource of its type.
                        root: meta.roots.contains(&declared.id),
                    },
                    untitled: untitled_of(meta, declared, words),
                },
            );
        }
        Self {
            meta: meta.clone(),
            planner: Arc::clone(planner),
            words: words.revision(),
            on_type,
        }
    }

    /// The package these types are of.
    #[must_use]
    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    /// Whether these are the types of the package as `meta` describes it now, run by `planner`,
    /// described in the words that read at `words`.
    fn describe(&self, meta: &Meta, planner: &Arc<dyn Planner>, words: u64) -> bool {
        self.meta.app == meta.app
            && self.meta.module_hash == meta.module_hash
            && Arc::ptr_eq(&self.planner, planner)
            && self.words == words
    }

    /// The record `id` of the type `type_name`, labelled `label`, carrying its `open`, the
    /// direct edits of its type ([`edit_types`]) and the actions on its type. A root and an
    /// item are published alike: an item's edits are the same typed field edits, spliced into
    /// the record by the item's id there ([`edit_action`]).
    ///
    /// A `root` record's `open` also takes the item under it to show, which a window that shows
    /// none of the record's items does not publish: a search hit on such an item opens it there.
    fn resource(&self, type_name: &str, id: &str, label: String, root: bool) -> ResourceSpec {
        let open = if root {
            root_open_spec(id)
        } else {
            open_spec(id)
        };
        let mut resource = ResourceSpec::new(
            resource_id(&self.meta.app, id),
            self.meta.kind_of(type_name),
            label,
        )
        .with_action(open);
        let Some(of_type) = self.on_type.get(type_name) else {
            return resource;
        };
        resource = resource.with_type(of_type.named.clone());
        for (view, opens) in &of_type.views {
            if let Some(opens) = opens {
                resource = resource.with_action(open_with_spec(opens, id, &view.id));
            }
            resource = resource.with_view(view.clone());
        }
        let subject = Target::new(id.to_owned());
        for ty in of_type.edits.iter().chain(&of_type.actions) {
            resource = resource.with_action(ty.bind(subject.clone()));
        }
        resource
    }
}

/// The views a resource of `type_name` is shown through: the views a declaration attaches to its
/// type ([`Meta::views_showing`]), each taking the resource and nothing else, so opening one needs
/// only the resource (`open(r, with: v)`).
///
/// A view and the action that opens it name each other one to one: the door projects a view
/// onto its open action's descriptor, and a caller opens the view by invoking that action. The
/// type's default view is the primary one and opens through the resource's own `open`, which
/// shows that view; every other view opens through an action of its own (`open-<view id>`, an
/// [`OpenEffect::OpenWith`](crate::effects::OpenEffect::OpenWith) bound to the resource and the
/// view), answered for each resource by [`open_with_spec`].
fn views_of(
    meta: &Meta,
    type_name: &str,
    words: &dyn Words,
) -> Vec<(ResourceViewSpec, Option<Arc<ActionType>>)> {
    let Some(declared) = meta.types.iter().find(|t| t.name == type_name) else {
        return Vec::new();
    };
    let primary = meta.default_view(&declared.id);
    meta.views_showing(type_name)
        .into_iter()
        .map(|view| {
            let name = if view.name.is_empty() {
                view.id.clone()
            } else {
                view.name.clone()
            };
            let label = label_of(words, &view.id, &name);
            let description = view
                .doc
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(|line| said_of(words, &view.id, "doc", None, line));
            let is_primary = primary == Some(view.id.as_str());
            let opens = (!is_primary)
                .then(|| open_with_type(&view.id, &name, &label, description.as_deref()));
            let open_action = opens.as_ref().map_or("open", |opens| opens.id.as_str());
            let mut spec = ResourceViewSpec::new(view.id.clone(), label, open_action);
            if let Some(line) = description {
                spec = spec.with_description(line);
            }
            (spec.with_primary(is_primary), opens)
        })
        .collect()
}

/// The id of the action that opens a resource through the view named `view`: published by its
/// name, as an action is ([`action_type`]).
#[must_use]
pub fn open_with_id(view: &str) -> String {
    format!("open-{view}")
}

/// The action type that opens a resource through the view `view`, labelled as the view is: the
/// [`OpenEffect::OpenWith`](crate::effects::OpenEffect::OpenWith) effect of the view `view` (an
/// id) under its name `name`, so each view's open action is its alone. Built once per type and
/// view; each resource binds it ([`open_with_spec`]).
fn open_with_type(
    view: &str,
    name: &str,
    label: &str,
    description: Option<&str>,
) -> Arc<ActionType> {
    use tilo_resource::Effect as _;
    let mut spec = crate::effects::OpenEffect::OpenWith {
        anchor: String::new(),
        view: view.to_owned(),
    }
    .bound();
    let ty = spec.type_mut();
    // The payload names the effect by its own id, so the action's id is free to be the view's.
    ty.id = open_with_id(name);
    label.clone_into(&mut ty.label);
    ty.description = description.map(str::to_owned);
    Arc::clone(spec.action_type())
}

/// The action that opens the published resource `anchor` through the view `view`, of the type
/// [`open_with_type`] built for that view: bound as [`tilo_resource::Effect::bound`] binds an
/// effect to the resource it is offered on.
fn open_with_spec(ty: &Arc<ActionType>, anchor: &str, view: &str) -> ActionSpec {
    use tilo_resource::Effect as _;
    let arguments = crate::effects::OpenEffect::OpenWith {
        anchor: anchor.to_owned(),
        view: view.to_owned(),
    }
    .arguments();
    ty.bind(Target::new(arguments.clone()))
        .with_bound(arguments.into_iter().collect())
}

/// The resource `id` of the type `type_name`, published with the actions and direct edits of
/// that type.
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

/// The `open` of the root record `anchor`: it opens the record, or the item under it a caller
/// names (`anchor`, as a search hit on the item does). A person choosing it asks for nothing: left
/// out, the item is the record itself.
fn root_open_spec(anchor: &str) -> ActionSpec {
    use tilo_resource::Effect as _;
    let open = crate::effects::OpenEffect::Open {
        anchor: anchor.to_owned(),
    };
    let params = crate::effects::OpenEffect::declaration(open.id())
        .map(|declared| {
            declared
                .params
                .into_iter()
                .map(|mut param| {
                    param.required = false;
                    param
                })
                .collect()
        })
        .unwrap_or_default();
    open.bound_asking_params(params).with_immediate_per_target()
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

/// The item `item` at `path`, published with its type's direct edits and actions, and its own
/// items.
fn item_resource(types: &ActionTypes, item: &crate::record::DynRecord, path: &str) -> ResourceSpec {
    let type_name = &item.schema().name;
    let label = item_label(item).unwrap_or_else(|| types.untitled_of(type_name));
    let mut resource = types.resource(type_name, path, label, false);
    for child in item_resources(types, item, path) {
        resource = resource.with_child(child);
    }
    resource
}

/// What a workspace publishes of its records, kept up to date by republishing only what changed.
///
/// A record is republished in its cached tree where a change reached it, at that resource's
/// depth: an item whose own field changed has its label read again (and so do the items holding
/// it); items added to, removed from or moved in a collection are added, removed or moved among
/// the children of the resource holding it; an item replaced as a whole is built anew.
/// Only a record that appeared, or one that changed as a whole, is read and built in full, and
/// that happens on a thread of its own, from a snapshot of the store, as search's first index
/// does ([`crate::search::SearchIndex`]): until it ends, updates change nothing.
/// A field of a published resource: the resource's id, and the field the change named.
type Field = (String, String);

/// What the updates since a revision changed, as a reader of an action's availability needs it.
pub enum Moved {
    /// These fields changed, by published resource id: a record's own, and every item's under
    /// it.
    These(std::collections::BTreeMap<String, std::collections::BTreeSet<String>>),
    /// Every resource: an update since read them all again, and what it changed is every one.
    Everything,
}

impl Moved {
    /// Whether what changed moves what a read of `read` saw, found by the resources read rather
    /// than by comparing every change with every read.
    ///
    /// The same resource and field, either read or changed whole, is the same thing read and
    /// changed again. A requirement that read a resource whole sees everything published under
    /// it, so a change to one of its items reads it again. A change to a resource above what was
    /// read does not, unless it changed whole: what an item's own fields are is what a
    /// requirement of the item reads, and a collection changing around it is a change to the
    /// collection.
    #[must_use]
    pub fn reaches(&self, (resource, field): &Field) -> bool {
        const WHOLE: &str = tilolang_runtime::ANY_FIELD;
        let changed = match self {
            Self::Everything => return true,
            Self::These(changed) => changed,
        };
        if changed.get(resource).is_some_and(|fields| {
            fields.contains(field) || fields.contains(WHOLE) || field == WHOLE
        }) {
            return true;
        }
        let under = format!("{resource}/");
        if changed
            .range(under.clone()..)
            .next()
            .is_some_and(|(inner, _)| inner.starts_with(&under))
        {
            return true;
        }
        let mut at = resource.as_str();
        while let Some((outer, _)) = at.rsplit_once('/') {
            if changed
                .get(outer)
                .is_some_and(|fields| fields.contains(WHOLE))
            {
                return true;
            }
            at = outer;
        }
        false
    }
}

/// What the updates since a revision moved of the records' text, as a reader that keeps one
/// entry per resource (the search index) reads it: a task checked replaces its entry, a task
/// added or deleted adds or drops one, whatever else the workspace holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextMoves {
    /// The resources whose text may have moved and those that are gone, by path (`doc` or
    /// `doc/item`).
    These {
        texts: std::collections::BTreeSet<String>,
        gone: std::collections::BTreeSet<String>,
    },
    /// Every resource: an update since read them all again.
    Everything,
}

/// The text a store change may have moved, by path.
fn text_moves(changes: &crate::store::Changes) -> TextMoves {
    let mut texts = std::collections::BTreeSet::new();
    let mut gone = std::collections::BTreeSet::new();
    for keys in changes.docs.values() {
        for key in keys {
            // A collection whose steps the store could not say may have gained or lost items
            // nothing here names: read every resource's entry again.
            if changes.unsaid(key) {
                return TextMoves::Everything;
            }
            texts.insert(key.resource.clone());
        }
    }
    texts.extend(changes.whole.iter().cloned());
    for change in &changes.listings {
        for splice in &change.splices {
            let item = match splice {
                tilolang_runtime::store::Splice::Insert { item, .. } => item,
                tilolang_runtime::store::Splice::Remove { item, .. } => {
                    gone.insert(crate::index::derive::item_path(&change.key.resource, item));
                    continue;
                }
            };
            texts.insert(crate::index::derive::item_path(&change.key.resource, item));
        }
    }
    TextMoves::These { texts, gone }
}

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
    /// The words the trees above were published in ([`Words::revision`]): when they read in
    /// another language, every record is published again in it.
    words: u64,
    /// The packages loaded when the trees were published: a record whose type none of them
    /// declares any more, or one a package that loaded since declares, is found by publishing
    /// every record again.
    packages: Vec<tilolang_runtime::PackageId>,
    /// Whether nothing is published yet: the first update publishes every record.
    fresh: bool,
    /// Whether records are published whole up front, without a window bounding them: what a
    /// host without frames publishes ([`Self::settle`]), and what a caller that reads the whole
    /// workspace (the session's service, the directory) is served. A window's surface turns it
    /// off ([`Self::window`]) and publishes what it reaches instead ([`Self::reached`]).
    whole: bool,
    /// How many records the updates have looked at, one per record per turn.
    visited: u64,
    /// How many times the updates read every record in full: only the first update, and a turn
    /// that changed what every record is published as.
    sweeps: u64,
    /// How many updates saw a change in the workspace, whether or not it changed what a record
    /// is published as: an action's availability follows what its app's requirements read, which
    /// a change may move without moving a label.
    moved: u64,
    /// The fields each recent update saw change, by [`Self::moved`], oldest first, and `None`
    /// for an update that read everything again: what a reader holding an earlier count reads
    /// the requirements of again.
    motions: std::collections::VecDeque<(u64, Option<Vec<Field>>)>,
    /// What each recent update moved of the records' text, by [`Self::moved`], oldest first:
    /// what a reader that keeps one entry per resource (the search index) replaces, and what it
    /// drops, instead of reading every record again.
    text_moves: std::collections::VecDeque<(u64, TextMoves)>,
    /// The published records that are what a window reaches of them, each with only the
    /// resources it reaches ([`Self::reached`]): every other record in [`Self::trees`] is whole.
    reached: std::collections::BTreeSet<String>,
    /// What the window showed when the reached trees were read, and the snapshot they were read
    /// from: a frame that shows the same reads nothing.
    shown: Option<(Arc<crate::store::Snapshot>, Vec<String>)>,
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

/// What a window reaches of a record, by the resource holding it: the resources under it the
/// window shows, each by its path. A read without one reaches every item of every record.
type Reached = std::collections::BTreeMap<String, std::collections::BTreeSet<String>>;

/// What `shown` reaches, by the record it is in: each record with the paths of the resources
/// under it that `shown` names, and the path each of them is reached by from the record down.
fn reached_by_doc(app: &str, shown: &[String]) -> std::collections::BTreeMap<String, Reached> {
    let prefix = resource_id(app, "");
    let mut out: std::collections::BTreeMap<String, Reached> = std::collections::BTreeMap::new();
    for id in shown {
        let Some(path) = id.strip_prefix(prefix.as_str()) else {
            continue;
        };
        let doc = path.split('/').next().unwrap_or(path);
        let reached = out.entry(doc.to_owned()).or_default();
        // Every step from the record down to what is shown: the route the window reaches it by.
        let mut at = path;
        while let Some((parent, _)) = at.rsplit_once('/') {
            reached
                .entry(parent.to_owned())
                .or_default()
                .insert(at.to_owned());
            at = parent;
        }
    }
    out
}

/// What `tree`, the published `doc` of a record, reaches of it: every resource in it, each under
/// the one that holds it — the reach a change to the whole record is read again with.
fn reach_of(tree: &ResourceSpec, doc: &str) -> Reached {
    // A resource's path is its published id without the prefix [`resource_id`] puts the record
    // under.
    let width = tree.id.as_str().len().saturating_sub(doc.len());
    let mut out = Reached::new();
    collect_reached(tree, width, &mut out);
    out
}

/// Each resource under `spec`, by its path, inserted under the path of the resource holding it.
fn collect_reached(spec: &ResourceSpec, width: usize, out: &mut Reached) {
    let of = |spec: &ResourceSpec| spec.id.as_str().get(width..).unwrap_or_default().to_owned();
    for child in &spec.children {
        out.entry(of(spec)).or_default().insert(of(child));
        collect_reached(child, width, out);
    }
}

impl Whole {
    fn build(&self, snapshot: &crate::store::Snapshot, reached: Option<&Reached>) -> Built {
        // This record is published by the package version the snapshot's store was taken from.
        let _schemas = snapshot.scoped();
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
            for child in indexed_items(&self.types, &view, snapshot, &self.doc, schema, reached) {
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
    reached: Option<&Reached>,
) -> Vec<ResourceSpec> {
    // What a window reaches: the items it shows, read by their paths, not the ten thousand the
    // record holds.
    if let Some(reached) = reached {
        return reached
            .get(parent)
            .into_iter()
            .flatten()
            .filter_map(|path| indexed_item(types, view, reader, path, None, Some(reached)))
            .collect();
    }
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
            if let Some(item) =
                indexed_item(types, view, reader, &path, field.record.as_deref(), None)
            {
                out.push(item);
            }
        }
    }
    out
}

/// The item at `path`, as the index holds it, published with its type's direct edits and
/// actions, and its own items; `item_type` is its collection's item type, for an item the index
/// holds no type of. Its label is its first text as the store's reader reads it (`reader`,
/// [`crate::reading`]).
fn indexed_item(
    types: &ActionTypes,
    view: &crate::index::view::View<'_>,
    reader: &crate::store::Snapshot,
    path: &str,
    item_type: Option<&str>,
    reached: Option<&Reached>,
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
        .unwrap_or_else(|| types.untitled(schema));
    let mut resource = types.resource(&schema.name, path, label, false);
    for child in indexed_items(types, view, reader, path, schema, reached) {
        resource = resource.with_child(child);
    }
    Some(resource)
}

impl Publications {
    /// Nothing published yet; the first [`Self::update`] publishes every record whole: what a
    /// host without frames publishes, and what the session's own service and the directory read.
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
            words: host.runtime().words().revision(),
            packages: host.package_ids(),
            fresh: true,
            whole: true,
            visited: 0,
            sweeps: 0,
            moved: 0,
            motions: std::collections::VecDeque::new(),
            text_moves: std::collections::VecDeque::new(),
            reached: std::collections::BTreeSet::new(),
            shown: None,
        }
    }

    /// Publish what a window reaches, not the whole workspace: a window's surface holds the
    /// rows on screen ([`Self::reached`]) and the resource an invocation names
    /// ([`Self::addressed`]), and nothing else until it reaches it. A workspace of ten thousand
    /// costs an open what it shows.
    ///
    /// A caller that reads the whole workspace (the session's own service, the directory) keeps
    /// publishing every record ([`Self::everything`]).
    pub fn window(&mut self) {
        self.whole = false;
    }

    /// The action types of the package `app`, built once per package and planner, described in
    /// the words the host's words resolve at now.
    fn types_of(
        &mut self,
        host: &crate::host::Host,
        app: &str,
        planner: &Arc<dyn Planner>,
    ) -> Option<Arc<ActionTypes>> {
        let words = host.runtime().words();
        if let Some(types) = self.types.get(app) {
            let describes = host.find_in_packages(|package| {
                (package.meta.app == app)
                    .then(|| types.describe(&package.meta, planner, words.revision()))
            });
            if describes == Some(true) {
                return Some(Arc::clone(types));
            }
        }
        let types = host.find_in_packages(|package| {
            (package.meta.app == app)
                .then(|| Arc::new(ActionTypes::new(&package.meta, planner, &*words)))
        })?;
        self.types.insert(app.to_owned(), Arc::clone(&types));
        Some(types)
    }

    /// The app, the name of the type and its id of the record `doc`, as the loaded packages
    /// declare them; `None` when the store holds no such resource or no package declares its
    /// type.
    fn declared(host: &crate::host::Host, doc: &str) -> Option<(String, String, String)> {
        let (app, type_id) = host.store().resource_of(doc)?;
        host.find_in_packages(|package| {
            if package.meta.app != app {
                return None;
            }
            package
                .meta
                .types
                .iter()
                .find(|t| t.id == type_id)
                .map(|t| (app.clone(), t.name.clone(), type_id.clone()))
        })
    }

    /// The action types and schema of the record `doc`, as the loaded packages declare it;
    /// `None` when none declares its type or the type is not registered.
    fn record_types(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
        doc: &str,
    ) -> Option<(Arc<ActionTypes>, &'static crate::schema::TypeSchema)> {
        let (app, _, type_id) = Self::declared(host, doc)?;
        let schema = crate::schema::lookup(&type_id)?;
        Some((self.types_of(host, &app, planner)?, schema))
    }

    /// The record `doc` to build whole, as the host has it now: `None` when the store holds no
    /// such resource, no loaded package declares its type, or the type is not registered.
    fn whole_of(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
        doc: &str,
    ) -> Option<Whole> {
        let (app, type_name, type_id) = Self::declared(host, doc)?;
        crate::schema::lookup(&type_id)?;
        Some(Whole {
            doc: doc.to_owned(),
            types: self.types_of(host, &app, planner)?,
            type_name,
            type_id,
        })
    }

    /// The records among `docs` to build whole, as the host has them now.
    fn wholes(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
        docs: &[String],
    ) -> Vec<Whole> {
        docs.iter()
            .filter_map(|doc| self.whole_of(host, planner, doc))
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

    /// Every record of the workspace, published whole: what a caller that asks about the whole
    /// workspace (the directory) reads. Nothing of it is what a window shows: a window's surface
    /// holds what it reaches ([`Self::reached`]), and the whole workspace is never published to
    /// it.
    pub fn everything(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
    ) -> Vec<Rc<ResourceSpec>> {
        let snapshot = host.store().snapshot();
        let docs: Vec<String> = host
            .records()
            .iter()
            .map(|(_, _, doc)| doc.clone())
            .collect();
        docs.iter()
            .filter_map(|doc| self.whole_of(host, planner, doc))
            .map(|whole| Rc::new(whole.build(&snapshot, None).tree))
            .collect()
    }

    /// What the window's surface publishes: the records `shown` reaches, each with the
    /// resources under it `shown` names, read from the store as it is now. A record the window
    /// shows nothing of is not published: a workspace of ten thousand publishes the rows on
    /// screen.
    ///
    /// The answer stands while the store holds what it was read from and the window shows what
    /// it showed: a frame that changed neither publishes without reading anything.
    pub fn reached(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
        app: &str,
        shown: &[String],
    ) -> Vec<Rc<ResourceSpec>> {
        let snapshot = host.store().snapshot();
        let stands = self
            .shown
            .as_ref()
            .is_some_and(|(at, ids)| Arc::ptr_eq(at, &snapshot) && ids.as_slice() == shown);
        let reach = reached_by_doc(app, shown);
        if !stands {
            // What the window no longer reaches is no longer published: a window scrolled away
            // from a record holds no part of it.
            let held: std::collections::BTreeSet<&str> = reach.keys().map(String::as_str).collect();
            let dropped: Vec<String> = self
                .reached
                .iter()
                .filter(|doc| !held.contains(doc.as_str()))
                .cloned()
                .collect();
            for doc in dropped {
                self.unpublish(&doc);
            }
            for (doc, this) in &reach {
                let Some(whole) = self.whole_of(host, planner, doc) else {
                    continue;
                };
                let built = whole.build(&snapshot, Some(this));
                self.trees.insert(doc.clone(), Rc::new(built.tree));
                self.named_by.insert(doc.clone(), built.named_by);
                self.reached.insert(doc.clone());
                self.visited = self.visited.saturating_add(1);
            }
            // The revision counts what a reader of the whole workspace saw: a reached tree is
            // read for the frame that reached it, so what moved here is the window's, which the
            // surface holds by the ids it shows.
            self.shown = Some((snapshot, shown.to_vec()));
        }
        reach
            .keys()
            .filter_map(|doc| self.trees.get(doc).cloned())
            .collect()
    }

    /// The record of the resource `address` names, published down to it: what an invocation is
    /// resolved against when the window shows nothing of it. `None` when the address is no
    /// resource of `app`.
    pub fn addressed(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
        app: &str,
        address: &str,
    ) -> Option<Rc<ResourceSpec>> {
        let reached = reached_by_doc(app, &[address.to_owned()]);
        let (doc, reached) = reached.into_iter().next()?;
        let whole = self.whole_of(host, planner, &doc)?;
        let snapshot = host.store().snapshot();
        Some(Rc::new(whole.build(&snapshot, Some(&reached)).tree))
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
                    .map(|whole| whole.build(&snapshot, None))
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
            self.reached.remove(&doc);
            touched.push(doc);
            self.revision = self.revision.wrapping_add(1);
            self.structural = self.revision;
            self.visited = self.visited.saturating_add(1);
        }
    }

    /// Publish nothing of `doc` any more: a record the window scrolled away from holds no part
    /// of it, and one the workspace no longer has is gone.
    fn unpublish(&mut self, doc: &str) {
        self.reached.remove(doc);
        self.named_by.remove(doc);
        if self.trees.remove(doc).is_some() {
            self.revision = self.revision.wrapping_add(1);
            self.structural = self.revision;
        }
    }

    /// Publish what the window reached of `doc` again: the same resources, read from the store as
    /// it is now, after a change to the whole record. Whether `doc` was published as reached.
    fn reread(&mut self, host: &crate::host::Host, planner: &Arc<dyn Planner>, doc: &str) -> bool {
        let reach = self.trees.get(doc).map(|at| reach_of(at, doc));
        let (Some(reach), Some(whole)) = (reach, self.whole_of(host, planner, doc)) else {
            return false;
        };
        let snapshot = host.store().snapshot();
        let built = whole.build(&snapshot, Some(&reach));
        self.trees.insert(doc.to_owned(), Rc::new(built.tree));
        self.named_by.insert(doc.to_owned(), built.named_by);
        self.visited = self.visited.saturating_add(1);
        self.revision = self.revision.wrapping_add(1);
        self.structural = self.revision;
        true
    }

    /// How many records the updates so far have looked at, one per record per turn: what
    /// publishing a change costs, whatever the workspace holds.
    #[must_use]
    pub fn visited(&self) -> u64 {
        self.visited
    }

    /// How many updates have seen a change in the workspace, whether or not it changed what a
    /// record is published as: what an action's availability, and only it, reads besides.
    #[must_use]
    pub fn moved(&self) -> u64 {
        self.moved
    }

    /// The fields that changed since the update counted `since`, each once; everything when an
    /// update since read it all again, and `None` when what changed that long ago is no longer
    /// kept, which reads everything too.
    ///
    /// A field is a record's own or one of its items': what a requirement of an item reads is
    /// named as the item's fields, so a change to one item reads that item's requirements again
    /// and leaves every other item's as they were.
    #[must_use]
    pub fn changed_since(&self, since: u64) -> Option<Moved> {
        if since == self.moved {
            return Some(Moved::These(std::collections::BTreeMap::new()));
        }
        if since > self.moved {
            return None;
        }
        let first = self.motions.iter().position(|(at, _)| *at > since)?;
        // Every update after `since` is one the journal holds.
        if self.motions.get(first)?.0 != since.wrapping_add(1) {
            return None;
        }
        if self
            .motions
            .iter()
            .skip(first)
            .any(|(_, moved)| moved.is_none())
        {
            return Some(Moved::Everything);
        }
        let mut out: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
            std::collections::BTreeMap::new();
        for (resource, field) in self
            .motions
            .iter()
            .skip(first)
            .flat_map(|(_, moved)| moved.iter().flatten())
        {
            out.entry(resource.clone())
                .or_default()
                .insert(field.clone());
        }
        Some(Moved::These(out))
    }

    /// What the updates since `since` moved of the records' text, each once: the entries a
    /// reader that keeps one per resource reads again, and those it drops. `None` when what
    /// moved that long ago is no longer kept, which reads everything too.
    #[must_use]
    pub fn text_since(&self, since: u64) -> Option<TextMoves> {
        if since == self.moved {
            return Some(TextMoves::These {
                texts: std::collections::BTreeSet::new(),
                gone: std::collections::BTreeSet::new(),
            });
        }
        if since > self.moved {
            return None;
        }
        let first = self.text_moves.iter().position(|(at, _)| *at > since)?;
        if self.text_moves.get(first)?.0 != since.wrapping_add(1) {
            return None;
        }
        let mut texts = std::collections::BTreeSet::new();
        let mut gone = std::collections::BTreeSet::new();
        for (_, moved) in self.text_moves.iter().skip(first) {
            match moved {
                TextMoves::Everything => return Some(TextMoves::Everything),
                TextMoves::These {
                    texts: these_texts,
                    gone: these_gone,
                } => {
                    texts.extend(these_texts.iter().cloned());
                    gone.extend(these_gone.iter().cloned());
                }
            }
        }
        gone.retain(|path| !texts.contains(path));
        Some(TextMoves::These { texts, gone })
    }

    /// How many times the updates so far have read every record in full: what publishing a
    /// change must never cost, whatever the workspace holds.
    #[must_use]
    pub fn sweeps(&self) -> u64 {
        self.sweeps
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
        // The words a package is published in read in another language now (the person chose
        // one, or a catalogue of it was added): every record is read again, so its labels,
        // descriptions and nouns say what they are now called.
        let reworded = {
            let words = host.runtime().words().revision();
            std::mem::replace(&mut self.words, words) != words
        };
        if reworded {
            self.types.clear();
        }
        // A package loaded or replaced: the records of a type none declares any more go, and
        // those of a type one declares now are published.
        let reloaded = std::mem::replace(&mut self.packages, host.package_ids()) != self.packages;
        let all = std::mem::take(&mut self.fresh) || reworded || reloaded || changes.everything;
        if all {
            if self.whole {
                // A caller that reads the whole workspace — a host without frames, the
                // session's own service, the directory — publishes every record, read off the
                // frame. No window bounds it, so nothing of it goes unpublished.
                self.publish_everything(host, planner, relabels, &mut touched);
            } else {
                // Nothing is published up front: what a window reaches is published when it
                // reaches it ([`Self::reached`]), and a caller that reads the whole workspace
                // asks for it ([`Self::everything`]). A workspace of ten thousand costs an open
                // what it shows.
                *relabels = None;
                self.forget(&mut touched);
            }
        } else {
            // Nothing changed: no record is looked at, whether the workspace holds one or ten
            // thousand.
            self.publish_changed(host, planner, &changes, relabels, &mut touched);
        }
        // A change may move what a requirement of an action reads without moving any tree of
        // its own (a task checked, and `Clear done` can run now): a surface holding the actions
        // reads the requirements of the records that changed again, whether or not the record
        // is published as something else.
        if all || !changes.is_empty() {
            self.moved = self.moved.saturating_add(1);
            // Every field a change named, not its record alone: what an item's requirement
            // reads is the item's fields, and a record of ten thousand items reads one item's
            // again.
            let moved = if all || changes.everything {
                None
            } else {
                let mut fields: std::collections::BTreeSet<Field> = changes
                    .docs
                    .values()
                    .flatten()
                    .map(|key| (key.resource.clone(), key.field.clone()))
                    .collect();
                // A document that may have changed anywhere is its record's own resource, and
                // everything under it.
                fields.extend(
                    changes
                        .whole
                        .iter()
                        .map(|doc| (doc.clone(), tilolang_runtime::store::ANY_FIELD.to_owned())),
                );
                Some(fields.into_iter().collect())
            };
            if self.motions.len() >= MOTIONS_KEPT {
                self.motions.pop_front();
            }
            self.motions.push_back((self.moved, moved));
            if self.text_moves.len() >= MOTIONS_KEPT {
                self.text_moves.pop_front();
            }
            let moved_text = if all || changes.everything {
                TextMoves::Everything
            } else {
                text_moves(&changes)
            };
            self.text_moves.push_back((self.moved, moved_text));
        }
        touched
    }

    /// Publish the records the turn changed, and no others: what the store's watch says moved,
    /// answered from its change set, whatever else the workspace holds.
    fn publish_changed(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
        changes: &crate::store::Changes,
        relabels: &mut Option<Vec<(String, String)>>,
        touched: &mut Vec<String>,
    ) {
        use std::collections::BTreeSet;

        use tilolang_runtime::store::ANY_FIELD;

        let mut whole_docs = Vec::new();
        let mut docs: BTreeSet<&str> = changes.docs.keys().map(String::as_str).collect();
        docs.extend(changes.whole.iter().map(String::as_str));
        self.visited = self.visited.saturating_add(docs.len() as u64);
        for doc in docs {
            if !host.store().contains(doc) {
                // The resource is gone: so is everything published of it.
                if self.trees.contains_key(doc) {
                    *relabels = None;
                    touched.push(doc.to_owned());
                }
                self.unpublish(doc);
                continue;
            }
            // Nothing of it is published: what a window reaches of it is built when it reaches
            // it ([`Self::reached`]), so a change to a record no one published reads no more of
            // it than the surface of what the window shows, which is published again. A
            // publication of the whole workspace builds it whole, as it builds every record.
            if !self.trees.contains_key(doc) && !self.whole {
                touched.push(doc.to_owned());
                *relabels = None;
                continue;
            }
            // The versions of a resource name no value a record is published as: marks and
            // history read them, and what the tree shows stays as it was.
            let fields: Option<BTreeSet<_>> = changes.docs.get(doc).map(|fields| {
                fields
                    .iter()
                    .filter(|key| key.field != tilolang_runtime::DOC_VERSIONS)
                    .cloned()
                    .collect()
            });
            let whole = !self.trees.contains_key(doc)
                || changes.whole.contains(doc)
                || fields.as_ref().is_some_and(|fields| {
                    fields
                        .iter()
                        .any(|key| key.resource == doc && key.field == ANY_FIELD)
                });
            if whole {
                // A record the window reaches is read again down to what it shows: a change to
                // the whole of it does not publish ten thousand items to a window showing ten.
                if self.reached.contains(doc) {
                    if self.reread(host, planner, doc) {
                        *relabels = None;
                    }
                } else {
                    whole_docs.extend(self.whole_of(host, planner, doc));
                }
                continue;
            }
            let (Some(fields), Some((types, schema))) =
                (fields.as_ref(), self.record_types(host, planner, doc))
            else {
                continue;
            };
            // Nothing the tree shows was named.
            if fields.is_empty() {
                continue;
            }
            let Some(tree) = self.trees.get_mut(doc) else {
                continue;
            };
            let at = Published {
                store: host.store(),
                types: &types,
                schema,
                doc,
                partial: self.reached.contains(doc),
            };
            match at.republish(
                tree,
                (fields, changes),
                self.named_by.entry(doc.to_owned()).or_default(),
            ) {
                Republished::Unchanged => {}
                Republished::Relabeled(labels) => {
                    if let Some(gathered) = relabels.as_mut() {
                        gathered.extend(labels);
                    }
                    touched.push(doc.to_owned());
                }
                Republished::Rebuilt => {
                    *relabels = None;
                    touched.push(doc.to_owned());
                }
            }
        }
        if !whole_docs.is_empty() {
            self.build_off_frame(host, whole_docs);
        }
    }

    /// Publish nothing, forgetting what was published: what a turn that changed what every record
    /// is published as costs (the words, or the packages loaded) — every record's tree is read
    /// again when something reaches it.
    fn forget(&mut self, touched: &mut Vec<String>) {
        self.shown = None;
        self.reached.clear();
        if self.trees.is_empty() {
            return;
        }
        for doc in std::mem::take(&mut self.trees).into_keys() {
            touched.push(doc);
        }
        self.named_by.clear();
        self.revision = self.revision.wrapping_add(1);
        self.structural = self.revision;
    }

    /// Publish every record in full: what a caller that reads the whole workspace asks for (the
    /// directory, a host without frames), read off the frame.
    fn publish_everything(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
        relabels: &mut Option<Vec<(String, String)>>,
        touched: &mut Vec<String>,
    ) {
        use std::collections::BTreeSet;

        let records = host.records();
        let now: BTreeSet<&str> = records.iter().map(|(_, _, doc)| doc.as_str()).collect();
        let gone: Vec<String> = self
            .trees
            .keys()
            .filter(|doc| !now.contains(doc.as_str()))
            .cloned()
            .collect();
        let gone_count = gone.len() as u64;
        for doc in gone {
            self.unpublish(&doc);
            touched.push(doc);
            *relabels = None;
        }
        // What is published whole already stays: a caller that asks for the workspace again
        // reads what it did not read whole before — a record a window reached is read whole.
        let missing: Vec<&String> = records
            .iter()
            .map(|(_, _, doc)| doc)
            .filter(|doc| {
                !self.trees.contains_key(doc.as_str()) || self.reached.contains(doc.as_str())
            })
            .collect();
        // A read of every record in full is a sweep; what is published already is not read
        // again.
        if missing.len() == records.len() {
            self.sweeps = self.sweeps.saturating_add(1);
        }
        let whole_docs: Vec<Whole> = missing
            .into_iter()
            .filter_map(|doc| self.whole_of(host, planner, doc))
            .collect();
        self.visited = self
            .visited
            .saturating_add(whole_docs.len() as u64)
            .saturating_add(gone_count);
        if !whole_docs.is_empty() {
            self.build_off_frame(host, whole_docs);
        }
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
                        .map(|whole| whole.build(&snapshot, None))
                        .collect()
                },
                self.wake.clone(),
            ),
        ));
    }
}

/// How many label-only revisions [`Publications::labels_since`] reaches back over.
const RELABELS_KEPT: usize = 256;

/// How many updates [`Publications::changed_since`] reaches back over.
const MOTIONS_KEPT: usize = 256;

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
    /// Whether the tree is what a window reaches of the record rather than the whole of it
    /// ([`Publications::reached`]).
    partial: bool,
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
        // The field of the record the name is read through, when an item holds it: a field
        // after it in naming order cannot name the record while it does
        // ([`TypeSchema::naming_scan`]).
        let named_chain = named_by
            .filter(|at| *at != self.doc)
            .and_then(|at| self.chain(at));
        let naming_field = named_chain
            .as_ref()
            .and_then(|chain| chain.get(1))
            .and_then(|top| self.holder(top))
            .map(|(_, field)| field);
        let naming_position = naming_field
            .as_deref()
            .and_then(|field| self.schema.naming_position(field));
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
                // Whether the field is read before (or as) the text that names the record.
                // In naming order, not by id: a title is read first whatever id it drew, and
                // its change always names the record again.
                let or_before = match (naming_position, self.schema.naming_position(&key.field)) {
                    (None, _) | (Some(_), None) => true,
                    (Some(naming), Some(changed)) => changed <= naming,
                };
                if or_before {
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
                .map_or_else(|| schema.untitled(), |(text, _)| text);
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
    /// of `nodes`). A tree a window reaches holds what it reached of a collection, not all of
    /// it, so an item it did not hold is added only when the change names it.
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
        if self.partial {
            self.reached_members(node, &at, path, nodes);
            return;
        }
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
                    Some(_) => {
                        nodes.remove(&item);
                        ordered.extend(self.item(&item));
                    }
                    // A whole record's tree holds every item of its collections; one a window
                    // reaches is [`Self::reached_members`]'.
                    None => ordered.extend(self.item(&item)),
                }
            }
        }
        node.children = ordered;
    }

    /// [`Self::membership`] of a tree that holds what a window reached of the record: the items
    /// it holds keep their order, those `nodes` names are read again (and leave once they are
    /// gone), and one it did not hold joins where its collection's listing puts it, found among
    /// the held ones by their places. The collection is not read whole: adding a task to ten
    /// thousand reads where the new one and a few held ones stand.
    fn reached_members(
        &self,
        node: &mut ResourceSpec,
        at: &tilolang_runtime::value::ResourceRef,
        path: &str,
        nodes: &mut std::collections::BTreeSet<String>,
    ) {
        let published = resource_id(&self.types.meta().app, "");
        let item_at = |child: &ResourceSpec| -> Option<String> {
            child
                .id
                .as_str()
                .strip_prefix(published.as_str())
                .map(str::to_owned)
        };
        let place = |item: &str| -> Option<usize> {
            let (_, field) = self.holder(item).filter(|(holder, _)| holder == path)?;
            let (before, _, _) = self.members(at, self.schema_at(path)?, &field)?;
            let id = crate::index::derive::id_of(item)?;
            before.checked_add(self.store.position(at, &field, id)?)
        };
        let mut children = Vec::with_capacity(node.children.len());
        for child in std::mem::take(&mut node.children) {
            match item_at(&child) {
                Some(item) if nodes.remove(&item) => children.extend(self.item(&item)),
                _ => children.push(child),
            }
        }
        let joining: Vec<String> = nodes
            .iter()
            .filter(|item| self.holder(item).is_some_and(|(holder, _)| holder == path))
            .cloned()
            .collect();
        for item in joining {
            nodes.remove(&item);
            let (Some(now), Some(spec)) = (place(&item), self.item(&item)) else {
                continue;
            };
            let at = children.partition_point(|child| {
                item_at(child)
                    .and_then(|held| place(&held))
                    .is_some_and(|held| held < now)
            });
            children.insert(at, spec);
        }
        node.children = children;
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
                .map_or_else(|| schema.untitled(), |(text, _)| text);
            if node.label != label {
                moved.push((node.id.as_str().to_owned(), label.clone()));
                node.label = label;
            }
        }
        moved
    }

    /// Apply the steps of the listing of the collection `field` of the resource at `path` to
    /// its published children: an item inserted is built (and taken out of `nodes`), one removed
    /// dropped, one moved kept. A change that says no step moved changed no item for those who
    /// read the mirror, and the rows the collection gained are the items the change names: one
    /// a person placed was where they read it before the record held it
    /// ([`crate::store::TiloStore::sync_from`]), and it goes where the listing has it. False
    /// when the steps do not fit the children, or the children are then not as many as the
    /// items: they are brought in line with the listing instead.
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
        if splices.is_empty() {
            self.place_named(node, (path, field), &at, before, nodes);
        }
        node.children.len() == before.saturating_add(len).saturating_add(after)
    }

    /// Place in `node`'s children the items `nodes` names that the collection `field` of the
    /// resource at `at` holds, each where its listing has it: what a change names says where
    /// its own rows stand.
    fn place_named(
        &self,
        node: &mut ResourceSpec,
        (path, field): (&str, &str),
        at: &tilolang_runtime::value::ResourceRef,
        before: usize,
        nodes: &mut std::collections::BTreeSet<String>,
    ) {
        let app = &self.types.meta().app;
        let mut named: Vec<(usize, String)> = nodes
            .iter()
            .filter(|item| {
                self.holder(item)
                    .is_some_and(|(holder, held)| holder == path && held == field)
            })
            .filter_map(|item| {
                let id = crate::index::derive::id_of(item)?;
                let place = self.store.position(at, field, id)?;
                Some((before.saturating_add(place), item.clone()))
            })
            .collect();
        // Placed at the index each has in the listing now, from the first on: one placed
        // before another does not move the place the listing gives it.
        named.sort();
        for (index, item) in named {
            let id = resource_id(app, &item);
            if node.children.iter().any(|child| child.id.as_str() == id) {
                // Already there, where it stands: `rebuild` reads the item again in place.
                continue;
            }
            let Some(child) = self.item(&item) else {
                continue;
            };
            let index = index.min(node.children.len());
            node.children.insert(index, child);
            nodes.remove(&item);
        }
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
        let snapshot = self.store.snapshot();
        let _schemas = snapshot.scoped();
        let schema = self.schema_at(path)?;
        let view = snapshot.view(&self.types.meta().app)?;
        indexed_item(self.types, &view, &snapshot, path, Some(&schema.id), None)
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

/// What names an item, as [`crate::reads::first_text`] names a resource: its own texts first, in
/// declaration order, by their first non-blank line, and only a record holding none of its own
/// is named by an item it holds (a record that is nothing but its list). A channel's messages
/// never name the channel: its `name` does.
fn item_label(item: &crate::record::DynRecord) -> Option<String> {
    use crate::record::DynValue;
    use crate::schema::MergeKind;
    let schema = item.schema();
    let read = |field: &crate::schema::FieldSchema| match item.get(&field.id) {
        Some(DynValue::Text(text)) if !text.trim().is_empty() => text
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(str::to_owned),
        _ => None,
    };
    let mut texts = schema
        .fields
        .iter()
        .filter(|field| field.kind == MergeKind::Text);
    if let Some(text) = texts.find_map(read) {
        return Some(text);
    }
    // No text of its own: a record that is nothing but its list is named by its first item.
    for field in &schema.fields {
        match item.get(&field.id) {
            Some(DynValue::Record(record)) => {
                if let Some(found) = item_label(record) {
                    return Some(found);
                }
            }
            Some(DynValue::Collection(items)) => {
                for (_, item) in items.iter() {
                    if let Some(found) = item_label(item) {
                        return Some(found);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// The package's own resource, carrying the actions that take no record.
#[must_use]
pub fn publish_package(meta: &Meta, planner: &Arc<dyn Planner>, words: &dyn Words) -> ResourceSpec {
    let kind = meta.package_kind();
    let mut resource = ResourceSpec::new(package_resource_id(&meta.app), kind.clone(), &meta.title);
    for action in meta.offered_actions() {
        if meta.subject_of(&action.params).is_none() {
            resource = resource
                .with_action(action_type(meta, &kind, action, planner, words).bind(Target::none()));
        }
    }
    resource
}

/// The host's own resource: the commit of an action the runtime planned while a window handled a
/// press. It is not offered in the palette; the window invokes it.
///
/// It also carries the [`HostEffect`] vocabulary an agent uses: run a block, read
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
    // A caller names the field as its type declares it; it is stored under its id.
    let named = field;
    let field = declared
        .fields
        .iter()
        .find(|declared| declared.name == named)
        .map(|declared| declared.id.as_str())
        .ok_or_else(|| format!("the type `{type_name}` has no field `{named}`"))?;
    let schema_field = schema
        .field(field)
        .ok_or_else(|| format!("the type `{type_name}` has no field `{named}`"))?;
    let op = match (schema_field.kind, value) {
        (MergeKind::Text, Json::String(text)) => EditOp::Replace { text: text.clone() },
        (MergeKind::Register, value) => EditOp::Set {
            value: value.clone(),
        },
        // A rich text comes as its words, a paragraph a line, or as the runtime's encoding of
        // it where it carries more than words: the edit is the one the package's own views write.
        (MergeKind::RichText, value) => {
            let rich = match value {
                Json::String(text) => tilolang_runtime::rich::RichValue::from_text(text),
                encoded => match tilolang_runtime::value::from_json(encoded)
                    .map_err(|why| format!("the field `{field}` takes a rich text: {why}"))?
                {
                    tilolang_runtime::value::Value::Rich(rich) => rich,
                    _ => return Err(format!("the field `{field}` takes a rich text")),
                },
            };
            let blocks = rich.blocks.iter().map(tilo_document::ClipBlock::from_view);
            crate::store::rich_edit(blocks.collect(), None, 0).map_err(|error| error.to_string())?
        }
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
/// a bound checkbox. The verbs and their arguments are [`DIRECT_EDITS`]'.
fn edit_types(meta: &Meta, type_name: &str, planner: &Arc<dyn Planner>) -> [Arc<ActionType>; 2] {
    let make = |direct: DirectEdit| {
        let (meta, type_name) = (meta.clone(), type_name.to_owned());
        let planner = Arc::clone(planner);
        let spec = ActionSpec::typed_on(
            direct.id,
            direct.label,
            direct.label,
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required("field", ParamType::Text, "The field's name"),
                ActionParam::required("value", direct.value, "The new value"),
            ],
            move |target: &Target, args: &ActionArgs| {
                let json = args.to_json();
                let field = json.get("field").and_then(Json::as_str).unwrap_or_default();
                let value = json.get("value").cloned().unwrap_or(Json::Null);
                let id = target.get::<String>().map_or("", String::as_str);
                edit_action(&meta, &type_name, id, field, &value)
                    // The edit is the caller's the door admitted: an agent's change is the
                    // agent's, in history and wherever the field shows who changed it.
                    .map(|mut planned| {
                        if let Some(caller) = planner.caller() {
                            planned.authorship.invoker = caller.actor;
                        }
                        planned
                    })
                    .and_then(|planned| planned.encode().map_err(|error| error.to_string()))
                    .unwrap_or_else(|message| EncodedAction {
                        type_id: INVALID_ARGUMENTS_TYPE.to_owned(),
                        payload: message.into_bytes(),
                    })
            },
        )
        // The platform runs it for a bound field, and an agent may ask for it; a person
        // changes the field in the control they see, whose own action they press.
        .for_audience(tilo_actions::Audience::Agents);
        Arc::clone(spec.action_type())
    };
    DIRECT_EDITS.map(make)
}
